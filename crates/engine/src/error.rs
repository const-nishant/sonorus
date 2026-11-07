//! Error type for the playback engine.

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
}

pub type Result<T> = std::result::Result<T, EngineError>;
