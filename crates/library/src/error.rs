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
}

pub type Result<T> = std::result::Result<T, LibraryError>;
