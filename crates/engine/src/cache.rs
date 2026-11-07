//! Whole-track disk cache for streamed tracks.
//!
//! While a track streams through a [`CachingObject`], every chunk it fetches is also written into
//! a sparse `.part` file next to a chunk bitmap `.map`. When all chunks are present the `.part`
//! file is renamed to `.audio` and the next open of that track reads the local file. Entries are
//! named by a hash of the storage key and size. [`TrackCache`] keeps the directory under a byte
//! limit (evicting least recently used unpinned entries) and expires unpinned entries older than
//! a TTL. Playback never fails because of the cache: write errors disable the writer.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use sonorus_storage::StorageError;

use crate::error::{EngineError, Result};
use crate::source::{BoxFuture, RemoteObject};

/// Cache chunk size in bytes. Must equal `SourceConfig::default().chunk_size` so that chunks
/// fetched by the streaming source line up with the cache bitmap.
pub const CACHE_CHUNK: u64 = 1024 * 1024;

/// Largest track (in chunks) the cache will keep a bitmap for; bounds the map allocation.
const MAX_CHUNKS: u64 = 1 << 24;
/// The chunk map is persisted after this many newly written chunks.
const PERSIST_EVERY: u32 = 8;
/// Retries after the first failed fetch in [`download`].
const DOWNLOAD_RETRIES: u32 = 3;
/// Base delay of the [`download`] retry backoff (doubles per retry).
const DOWNLOAD_BACKOFF: Duration = Duration::from_millis(500);

const AUDIO_EXT: &str = "audio";
const PART_EXT: &str = "part";
const MAP_EXT: &str = "map";
/// Length of an entry name (hex characters of the BLAKE3 digest).
const NAME_LEN: usize = 32;

/// Snapshot of the cache directory returned by [`TrackCache::usage`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheUsage {
    /// Bytes used by complete and partial entries.
    pub total_bytes: u64,
    /// Bytes used by pinned entries.
    pub pinned_bytes: u64,
    /// Number of complete (`.audio`) entries.
    pub complete_files: u32,
    /// Number of partial (`.part`) entries.
    pub partial_files: u32,
    /// Configured byte limit (0 disables caching of unpinned tracks).
    pub limit_bytes: u64,
    /// Expiry of unpinned entries in seconds; None when entries never expire.
    pub ttl_secs: Option<u64>,
}

/// Disk cache of whole tracks. Shared as `Arc<TrackCache>`; all methods take `&self`.
pub struct TrackCache {
    inner: Arc<Mutex<Inner>>,
}

/// State guarded by the cache mutex. Writers hold a clone of the same mutex.
struct Inner {
    dir: PathBuf,
    limit_bytes: u64,
    ttl: Option<Duration>,
    /// Entry names that are never evicted or expired.
    pinned: HashSet<String>,
    /// Live writers by entry name; a dead `Weak` means no writer.
    writers: HashMap<String, Weak<PartialWriter>>,
}

impl Inner {
    /// True while a [`PartialWriter`] for `name` is alive.
    fn is_live(&self, name: &str) -> bool {
        self.writers.get(name).is_some_and(|w| w.strong_count() > 0)
    }
}

impl TrackCache {
    /// Opens (creating if needed) the cache in `dir`. Removes orphaned `.part`/`.map` files,
    /// then applies expiry and the byte limit. `ttl` of None means unpinned entries never expire.
    pub fn open(dir: PathBuf, limit_bytes: u64, ttl: Option<Duration>) -> Result<Arc<TrackCache>> {
        fs::create_dir_all(&dir)?;
        let mut inner = Inner {
            dir,
            limit_bytes,
            ttl,
            pinned: HashSet::new(),
            writers: HashMap::new(),
        };
        let groups = scan_dir(&inner.dir)?;
        remove_orphans(&inner.dir, &groups);
        evict_locked(&mut inner);
        Ok(Arc::new(TrackCache {
            inner: Arc::new(Mutex::new(inner)),
        }))
    }

    /// Returns the path of a complete entry for `key` whose length equals `size`, and bumps its
    /// mtime for LRU. A wrong-length entry is deleted as corrupt. An expired unpinned entry is
    /// deleted and reported as a miss. Returns None for `size == 0`.
    pub fn lookup(&self, key: &str, size: u64) -> Option<PathBuf> {
        if size == 0 {
            return None;
        }
        let inner = lock(&self.inner);
        let name = entry_name(key, size);
        let path = file_path(&inner.dir, &name, AUDIO_EXT);
        let meta = fs::metadata(&path).ok().filter(|m| m.is_file())?;
        if meta.len() != size {
            log::warn!("removing corrupt cache entry {name}");
            if let Err(e) = remove_if_exists(&path) {
                log::warn!("could not remove corrupt cache entry {name}: {e}");
            }
            return None;
        }
        let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
        let expired = ttl_cutoff(inner.ttl).is_some_and(|cutoff| mtime < cutoff);
        if expired && !is_protected(&inner, &name) {
            log::info!("expiring cache entry {name} on lookup");
            if let Err(e) = remove_entry(&inner.dir, &name) {
                log::warn!("could not expire cache entry {name}: {e}");
            }
            return None;
        }
        touch(&path);
        Some(path)
    }

    /// Returns the live writer for `key`/`size`, or creates one. Creating a writer opens (or
    /// resumes) the `.part` file and its chunk map. Fails for `size == 0`, for tracks whose entry
    /// is already complete, for tracks too large to map, and for unpinned tracks while the limit
    /// is 0 (callers then stream without caching; see [`TrackCache::caching_enabled_for`]).
    pub fn writer(&self, key: &str, size: u64) -> Result<Arc<PartialWriter>> {
        if size == 0 {
            return Err(cache_error("empty tracks are not cached"));
        }
        let chunks = chunk_count(size);
        if chunks > MAX_CHUNKS {
            return Err(cache_error("track is too large to cache"));
        }
        let name = entry_name(key, size);
        let mut inner = lock(&self.inner);
        if let Some(live) = inner.writers.get(&name).and_then(Weak::upgrade) {
            return Ok(live);
        }
        if inner.limit_bytes == 0 && !inner.pinned.contains(&name) {
            return Err(cache_error("caching is disabled for unpinned tracks"));
        }
        let audio_path = file_path(&inner.dir, &name, AUDIO_EXT);
        if file_len(&audio_path) == Some(size) {
            return Err(cache_error("entry is already complete"));
        }
        let part_path = file_path(&inner.dir, &name, PART_EXT);
        let map_path = file_path(&inner.dir, &name, MAP_EXT);

        let map_len = map_len(chunks);
        // A saved map is only trusted when the partial file it describes is still there.
        let resumable = file_len(&part_path) == Some(size);
        let saved = if resumable {
            fs::read(&map_path)
                .ok()
                .filter(|bits| bits.len() == map_len)
        } else {
            None
        };
        let bits = saved.unwrap_or_else(|| vec![0u8; map_len]);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&part_path)?;
        file.set_len(size)?;
        // Bump the mtime so the new partial is not the first LRU victim.
        if let Err(e) = file.set_modified(SystemTime::now()) {
            log::debug!("could not bump mtime of {name}: {e}");
        }
        persist_map(&map_path, &bits)?;
        let present = count_present(&bits, chunks);
        let writer = Arc::new(PartialWriter {
            cache: Arc::clone(&self.inner),
            name: name.clone(),
            size,
            chunks,
            part_path,
            map_path,
            audio_path,
            state: Mutex::new(WriterState {
                file: Some(file),
                bits,
                present,
                unsaved: 0,
                done: false,
                disabled: false,
            }),
        });
        if present == chunks {
            // Every chunk survived an earlier run that stopped before the final rename.
            let mut st = lock(&writer.state);
            writer.complete_locked(&mut st)?;
        }
        inner.writers.retain(|_, w| w.strong_count() > 0);
        inner.writers.insert(name, Arc::downgrade(&writer));
        Ok(writer)
    }

    /// True when [`TrackCache::writer`] may succeed for `key`/`size`: the track is non-empty and
    /// either the limit is non-zero or the track is pinned.
    pub fn caching_enabled_for(&self, key: &str, size: u64) -> bool {
        if size == 0 {
            return false;
        }
        let inner = lock(&self.inner);
        inner.limit_bytes > 0 || inner.pinned.contains(&entry_name(key, size))
    }

    /// Replaces the pinned set. Pinned entries are never evicted or expired. Runs eviction.
    pub fn set_pinned(&self, tracks: impl IntoIterator<Item = (String, u64)>) {
        let mut inner = lock(&self.inner);
        inner.pinned = tracks
            .into_iter()
            .map(|(key, size)| entry_name(&key, size))
            .collect();
        evict_locked(&mut inner);
    }

    /// True when a complete entry for `key`/`size` exists.
    pub fn is_complete(&self, key: &str, size: u64) -> bool {
        if size == 0 {
            return false;
        }
        let inner = lock(&self.inner);
        file_len(&file_path(&inner.dir, &entry_name(key, size), AUDIO_EXT)) == Some(size)
    }

    /// Sets the byte limit and evicts down to it.
    pub fn set_limit(&self, bytes: u64) {
        let mut inner = lock(&self.inner);
        inner.limit_bytes = bytes;
        evict_locked(&mut inner);
    }

    /// Sets the TTL of unpinned entries (None = never expire) and applies it immediately.
    pub fn set_ttl(&self, ttl: Option<Duration>) {
        let mut inner = lock(&self.inner);
        inner.ttl = ttl;
        evict_locked(&mut inner);
    }

    /// Deletes unpinned entries older than the TTL, skipping names with a live writer.
    /// Returns the bytes freed. Does nothing when no TTL is set.
    pub fn purge_expired(&self) -> Result<u64> {
        let inner = lock(&self.inner);
        let groups = scan_dir(&inner.dir)?;
        Ok(expire_locked(&inner, &groups))
    }

    /// Snapshot of the cache contents and settings.
    pub fn usage(&self) -> CacheUsage {
        let inner = lock(&self.inner);
        let mut usage = CacheUsage {
            limit_bytes: inner.limit_bytes,
            ttl_secs: inner.ttl.map(|ttl| ttl.as_secs()),
            ..CacheUsage::default()
        };
        let Ok(groups) = scan_dir(&inner.dir) else {
            return usage;
        };
        for (name, group) in &groups {
            let bytes = group.bytes();
            usage.total_bytes = usage.total_bytes.saturating_add(bytes);
            if inner.pinned.contains(name) {
                usage.pinned_bytes = usage.pinned_bytes.saturating_add(bytes);
            }
            if group.audio.is_some() {
                usage.complete_files = usage.complete_files.saturating_add(1);
            }
            if group.part.is_some() {
                usage.partial_files = usage.partial_files.saturating_add(1);
            }
        }
        usage
    }

    /// Deletes every unpinned entry that has no live writer. Returns the bytes freed.
    pub fn clear_unpinned(&self) -> Result<u64> {
        let inner = lock(&self.inner);
        let groups = scan_dir(&inner.dir)?;
        let mut freed = 0u64;
        for (name, group) in &groups {
            if is_protected(&inner, name) {
                continue;
            }
            remove_entry(&inner.dir, name)?;
            freed = freed.saturating_add(group.bytes());
        }
        Ok(freed)
    }
}

/// Writes the chunks of one track into its `.part` file and promotes it to `.audio` when all
/// chunks are present. Obtained from [`TrackCache::writer`].
pub struct PartialWriter {
    cache: Arc<Mutex<Inner>>,
    name: String,
    size: u64,
    chunks: u64,
    part_path: PathBuf,
    map_path: PathBuf,
    audio_path: PathBuf,
    state: Mutex<WriterState>,
}

struct WriterState {
    /// None once the entry was promoted.
    file: Option<File>,
    /// One bit per chunk; bit `i` set means chunk `i` is on disk.
    bits: Vec<u8>,
    present: u64,
    /// Chunks written since the map was last persisted.
    unsaved: u32,
    /// The entry was promoted to `.audio`.
    done: bool,
    /// A write failed; further writes are no-ops.
    disabled: bool,
}

impl PartialWriter {
    /// Writes `data` at `offset` if it is a whole chunk (aligned start, and length equal to the
    /// chunk length or the exact tail length). Anything else, and chunks already present, are
    /// ignored and return Ok. A failed disk write logs once, disables this writer and returns the
    /// error. Completing the last chunk promotes the entry.
    pub fn write_chunk(&self, offset: u64, data: &[u8]) -> Result<()> {
        let Some(index) = self.chunk_index(offset, data.len()) else {
            return Ok(());
        };
        let mut st = lock(&self.state);
        if st.done || st.disabled || bit_set(&st.bits, index) {
            return Ok(());
        }
        let Some(file) = st.file.as_ref() else {
            return Ok(());
        };
        if let Err(e) = write_at(file, offset, data) {
            st.disabled = true;
            log::warn!(
                "cache write for {} failed, caching disabled for it: {e}",
                self.name
            );
            return Err(EngineError::Io(e));
        }
        set_bit(&mut st.bits, index);
        st.present += 1;
        st.unsaved += 1;
        if st.unsaved >= PERSIST_EVERY {
            self.persist_locked(&mut st);
        }
        if st.present < self.chunks {
            return Ok(());
        }
        if let Err(e) = self.complete_locked(&mut st) {
            st.disabled = true;
            log::warn!("cache promotion of {} failed: {e}", self.name);
            return Err(e);
        }
        drop(st);
        let mut inner = lock(&self.cache);
        evict_locked(&mut inner);
        Ok(())
    }

    /// Offsets of the chunks not yet on disk, in ascending order.
    pub fn missing_chunks(&self) -> Vec<u64> {
        let st = lock(&self.state);
        if st.done {
            return Vec::new();
        }
        (0..self.chunks)
            .filter(|&i| !bit_set(&st.bits, i))
            .map(|i| i * CACHE_CHUNK)
            .collect()
    }

    /// True once the entry was promoted to a complete `.audio` file.
    pub fn is_complete(&self) -> bool {
        lock(&self.state).done
    }

    /// Chunk index for a valid write, or None when the write must be ignored.
    fn chunk_index(&self, offset: u64, len: usize) -> Option<u64> {
        if !offset.is_multiple_of(CACHE_CHUNK) {
            return None;
        }
        let index = offset / CACHE_CHUNK;
        if index >= self.chunks {
            return None;
        }
        (u64::try_from(len).ok()? == chunk_len(self.size, index)).then_some(index)
    }

    /// Persists the chunk map; a failure is logged and retried at the next persist point.
    fn persist_locked(&self, st: &mut WriterState) {
        match persist_map(&self.map_path, &st.bits) {
            Ok(()) => st.unsaved = 0,
            Err(e) => log::warn!("could not persist chunk map of {}: {e}", self.name),
        }
    }

    /// Syncs and renames `.part` to `.audio`, then removes the map.
    fn complete_locked(&self, st: &mut WriterState) -> Result<()> {
        if let Some(file) = st.file.take() {
            file.sync_all()?;
        }
        fs::rename(&self.part_path, &self.audio_path)?;
        st.done = true;
        if let Err(e) = remove_if_exists(&self.map_path) {
            log::warn!("could not remove chunk map of {}: {e}", self.name);
        }
        Ok(())
    }
}

impl Drop for PartialWriter {
    fn drop(&mut self) {
        let st = self
            .state
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !st.done
            && st.unsaved > 0
            && let Err(e) = persist_map(&self.map_path, &st.bits)
        {
            log::warn!("could not persist chunk map of {}: {e}", self.name);
        }
    }
}

/// [`RemoteObject`] that records every chunk it returns into a [`PartialWriter`].
pub struct CachingObject {
    inner: Arc<dyn RemoteObject>,
    writer: Arc<PartialWriter>,
}

impl CachingObject {
    /// Wraps `inner`; chunks returned by it are written through `writer`.
    pub fn new(inner: Arc<dyn RemoteObject>, writer: Arc<PartialWriter>) -> Self {
        Self { inner, writer }
    }
}

impl RemoteObject for CachingObject {
    fn get_range(&self, start: u64, len: u64) -> BoxFuture<'_, sonorus_storage::Result<Bytes>> {
        Box::pin(async move {
            let bytes = self.inner.get_range(start, len).await?;
            // The writer logs its own failures and disables itself; playback must not fail
            // because of the cache.
            let _ = self.writer.write_chunk(start, &bytes);
            Ok(bytes)
        })
    }
}

/// Fills the missing chunks of `key`/`size` from `object` into the cache, then completes the
/// entry. Each chunk is retried up to 3 times on retryable errors with 500 ms * 2^n backoff.
/// Stops early when `cancel` is set and returns an error, so the entry stays partial.
pub async fn download(
    cache: &TrackCache,
    object: Arc<dyn RemoteObject>,
    key: &str,
    size: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    if cache.is_complete(key, size) {
        return Ok(());
    }
    let writer = cache.writer(key, size)?;
    for offset in writer.missing_chunks() {
        if cancel.load(Ordering::Relaxed) {
            return Err(cache_error("download cancelled"));
        }
        let len = chunk_len(size, offset / CACHE_CHUNK);
        let bytes = fetch_chunk(object.as_ref(), offset, len, cancel).await?;
        writer.write_chunk(offset, &bytes)?;
    }
    if writer.is_complete() {
        Ok(())
    } else {
        Err(cache_error("download ended with missing chunks"))
    }
}

/// Fetches one chunk, retrying retryable failures with exponential backoff.
async fn fetch_chunk(
    object: &dyn RemoteObject,
    offset: u64,
    len: u64,
    cancel: &AtomicBool,
) -> Result<Bytes> {
    let mut retries: u32 = 0;
    loop {
        let failure = match object.get_range(offset, len).await {
            Ok(bytes) if bytes.len() as u64 == len => return Ok(bytes),
            Ok(bytes) => {
                StorageError::Network(format!("short read: got {} of {len} bytes", bytes.len()))
            }
            Err(e) => e,
        };
        if !failure.is_retryable() || retries >= DOWNLOAD_RETRIES {
            return Err(EngineError::Storage(failure));
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(cache_error("download cancelled"));
        }
        let delay = DOWNLOAD_BACKOFF * 2u32.pow(retries);
        retries += 1;
        log::warn!("chunk at {offset} failed, retrying in {delay:?}: {failure}");
        tokio::time::sleep(delay).await;
    }
}

/// Locks, ignoring poisoning: the guarded state is only ever left consistent between steps.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cache_error(msg: impl Into<String>) -> EngineError {
    EngineError::Cache(msg.into())
}

/// First 32 hex characters of `blake3(key || "\n" || size_decimal)`.
fn entry_name(key: &str, size: u64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(key.as_bytes());
    hasher.update(b"\n");
    hasher.update(size.to_string().as_bytes());
    let hex = hasher.finalize().to_hex();
    hex.as_str()[..NAME_LEN].to_string()
}

fn is_entry_name(stem: &str) -> bool {
    stem.len() == NAME_LEN && stem.bytes().all(|b| b.is_ascii_hexdigit())
}

fn file_path(dir: &Path, name: &str, ext: &str) -> PathBuf {
    dir.join(format!("{name}.{ext}"))
}

fn chunk_count(size: u64) -> u64 {
    size.div_ceil(CACHE_CHUNK)
}

/// Length of chunk `index` of a `size`-byte track (the last chunk may be short).
fn chunk_len(size: u64, index: u64) -> u64 {
    CACHE_CHUNK.min(size.saturating_sub(index.saturating_mul(CACHE_CHUNK)))
}

fn map_len(chunks: u64) -> usize {
    usize::try_from(chunks.div_ceil(8)).unwrap_or(usize::MAX)
}

fn bit_set(bits: &[u8], index: u64) -> bool {
    usize::try_from(index / 8)
        .ok()
        .and_then(|i| bits.get(i))
        .is_some_and(|byte| *byte & (1u8 << (index % 8)) != 0)
}

fn set_bit(bits: &mut [u8], index: u64) {
    if let Some(byte) = usize::try_from(index / 8)
        .ok()
        .and_then(|i| bits.get_mut(i))
    {
        *byte |= 1u8 << (index % 8);
    }
}

fn count_present(bits: &[u8], chunks: u64) -> u64 {
    (0..chunks).filter(|&i| bit_set(bits, i)).count() as u64
}

#[cfg(unix)]
fn write_at(file: &File, offset: u64, data: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(data, offset)
}

#[cfg(not(unix))]
fn write_at(file: &File, offset: u64, data: &[u8]) -> io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = file;
    f.seek(SeekFrom::Start(offset))?;
    f.write_all(data)
}

fn persist_map(path: &Path, bits: &[u8]) -> io::Result<()> {
    fs::write(path, bits)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Removes all files of an entry.
fn remove_entry(dir: &Path, name: &str) -> io::Result<()> {
    for ext in [AUDIO_EXT, PART_EXT, MAP_EXT] {
        remove_if_exists(&file_path(dir, name, ext))?;
    }
    Ok(())
}

/// Length of a regular file, or None when it is missing.
fn file_len(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
}

/// Marks `path` as used now (LRU). Failure is harmless: the entry just ages sooner.
fn touch(path: &Path) {
    if let Ok(file) = File::open(path)
        && let Err(e) = file.set_modified(SystemTime::now())
    {
        log::debug!("could not bump mtime of {}: {e}", path.display());
    }
}

#[derive(Debug, Clone, Copy)]
struct FileMeta {
    len: u64,
    mtime: SystemTime,
}

/// The files of one entry as found on disk.
#[derive(Debug, Default)]
struct Group {
    audio: Option<FileMeta>,
    part: Option<FileMeta>,
    map: bool,
}

impl Group {
    fn bytes(&self) -> u64 {
        self.audio
            .map_or(0, |m| m.len)
            .saturating_add(self.part.map_or(0, |m| m.len))
    }

    /// Last access time of the entry (newest of its data files).
    fn mtime(&self) -> SystemTime {
        self.audio
            .iter()
            .chain(self.part.iter())
            .map(|m| m.mtime)
            .max()
            .unwrap_or(UNIX_EPOCH)
    }
}

/// Lists the entries in `dir`, ignoring anything that is not a cache file.
fn scan_dir(dir: &Path) -> io::Result<BTreeMap<String, Group>> {
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some((stem, ext)) = file_name.rsplit_once('.') else {
            continue;
        };
        if !is_entry_name(stem) || !matches!(ext, AUDIO_EXT | PART_EXT | MAP_EXT) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let group = groups.entry(stem.to_string()).or_default();
        let info = FileMeta {
            len: meta.len(),
            mtime: meta.modified().unwrap_or(UNIX_EPOCH),
        };
        match ext {
            AUDIO_EXT => group.audio = Some(info),
            PART_EXT => group.part = Some(info),
            _ => group.map = true,
        }
    }
    Ok(groups)
}

/// Deletes files that cannot form a valid entry: a `.map` without `.part`, a `.part` without
/// `.map`, and leftovers next to a complete `.audio`.
fn remove_orphans(dir: &Path, groups: &BTreeMap<String, Group>) {
    for (name, group) in groups {
        let (drop_part, drop_map) = match (&group.audio, &group.part, group.map) {
            (Some(_), part, map) => (part.is_some(), map),
            (None, Some(_), false) => (true, false),
            (None, None, true) => (false, true),
            _ => (false, false),
        };
        if drop_part {
            log::info!("removing orphaned partial cache file {name}");
            if let Err(e) = remove_if_exists(&file_path(dir, name, PART_EXT)) {
                log::warn!("could not remove {name}.part: {e}");
            }
        }
        if drop_map {
            log::info!("removing orphaned chunk map {name}");
            if let Err(e) = remove_if_exists(&file_path(dir, name, MAP_EXT)) {
                log::warn!("could not remove {name}.map: {e}");
            }
        }
    }
}

/// Entries that must not be removed: pinned, or with a live writer.
fn is_protected(inner: &Inner, name: &str) -> bool {
    inner.pinned.contains(name) || inner.is_live(name)
}

/// Mtime before which unpinned entries expire, or None when entries never expire.
fn ttl_cutoff(ttl: Option<Duration>) -> Option<SystemTime> {
    ttl.and_then(|ttl| SystemTime::now().checked_sub(ttl))
}

fn is_expirable(inner: &Inner, name: &str, group: &Group, cutoff: Option<SystemTime>) -> bool {
    cutoff.is_some_and(|cutoff| group.mtime() < cutoff) && !is_protected(inner, name)
}

/// Deletes every expired entry. Returns the bytes freed.
fn expire_locked(inner: &Inner, groups: &BTreeMap<String, Group>) -> u64 {
    let cutoff = ttl_cutoff(inner.ttl);
    let mut freed = 0u64;
    for (name, group) in groups {
        if !is_expirable(inner, name, group, cutoff) {
            continue;
        }
        match remove_entry(&inner.dir, name) {
            Ok(()) => {
                freed = freed.saturating_add(group.bytes());
                log::info!("expired cache entry {name}");
            }
            Err(e) => log::warn!("failed to expire cache entry {name}: {e}"),
        }
    }
    freed
}

/// Expires old entries, then deletes least recently used unpinned entries (no live writer)
/// until the total is within the limit. Pinned entries are never removed.
fn evict_locked(inner: &mut Inner) {
    let groups = match scan_dir(&inner.dir) {
        Ok(groups) => groups,
        Err(e) => {
            log::warn!("cache scan failed, skipping eviction: {e}");
            return;
        }
    };
    let view: &Inner = inner;
    let expired = expire_locked(view, &groups);
    let mut total = groups
        .values()
        .map(Group::bytes)
        .fold(0u64, u64::saturating_add)
        .saturating_sub(expired);
    if total <= view.limit_bytes {
        return;
    }
    let cutoff = ttl_cutoff(view.ttl);
    let mut candidates: Vec<(SystemTime, String, u64)> = groups
        .iter()
        .filter(|(name, group)| {
            !is_protected(view, name) && !is_expirable(view, name, group, cutoff)
        })
        .map(|(name, group)| (group.mtime(), name.clone(), group.bytes()))
        .collect();
    candidates.sort();
    for (_, name, bytes) in candidates {
        if total <= view.limit_bytes {
            break;
        }
        match remove_entry(&view.dir, &name) {
            Ok(()) => {
                total = total.saturating_sub(bytes);
                log::info!("evicted cache entry {name} ({bytes} bytes)");
            }
            Err(e) => log::warn!("failed to evict cache entry {name}: {e}"),
        }
    }
}

/// Test helpers shared with other modules' tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Unique directory under the system temp dir, removed on drop.
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new() -> Self {
            let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            Self(
                std::env::temp_dir()
                    .join(format!("sonorus-cache-{}-{n}-{nanos}", std::process::id())),
            )
        }

        pub(crate) fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Deterministic test bytes.
    pub(crate) fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{TempDir, pattern};
    use super::*;
    use crate::source::{RangeSource, SourceConfig};
    use std::collections::VecDeque;
    use std::io::Read;

    const MAX: u64 = u64::MAX;
    const DAY: Duration = Duration::from_secs(24 * 3600);

    /// In-memory object that records calls and pops planned errors before serving.
    struct FakeRemote {
        data: Bytes,
        calls: Mutex<Vec<u64>>,
        fail_plan: Mutex<VecDeque<StorageError>>,
    }

    impl FakeRemote {
        fn new(data: &[u8], plan: Vec<StorageError>) -> Arc<Self> {
            Arc::new(Self {
                data: Bytes::from(data.to_vec()),
                calls: Mutex::new(Vec::new()),
                fail_plan: Mutex::new(plan.into_iter().collect()),
            })
        }

        fn calls(&self) -> Vec<u64> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl RemoteObject for FakeRemote {
        fn get_range(&self, start: u64, len: u64) -> BoxFuture<'_, sonorus_storage::Result<Bytes>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(start);
                if let Some(err) = self.fail_plan.lock().unwrap().pop_front() {
                    return Err(err);
                }
                let total = self.data.len() as u64;
                let from = start.min(total) as usize;
                let to = start.saturating_add(len).min(total) as usize;
                Ok(self.data.slice(from..to))
            })
        }
    }

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Runtime::new().unwrap().block_on(fut)
    }

    fn open(dir: &TempDir, limit: u64, ttl: Option<Duration>) -> Arc<TrackCache> {
        TrackCache::open(dir.path(), limit, ttl).unwrap()
    }

    /// Writes every chunk of `data` through a writer for `key`.
    fn store(cache: &TrackCache, key: &str, data: &[u8]) {
        let size = data.len() as u64;
        let writer = cache.writer(key, size).unwrap();
        for offset in writer.missing_chunks() {
            let start = offset as usize;
            let end = (offset + CACHE_CHUNK).min(size) as usize;
            writer.write_chunk(offset, &data[start..end]).unwrap();
        }
        assert!(writer.is_complete());
    }

    fn audio_path(dir: &TempDir, key: &str, size: u64) -> PathBuf {
        file_path(&dir.path(), &entry_name(key, size), AUDIO_EXT)
    }

    fn part_path(dir: &TempDir, key: &str, size: u64) -> PathBuf {
        file_path(&dir.path(), &entry_name(key, size), PART_EXT)
    }

    /// Moves a file's mtime `age` into the past.
    fn set_age(path: &Path, age: Duration) {
        let when = SystemTime::now().checked_sub(age).unwrap();
        File::open(path).unwrap().set_modified(when).unwrap();
    }

    #[test]
    fn chunk_constant_matches_source_default() {
        assert_eq!(CACHE_CHUNK, SourceConfig::default().chunk_size);
    }

    #[test]
    fn entry_name_is_stable_and_sensitive_to_key_and_size() {
        let name = entry_name("a/b.flac", 10);
        let expected = blake3::hash(b"a/b.flac\n10").to_hex();
        assert_eq!(name, expected.as_str()[..NAME_LEN]);
        assert_eq!(name.len(), NAME_LEN);
        assert_ne!(name, entry_name("a/b.flac", 11));
        assert_ne!(name, entry_name("a/c.flac", 10));
    }

    #[test]
    fn streamed_track_completes_entry_and_lookup_serves_it() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        let data = pattern(2 * CACHE_CHUNK as usize + 1000, 3);
        let size = data.len() as u64;
        let writer = cache.writer("t.flac", size).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let object = Arc::new(CachingObject::new(FakeRemote::new(&data, vec![]), writer));
        let (mut src, _handle) =
            RangeSource::open(object, size, rt.handle(), SourceConfig::default());
        let mut out = Vec::new();
        src.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
        drop(src);

        assert!(cache.is_complete("t.flac", size));
        let path = cache.lookup("t.flac", size).unwrap();
        assert_eq!(fs::read(path).unwrap(), data);
        assert!(!part_path(&dir, "t.flac", size).exists());
    }

    #[test]
    fn partial_persists_and_reopen_restores_missing_chunks() {
        let dir = TempDir::new();
        let data = pattern(3 * CACHE_CHUNK as usize + 5, 1);
        let size = data.len() as u64;
        let chunk = |i: u64| {
            let start = (i * CACHE_CHUNK) as usize;
            let end = (start + CACHE_CHUNK as usize).min(data.len());
            data[start..end].to_vec()
        };
        {
            let cache = open(&dir, MAX, None);
            let writer = cache.writer("p.flac", size).unwrap();
            writer.write_chunk(0, &chunk(0)).unwrap();
            writer.write_chunk(2 * CACHE_CHUNK, &chunk(2)).unwrap();
            assert_eq!(writer.missing_chunks(), vec![CACHE_CHUNK, 3 * CACHE_CHUNK]);
        }
        let cache = open(&dir, MAX, None);
        let usage = cache.usage();
        assert_eq!(usage.partial_files, 1);
        assert_eq!(usage.complete_files, 0);
        assert_eq!(usage.total_bytes, size);

        let writer = cache.writer("p.flac", size).unwrap();
        assert_eq!(writer.missing_chunks(), vec![CACHE_CHUNK, 3 * CACHE_CHUNK]);
        writer.write_chunk(CACHE_CHUNK, &chunk(1)).unwrap();
        writer.write_chunk(3 * CACHE_CHUNK, &chunk(3)).unwrap();
        assert!(writer.is_complete());
        drop(writer);
        assert_eq!(
            fs::read(cache.lookup("p.flac", size).unwrap()).unwrap(),
            data
        );
    }

    #[test]
    fn tail_and_short_tracks_complete() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        for (key, len) in [
            ("tail.flac", 2 * CACHE_CHUNK as usize + 100),
            ("short.flac", 10),
        ] {
            let data = pattern(len, 9);
            store(&cache, key, &data);
            let path = cache.lookup(key, len as u64).unwrap();
            assert_eq!(fs::read(path).unwrap(), data);
        }
    }

    #[test]
    fn zero_size_is_never_cached() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        assert!(cache.writer("empty", 0).is_err());
        assert!(!cache.caching_enabled_for("empty", 0));
        assert!(cache.lookup("empty", 0).is_none());
        assert!(!cache.is_complete("empty", 0));
    }

    #[test]
    fn misaligned_and_wrong_length_writes_are_ignored() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        let data = pattern(CACHE_CHUNK as usize + 10, 5);
        let size = data.len() as u64;
        let writer = cache.writer("m.flac", size).unwrap();
        let full = CACHE_CHUNK as usize;
        // Misaligned offset.
        writer.write_chunk(1, &data[1..full + 1]).unwrap();
        // Wrong length for a full chunk (short and long).
        writer.write_chunk(0, &data[..full - 1]).unwrap();
        writer.write_chunk(0, &data[..full + 1]).unwrap();
        // Wrong tail length.
        writer.write_chunk(CACHE_CHUNK, &data[..5]).unwrap();
        // Beyond the end.
        writer.write_chunk(2 * CACHE_CHUNK, &data[..5]).unwrap();
        assert_eq!(writer.missing_chunks(), vec![0, CACHE_CHUNK]);
        assert!(!writer.is_complete());

        writer.write_chunk(0, &data[..full]).unwrap();
        writer.write_chunk(CACHE_CHUNK, &data[full..]).unwrap();
        assert!(writer.is_complete());
    }

    #[test]
    fn eviction_removes_oldest_unpinned_first() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        let a = pattern(2 * CACHE_CHUNK as usize, 1);
        let b = pattern(2 * CACHE_CHUNK as usize, 2);
        let c = pattern(2 * CACHE_CHUNK as usize, 3);
        store(&cache, "a", &a);
        store(&cache, "b", &b);
        store(&cache, "c", &c);
        set_age(
            &audio_path(&dir, "a", a.len() as u64),
            Duration::from_secs(300),
        );
        set_age(
            &audio_path(&dir, "b", b.len() as u64),
            Duration::from_secs(200),
        );
        set_age(
            &audio_path(&dir, "c", c.len() as u64),
            Duration::from_secs(100),
        );
        cache.set_pinned([("a".to_string(), a.len() as u64)]);
        assert_eq!(cache.usage().total_bytes, 6 * CACHE_CHUNK);

        cache.set_limit(4 * CACHE_CHUNK);
        assert!(cache.is_complete("a", a.len() as u64), "pinned entry kept");
        assert!(
            !cache.is_complete("b", b.len() as u64),
            "oldest unpinned removed"
        );
        assert!(cache.is_complete("c", c.len() as u64));
        let usage = cache.usage();
        assert_eq!(usage.total_bytes, 4 * CACHE_CHUNK);
        assert_eq!(usage.pinned_bytes, 2 * CACHE_CHUNK);
    }

    #[test]
    fn eviction_skips_live_writers_and_keeps_pinned_over_limit() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        let a = pattern(2 * CACHE_CHUNK as usize, 1);
        let c = pattern(2 * CACHE_CHUNK as usize, 3);
        store(&cache, "a", &a);
        store(&cache, "c", &c);
        cache.set_pinned([("a".to_string(), a.len() as u64)]);

        let p_size = 2 * CACHE_CHUNK;
        let writer = cache.writer("p", p_size).unwrap();
        writer
            .write_chunk(0, &pattern(CACHE_CHUNK as usize, 4))
            .unwrap();
        set_age(&part_path(&dir, "p", p_size), Duration::from_secs(500));
        set_age(
            &audio_path(&dir, "c", c.len() as u64),
            Duration::from_secs(100),
        );

        cache.set_limit(1);
        assert!(cache.is_complete("a", a.len() as u64));
        assert!(!cache.is_complete("c", c.len() as u64));
        assert!(part_path(&dir, "p", p_size).exists(), "live writer kept");
        assert_eq!(cache.usage().partial_files, 1);
        drop(writer);
    }

    #[test]
    fn limit_zero_disables_unpinned_caching() {
        let dir = TempDir::new();
        let cache = open(&dir, 0, None);
        assert!(!cache.caching_enabled_for("x", 10));
        assert!(cache.writer("x", 10).is_err());
        cache.set_pinned([("x".to_string(), 10)]);
        assert!(cache.caching_enabled_for("x", 10));
        assert!(cache.writer("x", 10).is_ok());
    }

    #[test]
    fn corrupt_complete_file_is_dropped() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        let data = pattern(100, 7);
        store(&cache, "c", &data);
        let path = audio_path(&dir, "c", 100);
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(5)
            .unwrap();
        assert!(cache.lookup("c", 100).is_none());
        assert!(!path.exists());
    }

    #[test]
    fn expired_entry_removed_on_open() {
        let dir = TempDir::new();
        let a = pattern(1000, 1);
        let p_size = 2 * CACHE_CHUNK;
        {
            let cache = open(&dir, MAX, None);
            store(&cache, "a", &a);
            let writer = cache.writer("p", p_size).unwrap();
            writer
                .write_chunk(0, &pattern(CACHE_CHUNK as usize, 2))
                .unwrap();
            drop(writer);
            set_age(&audio_path(&dir, "a", 1000), 40 * DAY);
            set_age(&part_path(&dir, "p", p_size), 40 * DAY);
        }
        let cache = open(&dir, MAX, Some(30 * DAY));
        assert!(!cache.is_complete("a", 1000));
        assert_eq!(
            cache.usage(),
            CacheUsage {
                limit_bytes: MAX,
                ttl_secs: Some(30 * 24 * 3600),
                ..CacheUsage::default()
            }
        );
    }

    #[test]
    fn expired_entry_is_a_miss_on_lookup() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, Some(30 * DAY));
        let data = pattern(500, 8);
        store(&cache, "old", &data);
        set_age(&audio_path(&dir, "old", 500), 31 * DAY);
        assert!(cache.lookup("old", 500).is_none());
        assert!(!audio_path(&dir, "old", 500).exists());

        store(&cache, "fresh", &data);
        assert!(cache.lookup("fresh", 500).is_some());
    }

    #[test]
    fn pinned_and_live_writer_never_expire() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, Some(30 * DAY));
        let a = pattern(500, 1);
        store(&cache, "pinned", &a);
        cache.set_pinned([("pinned".to_string(), 500)]);
        set_age(&audio_path(&dir, "pinned", 500), 60 * DAY);

        let p_size = 2 * CACHE_CHUNK;
        let writer = cache.writer("live", p_size).unwrap();
        writer
            .write_chunk(0, &pattern(CACHE_CHUNK as usize, 2))
            .unwrap();
        set_age(&part_path(&dir, "live", p_size), 60 * DAY);

        assert_eq!(cache.purge_expired().unwrap(), 0);
        assert!(audio_path(&dir, "pinned", 500).exists());
        assert!(part_path(&dir, "live", p_size).exists());
        assert!(cache.lookup("pinned", 500).is_some());

        drop(writer);
        assert!(cache.purge_expired().unwrap() > 0);
        assert!(!part_path(&dir, "live", p_size).exists());
        assert!(audio_path(&dir, "pinned", 500).exists());
    }

    #[test]
    fn none_ttl_never_expires() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        let data = pattern(500, 1);
        store(&cache, "old", &data);
        set_age(&audio_path(&dir, "old", 500), 3650 * DAY);
        assert_eq!(cache.purge_expired().unwrap(), 0);
        assert!(cache.lookup("old", 500).is_some());
        assert_eq!(cache.usage().ttl_secs, None);
    }

    #[test]
    fn set_ttl_applies_expiry_immediately() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        store(&cache, "old", &pattern(500, 1));
        set_age(&audio_path(&dir, "old", 500), 40 * DAY);
        cache.set_ttl(Some(30 * DAY));
        assert!(!cache.is_complete("old", 500));
        assert_eq!(cache.usage().ttl_secs, Some(30 * 24 * 3600));
    }

    #[test]
    fn download_fills_missing_chunks_and_completes() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        let data = pattern(3 * CACHE_CHUNK as usize + 7, 6);
        let size = data.len() as u64;
        {
            let writer = cache.writer("d", size).unwrap();
            writer
                .write_chunk(0, &data[..CACHE_CHUNK as usize])
                .unwrap();
        }
        let remote = FakeRemote::new(&data, vec![]);
        let cancel = AtomicBool::new(false);
        block_on(download(&cache, remote.clone(), "d", size, &cancel)).unwrap();
        assert_eq!(
            remote.calls(),
            vec![CACHE_CHUNK, 2 * CACHE_CHUNK, 3 * CACHE_CHUNK]
        );
        assert_eq!(fs::read(cache.lookup("d", size).unwrap()).unwrap(), data);
    }

    #[test]
    fn download_honors_cancel() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        let data = pattern(2 * CACHE_CHUNK as usize, 6);
        let size = data.len() as u64;
        let remote = FakeRemote::new(&data, vec![]);
        let cancel = AtomicBool::new(true);
        assert!(block_on(download(&cache, remote.clone(), "e", size, &cancel)).is_err());
        assert!(remote.calls().is_empty());
        assert!(!cache.is_complete("e", size));
    }

    #[test]
    fn download_retries_transient_and_fails_fast_on_permanent() {
        let dir = TempDir::new();
        let cache = open(&dir, MAX, None);
        let data = pattern(100, 6);
        let cancel = AtomicBool::new(false);

        let remote = FakeRemote::new(&data, vec![StorageError::Network("blip".into())]);
        block_on(download(&cache, remote.clone(), "r", 100, &cancel)).unwrap();
        assert_eq!(remote.calls().len(), 2);
        assert!(cache.is_complete("r", 100));

        let remote = FakeRemote::new(&data, vec![StorageError::NotFound("gone".into())]);
        assert!(block_on(download(&cache, remote.clone(), "n", 100, &cancel)).is_err());
        assert_eq!(remote.calls().len(), 1);
        assert!(!cache.is_complete("n", 100));
    }
}
