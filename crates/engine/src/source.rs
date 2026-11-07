//! Streaming byte source over a remote object.
//!
//! [`RangeSource`] implements blocking [`std::io::Read`] + [`std::io::Seek`] (and therefore
//! Symphonia's `MediaSource`) on top of an async fetcher task that downloads fixed-size chunks
//! ahead of the read position. Reads block on a condition variable; they must never run on a
//! tokio worker thread or on the audio thread.

use std::collections::BTreeMap;
use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use sonorus_storage::{Storage, StorageError};
use symphonia::core::io::MediaSource;
use tokio::sync::Notify;

/// Boxed future used by [`RemoteObject::get_range`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Object that can serve byte ranges. Impl'd for [`StorageObject`]; tests use an in-memory fake.
pub trait RemoteObject: Send + Sync + 'static {
    fn get_range(&self, start: u64, len: u64) -> BoxFuture<'_, sonorus_storage::Result<Bytes>>;
}

/// A bucket handle plus the key of one object in it.
pub struct StorageObject {
    pub storage: Storage,
    pub key: String,
}

impl StorageObject {
    /// Wraps `storage` with SDK retries disabled: the fetcher owns the retry/backoff loop,
    /// and stacking SDK retries under it would delay seeks and error reporting.
    pub fn new(storage: &Storage, key: impl Into<String>) -> Self {
        Self {
            storage: storage.without_sdk_retries(),
            key: key.into(),
        }
    }
}

impl RemoteObject for StorageObject {
    fn get_range(&self, start: u64, len: u64) -> BoxFuture<'_, sonorus_storage::Result<Bytes>> {
        Box::pin(async move { self.storage.get_range(&self.key, start, len).await })
    }
}

/// Tunables for [`RangeSource`].
#[derive(Debug, Clone)]
pub struct SourceConfig {
    /// Chunk size in bytes (1 MiB by default).
    pub chunk_size: u64,
    /// Number of chunks fetched ahead of the current chunk.
    pub chunks_ahead: u64,
    /// Number of chunks kept behind the current chunk.
    pub chunks_behind: u64,
    /// How long a blocking read waits for data before failing with `TimedOut`.
    pub read_timeout: Duration,
    /// Base delay of the exponential retry backoff.
    pub backoff_base: Duration,
    /// Upper bound of the retry backoff.
    pub backoff_cap: Duration,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            chunk_size: 1024 * 1024,
            chunks_ahead: 24,
            chunks_behind: 2,
            read_timeout: Duration::from_secs(45),
            backoff_base: Duration::from_millis(250),
            backoff_cap: Duration::from_secs(8),
        }
    }
}

/// Blocking, seekable reader over a remote object. Dropping it stops the fetcher task.
pub struct RangeSource {
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

/// Cheap, cloneable view of a [`RangeSource`]'s buffering state.
#[derive(Clone)]
pub struct BufferHandle {
    shared: Arc<Shared>,
}

/// Upper bound applied to `read_timeout` so that `Instant + timeout` can never overflow.
const MAX_READ_TIMEOUT: Duration = Duration::from_secs(60 * 60 * 24 * 365);

impl RangeSource {
    /// Spawns the fetcher on `rt`. `size` comes from list/head. Starts fetching chunk 0 immediately.
    pub fn open(
        object: Arc<dyn RemoteObject>,
        size: u64,
        rt: &tokio::runtime::Handle,
        config: SourceConfig,
    ) -> (RangeSource, BufferHandle) {
        let mut config = config;
        // A zero chunk size would divide by zero; treat it as the smallest valid size.
        config.chunk_size = config.chunk_size.max(1);
        let shared = Arc::new(Shared {
            size,
            config,
            state: Mutex::new(State {
                chunks: BTreeMap::new(),
                read_pos: 0,
                failed: None,
                last_error: None,
                closed: false,
            }),
            data_ready: Condvar::new(),
            wake_fetcher: Notify::new(),
            buffering: AtomicBool::new(false),
        });
        let task = rt.spawn(fetch_loop(Arc::clone(&shared), object));
        (
            RangeSource {
                shared: Arc::clone(&shared),
                task,
            },
            BufferHandle { shared },
        )
    }

    /// Total size of the object in bytes.
    pub fn size(&self) -> u64 {
        self.shared.size
    }
}

impl Drop for RangeSource {
    fn drop(&mut self) {
        {
            // Poison is ignored: we only need to flag the fetcher to stop.
            let mut st = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            st.closed = true;
        }
        self.shared.wake_fetcher.notify_one();
        self.task.abort();
    }
}

impl BufferHandle {
    /// True while a read is blocked waiting for data.
    pub fn is_buffering(&self) -> bool {
        self.shared.buffering.load(Ordering::Relaxed)
    }

    /// Contiguous bytes available from the read position forward.
    pub fn buffered_ahead_bytes(&self) -> u64 {
        let Ok(st) = self.shared.state.lock() else {
            return 0;
        };
        let chunk_size = self.shared.config.chunk_size;
        let mut idx = st.read_pos / chunk_size;
        let mut off = st.read_pos - idx.saturating_mul(chunk_size);
        let mut total = 0u64;
        while let Some(chunk) = st.chunks.get(&idx) {
            let len = chunk.len() as u64;
            total = total.saturating_add(len.saturating_sub(off));
            off = 0;
            match idx.checked_add(1) {
                Some(next) => idx = next,
                None => break,
            }
        }
        total
    }

    /// Most recent fetch error; cleared on the next successful fetch.
    pub fn last_error(&self) -> Option<String> {
        let Ok(st) = self.shared.state.lock() else {
            return None;
        };
        st.last_error.clone()
    }

    /// Current read position.
    pub fn read_position(&self) -> u64 {
        let Ok(st) = self.shared.state.lock() else {
            return 0;
        };
        st.read_pos
    }

    /// Total size of the object in bytes.
    pub fn size(&self) -> u64 {
        self.shared.size
    }
}

impl Read for RangeSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let shared = &self.shared;
        let chunk_size = shared.config.chunk_size;
        let timeout = shared.config.read_timeout.min(MAX_READ_TIMEOUT);
        let deadline = Instant::now() + timeout;
        let mut st = shared.state.lock().map_err(|_| poisoned())?;
        loop {
            if st.read_pos >= shared.size {
                shared.buffering.store(false, Ordering::Relaxed);
                return Ok(0);
            }
            let idx = st.read_pos / chunk_size;
            let off = (st.read_pos - idx.saturating_mul(chunk_size)) as usize;
            let copied = st.chunks.get(&idx).map(|chunk| {
                let avail = chunk.get(off..).unwrap_or(&[]);
                let n = buf.len().min(avail.len());
                buf[..n].copy_from_slice(&avail[..n]);
                n
            });
            if let Some(n) = copied {
                st.read_pos = st.read_pos.saturating_add(n as u64);
                shared.buffering.store(false, Ordering::Relaxed);
                if st.read_pos / chunk_size != idx {
                    // The window moved; let the fetcher schedule the next chunks.
                    shared.wake_fetcher.notify_one();
                }
                return Ok(n);
            }
            if let Some((_, msg)) = st.failed.as_ref().filter(|(fi, _)| *fi == idx) {
                return Err(io::Error::other(msg.clone()));
            }
            if st.closed {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "range source closed",
                ));
            }
            shared.buffering.store(true, Ordering::Relaxed);
            shared.wake_fetcher.notify_one();
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("no data for {:?}", shared.config.read_timeout),
                ));
            }
            // Spurious wakeups loop back and re-check state; the deadline is never reset.
            let (next, _) = shared
                .data_ready
                .wait_timeout(st, remaining)
                .map_err(|_| poisoned())?;
            st = next;
        }
    }
}

impl Seek for RangeSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let shared = &self.shared;
        let mut st = shared.state.lock().map_err(|_| poisoned())?;
        let new_pos = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(d) => offset_position(st.read_pos, d)?,
            SeekFrom::End(d) => offset_position(shared.size, d)?,
        };
        st.read_pos = new_pos;
        let idx = new_pos / shared.config.chunk_size;
        if matches!(&st.failed, Some((fi, _)) if *fi != idx) {
            st.failed = None;
        }
        drop(st);
        shared.wake_fetcher.notify_one();
        Ok(new_pos)
    }
}

impl MediaSource for RangeSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.shared.size)
    }
}

/// State shared by the reader, the handles and the fetcher task.
struct Shared {
    size: u64,
    config: SourceConfig,
    state: Mutex<State>,
    /// Reader waits here; the fetcher `notify_all()`s after inserting a chunk or setting `failed`.
    data_ready: Condvar,
    /// Reader/seek/drop call `notify_one()` so a wakeup is never lost.
    wake_fetcher: Notify,
    buffering: AtomicBool,
}

struct State {
    /// Chunk index -> exactly `expected_len(index)` bytes.
    chunks: BTreeMap<u64, Bytes>,
    read_pos: u64,
    /// Non-retryable failure for one chunk index.
    failed: Option<(u64, String)>,
    last_error: Option<String>,
    closed: bool,
}

fn poisoned() -> io::Error {
    io::Error::other("source state poisoned")
}

fn offset_position(base: u64, delta: i64) -> io::Result<u64> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "invalid seek position");
    let base = i64::try_from(base).map_err(|_| invalid())?;
    let target = base.checked_add(delta).ok_or_else(invalid)?;
    u64::try_from(target).map_err(|_| invalid())
}

/// Index of the last chunk, or None when the object is empty.
fn last_chunk(size: u64, chunk_size: u64) -> Option<u64> {
    if size == 0 {
        None
    } else {
        Some((size - 1) / chunk_size)
    }
}

/// Byte length of chunk `index` (the final chunk may be short).
fn expected_len(size: u64, chunk_size: u64, index: u64) -> u64 {
    let start = index.saturating_mul(chunk_size);
    chunk_size.min(size.saturating_sub(start))
}

/// Returns `(cur, window_start, window_end)` for the read position, or None for an empty object.
fn window(config: &SourceConfig, size: u64, read_pos: u64) -> Option<(u64, u64, u64)> {
    let last = last_chunk(size, config.chunk_size)?;
    let cur = (read_pos / config.chunk_size).min(last);
    let lo = cur.saturating_sub(config.chunks_behind);
    let hi = cur.saturating_add(config.chunks_ahead).min(last);
    Some((cur, lo, hi))
}

fn in_window(config: &SourceConfig, size: u64, read_pos: u64, index: u64) -> bool {
    window(config, size, read_pos).is_some_and(|(_, lo, hi)| (lo..=hi).contains(&index))
}

/// Upper bound of the retry delay in milliseconds: `min(cap, base * 2^(attempt-1))`.
fn backoff_ceiling_ms(base: Duration, cap: Duration, attempt: u32) -> u64 {
    let base_ms = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
    let cap_ms = u64::try_from(cap.as_millis()).unwrap_or(u64::MAX);
    base_ms
        .saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)))
        .min(cap_ms)
}

async fn fetch_loop(shared: Arc<Shared>, object: Arc<dyn RemoteObject>) {
    let mut attempt: u32 = 0;
    loop {
        // Step 1: pick the next target under the lock, then release the lock before awaiting.
        let target = {
            let Ok(mut st) = shared.state.lock() else {
                return;
            };
            if st.closed {
                return;
            }
            let Some((cur, lo, hi)) = window(&shared.config, shared.size, st.read_pos) else {
                return;
            };
            st.chunks.retain(|&i, _| (lo..=hi).contains(&i));
            if matches!(&st.failed, Some((fi, _)) if !(lo..=hi).contains(fi)) {
                st.failed = None;
            }
            let failed_idx = st.failed.as_ref().map(|(i, _)| *i);
            (cur..=hi).find(|i| !st.chunks.contains_key(i) && Some(*i) != failed_idx)
        };

        // Step 2: nothing to fetch; sleep until the reader or a seek wakes us.
        let Some(target) = target else {
            shared.wake_fetcher.notified().await;
            continue;
        };

        // Step 3: fetch the target. A wake that leaves the target in the window keeps the
        // same pinned request; a wake that moves it out of the window cancels the request.
        let len = expected_len(shared.size, shared.config.chunk_size, target);
        let offset = target.saturating_mul(shared.config.chunk_size);
        let fut = object.get_range(offset, len);
        tokio::pin!(fut);
        let fetched = loop {
            tokio::select! {
                r = &mut fut => break Some(r),
                _ = shared.wake_fetcher.notified() => {
                    let still_wanted = {
                        let Ok(st) = shared.state.lock() else {
                            return;
                        };
                        if st.closed {
                            return;
                        }
                        in_window(&shared.config, shared.size, st.read_pos, target)
                    };
                    if !still_wanted {
                        break None;
                    }
                }
            }
        };
        let Some(result) = fetched else {
            continue;
        };

        // A success with the wrong length is treated as a retryable short read.
        let outcome = match result {
            Ok(bytes) if bytes.len() as u64 == len => Ok(bytes),
            Ok(bytes) => Err(StorageError::Network(format!(
                "short read: got {} of {} bytes",
                bytes.len(),
                len
            ))),
            Err(e) => Err(e),
        };

        match outcome {
            // Step 4: insert if still in the window, clear the error, reset the backoff.
            Ok(bytes) => {
                {
                    let Ok(mut st) = shared.state.lock() else {
                        return;
                    };
                    if st.closed {
                        return;
                    }
                    if in_window(&shared.config, shared.size, st.read_pos, target) {
                        st.chunks.insert(target, bytes);
                    }
                    st.last_error = None;
                }
                attempt = 0;
                shared.data_ready.notify_all();
            }
            // Step 5: retryable error. Keep existing chunks, back off with full jitter.
            Err(e) if e.is_retryable() => {
                let msg = e.to_string();
                {
                    let Ok(mut st) = shared.state.lock() else {
                        return;
                    };
                    if st.closed {
                        return;
                    }
                    st.last_error = Some(msg.clone());
                }
                attempt = attempt.saturating_add(1);
                log::warn!("chunk {target} fetch failed (attempt {attempt}), retrying: {msg}");
                let ceiling_ms = backoff_ceiling_ms(
                    shared.config.backoff_base,
                    shared.config.backoff_cap,
                    attempt,
                );
                let delay = Duration::from_millis(fastrand::u64(0..=ceiling_ms));
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shared.wake_fetcher.notified() => {}
                }
            }
            // Step 6: non-retryable error. Record it for the reader and wake it.
            Err(e) => {
                let msg = e.to_string();
                {
                    let Ok(mut st) = shared.state.lock() else {
                        return;
                    };
                    if st.closed {
                        return;
                    }
                    st.failed = Some((target, msg.clone()));
                    st.last_error = Some(msg);
                }
                shared.data_ready.notify_all();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// In-memory object. Pops one planned error per call before serving; `never_returns`
    /// makes every call pend forever.
    struct MemObject {
        data: Bytes,
        fail_plan: Mutex<VecDeque<StorageError>>,
        never_returns: bool,
    }

    impl MemObject {
        fn new(data: Vec<u8>) -> Self {
            Self {
                data: Bytes::from(data),
                fail_plan: Mutex::new(VecDeque::new()),
                never_returns: false,
            }
        }

        fn with_failures(data: Vec<u8>, plan: Vec<StorageError>) -> Self {
            Self {
                data: Bytes::from(data),
                fail_plan: Mutex::new(plan.into_iter().collect()),
                never_returns: false,
            }
        }

        fn never_returns() -> Self {
            Self {
                data: Bytes::new(),
                fail_plan: Mutex::new(VecDeque::new()),
                never_returns: true,
            }
        }
    }

    impl RemoteObject for MemObject {
        fn get_range(&self, start: u64, len: u64) -> BoxFuture<'_, sonorus_storage::Result<Bytes>> {
            Box::pin(async move {
                if self.never_returns {
                    return std::future::pending().await;
                }
                let planned = self.fail_plan.lock().unwrap().pop_front();
                if let Some(err) = planned {
                    return Err(err);
                }
                let total = self.data.len() as u64;
                let from = start.min(total) as usize;
                let to = start.saturating_add(len).min(total) as usize;
                Ok(self.data.slice(from..to))
            })
        }
    }

    fn random_bytes(n: usize) -> Vec<u8> {
        std::iter::repeat_with(|| fastrand::u8(..))
            .take(n)
            .collect()
    }

    fn test_config() -> SourceConfig {
        SourceConfig {
            chunk_size: 1024,
            chunks_ahead: 4,
            chunks_behind: 1,
            read_timeout: Duration::from_secs(2),
            backoff_base: Duration::from_millis(1),
            backoff_cap: Duration::from_millis(10),
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    #[test]
    fn sequential_read_matches_data() {
        let rt = runtime();
        let data = random_bytes(10_000);
        let object = Arc::new(MemObject::new(data.clone()));
        let (mut src, _handle) =
            RangeSource::open(object, data.len() as u64, rt.handle(), test_config());
        let mut out = Vec::new();
        src.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn seek_start_then_read_matches_tail() {
        let rt = runtime();
        let data = random_bytes(10_000);
        let object = Arc::new(MemObject::new(data.clone()));
        let (mut src, _handle) =
            RangeSource::open(object, data.len() as u64, rt.handle(), test_config());
        assert_eq!(src.seek(SeekFrom::Start(5000)).unwrap(), 5000);
        let mut out = Vec::new();
        src.read_to_end(&mut out).unwrap();
        assert_eq!(out, data[5000..]);
    }

    #[test]
    fn seek_from_end_reads_last_bytes() {
        let rt = runtime();
        let data = random_bytes(10_000);
        let object = Arc::new(MemObject::new(data.clone()));
        let (mut src, _handle) =
            RangeSource::open(object, data.len() as u64, rt.handle(), test_config());
        assert_eq!(src.seek(SeekFrom::End(-10)).unwrap(), 9_990);
        let mut out = Vec::new();
        src.read_to_end(&mut out).unwrap();
        assert_eq!(out, data[9_990..]);
    }

    #[test]
    fn retryable_errors_then_success_reads_correctly() {
        let rt = runtime();
        let data = random_bytes(10_000);
        let plan = vec![
            StorageError::Network("planned 1".to_string()),
            StorageError::Network("planned 2".to_string()),
            StorageError::Network("planned 3".to_string()),
        ];
        let object = Arc::new(MemObject::with_failures(data.clone(), plan));
        let (mut src, handle) =
            RangeSource::open(object, data.len() as u64, rt.handle(), test_config());
        let mut out = Vec::new();
        src.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
        assert_eq!(handle.last_error(), None);
    }

    #[test]
    fn planned_not_found_fails_read() {
        let rt = runtime();
        let data = random_bytes(10_000);
        let plan = vec![StorageError::NotFound("planned".to_string())];
        let object = Arc::new(MemObject::with_failures(data.clone(), plan));
        let (mut src, _handle) =
            RangeSource::open(object, data.len() as u64, rt.handle(), test_config());
        let mut buf = [0u8; 64];
        assert!(src.read(&mut buf).is_err());
    }

    #[test]
    fn zero_size_read_returns_zero() {
        let rt = runtime();
        let object = Arc::new(MemObject::new(Vec::new()));
        let (mut src, _handle) = RangeSource::open(object, 0, rt.handle(), test_config());
        let mut buf = [0u8; 64];
        assert_eq!(src.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn pending_fetch_times_out_while_buffering() {
        let rt = runtime();
        let mut config = test_config();
        config.read_timeout = Duration::from_millis(200);
        let object = Arc::new(MemObject::never_returns());
        let (mut src, handle) = RangeSource::open(object, 10_000, rt.handle(), config);
        let mut buf = [0u8; 64];
        let err = src.read(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(handle.is_buffering());
    }
}
