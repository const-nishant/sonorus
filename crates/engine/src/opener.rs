//! Track openers: turn a [`TrackRef`] into a byte source the decoder can read.

use std::path::PathBuf;
use std::sync::Arc;

use symphonia::core::io::MediaSource;

use crate::error::Result;
use crate::source::{BufferHandle, RangeSource, SourceConfig, StorageObject};
use sonorus_storage::Storage;

/// A track to open. `key` is the storage object key (or path relative to a local root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRef {
    pub id: String,
    pub key: String,
    pub size: u64,
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
        };
        let opened = opener.open(&track).unwrap();
        assert_eq!(opened.ext_hint.as_deref(), Some("flac"));
        assert!(opened.buffer.is_none());
        let decoder = TrackDecoder::new(opened.source, opened.ext_hint.as_deref()).unwrap();
        assert_eq!(decoder.info().sample_rate, 96_000);
    }
}
