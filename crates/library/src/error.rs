//! Error type for the library crate.

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tag error: {0}")]
    Tags(String),
    #[error(transparent)]
    Storage(#[from] sonorus_storage::StorageError),
    #[error("not found: {0}")]
    NotFound(String),
    /// A caller-supplied path is not a valid catalogue folder path (absolute, or with `..`).
    #[error("invalid folder path: {0}")]
    InvalidPath(String),
    /// A remote user-data document is corrupt or uses a newer format; it is never overwritten.
    #[error("sync error: {0}")]
    Sync(String),
}

pub type Result<T> = std::result::Result<T, LibraryError>;
