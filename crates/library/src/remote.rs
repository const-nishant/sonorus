//! Seekable `Read` over a bucket object, fetched lazily in fixed-size range requests.
//!
//! [`RemoteReader`] is a blocking adapter: every cache miss calls `Handle::block_on`, so it
//! MUST be driven from a blocking thread (e.g. `tokio::task::spawn_blocking`). Calling it from a
//! tokio worker thread will panic, and calling it from inside the runtime would deadlock.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom};

use bytes::Bytes;
use sonorus_storage::Storage;
use tokio::runtime::Handle;

/// Size of one range request (256 KiB).
const BLOCK_SIZE: u64 = 256 * 1024;
/// Maximum number of cached blocks.
const MAX_BLOCKS: usize = 64;
/// Maximum total bytes fetched by one reader (32 MiB).
const MAX_FETCH: u64 = 32 * 1024 * 1024;

/// Returns `(block_index, block_start, block_len)` for the block containing `pos`, where the
/// object is `size` bytes long and blocks are `block_size` bytes. The last block is clipped to
/// `size`. Returns `None` when `pos` is at or past the end, or when `block_size` is zero.
pub fn block_span(pos: u64, size: u64, block_size: u64) -> Option<(u64, u64, u64)> {
    if block_size == 0 || pos >= size {
        return None;
    }
    let index = pos / block_size;
    let start = index * block_size;
    let len = block_size.min(size - start);
    Some((index, start, len))
}

/// Lazily fetches a bucket object with `Storage::get_range`, behaving like a seekable file.
///
/// Must be used from a blocking thread; see the module docs.
pub struct RemoteReader {
    storage: Storage,
    key: String,
    size: u64,
    rt: Handle,
    blocks: BTreeMap<u64, Bytes>,
    pos: u64,
    block_size: u64,
    fetched: u64,
    max_fetch: u64,
}

impl RemoteReader {
    /// Creates a reader for `key`, which is `size` bytes long, using `rt` to drive the requests.
    pub fn new(storage: Storage, key: String, size: u64, rt: Handle) -> Self {
        Self {
            storage,
            key,
            size,
            rt,
            blocks: BTreeMap::new(),
            pos: 0,
            block_size: BLOCK_SIZE,
            fetched: 0,
            max_fetch: MAX_FETCH,
        }
    }

    /// Total bytes downloaded so far by this reader.
    pub fn fetched_bytes(&self) -> u64 {
        self.fetched
    }

    /// Fetches block `index` (`start`, `len`) and caches it, evicting the lowest-indexed other
    /// blocks when the cache holds more than `MAX_BLOCKS`.
    fn fetch_block(&mut self, index: u64, start: u64, len: u64) -> io::Result<()> {
        if self.fetched.saturating_add(len) > self.max_fetch {
            return Err(io::Error::other("tag read budget exceeded"));
        }
        let bytes = self
            .rt
            .block_on(self.storage.get_range(&self.key, start, len))
            .map_err(io::Error::other)?;
        self.fetched = self.fetched.saturating_add(bytes.len() as u64);
        self.blocks.insert(index, bytes);

        while self.blocks.len() > MAX_BLOCKS {
            let victim = self.blocks.keys().copied().find(|k| *k != index);
            match victim {
                Some(k) => {
                    self.blocks.remove(&k);
                }
                None => break,
            }
        }
        Ok(())
    }
}

impl Read for RemoteReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some((index, start, len)) = block_span(self.pos, self.size, self.block_size) else {
            return Ok(0);
        };
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.blocks.contains_key(&index) {
            self.fetch_block(index, start, len)?;
        }
        let Some(block) = self.blocks.get(&index) else {
            return Ok(0);
        };
        let offset = (self.pos - start) as usize;
        let available = block.len().saturating_sub(offset);
        let n = buf.len().min(available);
        if n == 0 {
            return Ok(0);
        }
        buf[..n].copy_from_slice(&block[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

fn offset_from(base: u64, delta: i64) -> io::Result<u64> {
    base.checked_add_signed(delta).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "seek to a negative or overflowing position",
        )
    })
}

impl Seek for RemoteReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let new_pos = match from {
            SeekFrom::Start(p) => p,
            SeekFrom::End(d) => offset_from(self.size, d)?,
            SeekFrom::Current(d) => offset_from(self.pos, d)?,
        };
        self.pos = new_pos;
        Ok(new_pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_span_first_block() {
        assert_eq!(block_span(0, 1000, 256), Some((0, 0, 256)));
        assert_eq!(block_span(255, 1000, 256), Some((0, 0, 256)));
    }

    #[test]
    fn block_span_middle_block() {
        assert_eq!(block_span(256, 1000, 256), Some((1, 256, 256)));
        assert_eq!(block_span(300, 1000, 256), Some((1, 256, 256)));
    }

    #[test]
    fn block_span_last_block_is_clipped() {
        assert_eq!(block_span(768, 1000, 256), Some((3, 768, 232)));
        assert_eq!(block_span(999, 1000, 256), Some((3, 768, 232)));
    }

    #[test]
    fn block_span_at_or_past_end_is_none() {
        assert_eq!(block_span(1000, 1000, 256), None);
        assert_eq!(block_span(5000, 1000, 256), None);
        assert_eq!(block_span(0, 0, 256), None);
    }

    #[test]
    fn block_span_zero_block_size_is_none() {
        assert_eq!(block_span(0, 1000, 0), None);
    }

    #[test]
    fn offset_from_rejects_negative_and_overflow() {
        assert_eq!(offset_from(10, -4).ok(), Some(6));
        assert_eq!(
            offset_from(10, -11).map_err(|e| e.kind()),
            Err(io::ErrorKind::InvalidInput)
        );
        assert_eq!(
            offset_from(u64::MAX, 1).map_err(|e| e.kind()),
            Err(io::ErrorKind::InvalidInput)
        );
    }
}
