//! Track openers: turn a [`TrackRef`] into a byte source the decoder can read.

use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use symphonia::core::io::MediaSource;

use crate::cache::{CachingObject, TrackCache};
use crate::error::Result;
use crate::source::{BufferHandle, RangeSource, RemoteObject, SourceConfig, StorageObject};
use sonorus_storage::Storage;

/// ReplayGain values read from a track's tags. Gains in dB, peaks as linear sample amplitude (1.0 = full scale).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ReplayGain {
    /// Track gain in dB (`REPLAYGAIN_TRACK_GAIN`).
    pub track_gain_db: Option<f32>,
    /// Track peak, linear (`REPLAYGAIN_TRACK_PEAK`).
    pub track_peak: Option<f32>,
    /// Album gain in dB (`REPLAYGAIN_ALBUM_GAIN`).
    pub album_gain_db: Option<f32>,
    /// Album peak, linear (`REPLAYGAIN_ALBUM_PEAK`).
    pub album_peak: Option<f32>,
}

/// A track to open. `key` is the storage object key (or path relative to a local root).
/// `gain` holds the track's ReplayGain tags when they are known.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackRef {
    pub id: String,
    pub key: String,
    pub size: u64,
    pub gain: Option<ReplayGain>,
}

/// Result of [`TrackOpener::open`].
pub struct OpenedSource {
    pub source: Box<dyn MediaSource>,
    pub ext_hint: Option<String>,
    pub buffer: Option<BufferHandle>,
}

pub trait TrackOpener: Send + Sync + 'static {
    /// Blocking; called on the decode thread (never the controller or a tokio worker).
    fn open(&self, track: &TrackRef) -> Result<OpenedSource>;
}

/// Lowercased text after the last '.' of the last '/' segment; None if absent.
pub fn ext_of(key: &str) -> Option<String> {
    let segment = key.rsplit('/').next().unwrap_or(key);
    let (_, ext) = segment.rsplit_once('.')?;
    if ext.is_empty() {
        None
    } else {
        Some(ext.to_lowercase())
    }
}

/// Opens tracks from remote storage through a [`RangeSource`].
pub struct StorageOpener {
    storage: Storage,
    rt: tokio::runtime::Handle,
    config: SourceConfig,
}

impl StorageOpener {
    /// Builds the retry-free client once so every opened track shares its connection pool.
    pub fn new(storage: Storage, rt: tokio::runtime::Handle, config: SourceConfig) -> Self {
        Self {
            storage: storage.without_sdk_retries(),
            rt,
            config,
        }
    }
}

impl TrackOpener for StorageOpener {
    /// Blocking; must only run on the decode thread (never the controller or a tokio worker).
    fn open(&self, track: &TrackRef) -> Result<OpenedSource> {
        // `self.storage` already has SDK retries disabled (see `new`).
        let object = Arc::new(StorageObject {
            storage: self.storage.clone(),
            key: track.key.clone(),
        });
        let (src, handle) = RangeSource::open(object, track.size, &self.rt, self.config.clone());
        Ok(OpenedSource {
            source: Box::new(src),
            ext_hint: ext_of(&track.key),
            buffer: Some(handle),
        })
    }
}

/// Opens tracks through the disk cache: a complete entry is read locally with no network;
/// otherwise the track streams from storage and its chunks are recorded in the cache.
pub struct CachingOpener {
    cache: Arc<TrackCache>,
    storage: Storage,
    rt: tokio::runtime::Handle,
    config: SourceConfig,
}

impl CachingOpener {
    /// Builds the retry-free client once, like [`StorageOpener::new`].
    pub fn new(
        cache: Arc<TrackCache>,
        storage: Storage,
        rt: tokio::runtime::Handle,
        config: SourceConfig,
    ) -> Self {
        Self {
            cache,
            storage: storage.without_sdk_retries(),
            rt,
            config,
        }
    }
}

impl TrackOpener for CachingOpener {
    /// Blocking; must only run on the decode thread (never the controller or a tokio worker).
    fn open(&self, track: &TrackRef) -> Result<OpenedSource> {
        if let Some(path) = self.cache.lookup(&track.key, track.size) {
            // A failed open (e.g. the entry was evicted just now) falls through to streaming.
            if let Ok(file) = File::open(&path) {
                return Ok(OpenedSource {
                    source: Box::new(file),
                    ext_hint: ext_of(&track.key),
                    buffer: None,
                });
            }
        }
        let storage_object = Arc::new(StorageObject {
            storage: self.storage.clone(),
            key: track.key.clone(),
        });
        let object: Arc<dyn RemoteObject> =
            if self.cache.caching_enabled_for(&track.key, track.size) {
                match self.cache.writer(&track.key, track.size) {
                    Ok(writer) => Arc::new(CachingObject::new(storage_object, writer)),
                    Err(e) => {
                        log::warn!("streaming {} without caching: {e}", track.key);
                        storage_object
                    }
                }
            } else {
                storage_object
            };
        let (src, handle) = RangeSource::open(object, track.size, &self.rt, self.config.clone());
        Ok(OpenedSource {
            source: Box::new(src),
            ext_hint: ext_of(&track.key),
            buffer: Some(handle),
        })
    }
}

/// Local files: key is a filesystem path relative to `root`. open → std::fs::File (Io error on failure).
pub struct FileOpener {
    root: PathBuf,
}

impl FileOpener {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl TrackOpener for FileOpener {
    /// Blocking file open; must only run on the decode thread (never the controller or a tokio worker).
    fn open(&self, track: &TrackRef) -> Result<OpenedSource> {
        let file = std::fs::File::open(self.root.join(&track.key))?;
        Ok(OpenedSource {
            source: Box::new(file),
            ext_hint: ext_of(&track.key),
            buffer: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::TrackDecoder;
    use sonorus_storage::StorageConfig;

    #[test]
    fn ext_of_cases() {
        assert_eq!(ext_of("A/b/01 - x.FLAC").as_deref(), Some("flac"));
        assert_eq!(ext_of("noext"), None);
        assert_eq!(ext_of("a.b/track"), None);
        assert_eq!(ext_of("trailing."), None);
    }

    #[test]
    fn file_opener_probes_96k_fixture() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let opener = FileOpener::new(root);
        let track = TrackRef {
            id: "tone".to_string(),
            key: "tone_24_96.flac".to_string(),
            size: 0,
            gain: None,
        };
        let opened = opener.open(&track).unwrap();
        assert_eq!(opened.ext_hint.as_deref(), Some("flac"));
        assert!(opened.buffer.is_none());
        let decoder = TrackDecoder::new(opened.source, opened.ext_hint.as_deref()).unwrap();
        assert_eq!(decoder.info().sample_rate, 96_000);
    }

    #[test]
    fn caching_opener_serves_complete_entry_locally() {
        use crate::cache::testing::TempDir;
        use std::io::Read;

        let dir = TempDir::new();
        let cache = TrackCache::open(dir.path(), u64::MAX, None).unwrap();
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let key = "music/tone.flac";
        let size = data.len() as u64;
        let writer = cache.writer(key, size).unwrap();
        writer.write_chunk(0, &data).unwrap();
        assert!(writer.is_complete());
        drop(writer);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let storage = Storage::new(&StorageConfig::backblaze_b2(
            "us-west-004",
            "bucket",
            "key-id",
            "secret",
        ))
        .unwrap();
        let opener =
            CachingOpener::new(cache, storage, rt.handle().clone(), SourceConfig::default());
        let track = TrackRef {
            id: "tone".to_string(),
            key: key.to_string(),
            size,
            gain: None,
        };
        let mut opened = opener.open(&track).unwrap();
        assert!(opened.buffer.is_none());
        assert_eq!(opened.ext_hint.as_deref(), Some("flac"));
        let mut out = Vec::new();
        opened.source.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }
}
