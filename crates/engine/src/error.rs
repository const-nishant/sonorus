//! Error type for the playback engine.

use std::io::ErrorKind;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("unsupported stream: {0}")]
    Unsupported(String),
    #[error("no audio track")]
    NoAudioTrack,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("seek error: {0}")]
    Seek(String),
    #[error(transparent)]
    Storage(#[from] sonorus_storage::StorageError),
    #[error("cache error: {0}")]
    Cache(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

impl EngineError {
    /// Whether a later attempt at the same operation may succeed: a network-style I/O failure
    /// (timeout, interruption, reset, abort, not connected) or a retryable storage error.
    /// `BrokenPipe` is not transient: it means the source was closed on our side.
    pub fn is_transient(&self) -> bool {
        match self {
            EngineError::Io(e) => is_transient_io_kind(e.kind()),
            EngineError::Storage(e) => e.is_retryable(),
            _ => false,
        }
    }
}

/// Network-style I/O failures that a later attempt may get past (see [`EngineError::is_transient`]).
pub(crate) fn is_transient_io_kind(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::TimedOut
            | ErrorKind::Interrupted
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::NotConnected
    )
}
