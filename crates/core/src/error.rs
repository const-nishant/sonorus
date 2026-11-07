//! The error type every exported `sonorus-core` method returns, and its conversions from the
//! lower crates' errors.
//!
//! Messages never contain credentials: the source error types do not carry them, and configs
//! are never formatted into messages.

use sonorus_engine::EngineError;
use sonorus_library::LibraryError;
use sonorus_storage::StorageError;

/// Failure reported to the Android shell. Each variant carries a human-readable `message`.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    /// No bucket settings were supplied, so the bucket backend is unavailable.
    #[error("the bucket is not configured")]
    NotConfigured,
    /// Settings were rejected (missing or malformed field, bad endpoint).
    #[error("invalid configuration: {message}")]
    InvalidConfig {
        /// What is wrong, without any secret values.
        message: String,
    },
    /// A requested row or object does not exist.
    #[error("not found: {message}")]
    NotFound {
        /// The missing item (track id, key, playlist id, ...).
        message: String,
    },
    /// The bucket or local storage failed.
    #[error("storage error: {message}")]
    Storage {
        /// Display text of the underlying error.
        message: String,
    },
    /// The catalogue database failed.
    #[error("library error: {message}")]
    Library {
        /// Display text of the underlying error.
        message: String,
    },
    /// A remote user-data document is corrupt or too new, so sync was refused.
    #[error("sync error: {message}")]
    Sync {
        /// Display text of the underlying error.
        message: String,
    },
    /// The operation cannot run right now (for example, a scan is already in progress).
    #[error("busy: {message}")]
    Busy {
        /// Why the operation was refused.
        message: String,
    },
    /// An unexpected failure inside the core (playback engine, I/O, or a bug).
    #[error("internal error: {message}")]
    Internal {
        /// Display text of the underlying error.
        message: String,
    },
}

/// Result alias used by every exported `sonorus-core` function.
pub type CoreResult<T> = std::result::Result<T, CoreError>;

impl From<StorageError> for CoreError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::MissingEnv(_) | StorageError::InvalidConfig(_) => {
                CoreError::InvalidConfig {
                    message: e.to_string(),
                }
            }
            StorageError::NotFound(key) => CoreError::NotFound { message: key },
            other => CoreError::Storage {
                message: other.to_string(),
            },
        }
    }
}

impl From<LibraryError> for CoreError {
    fn from(e: LibraryError) -> Self {
        match e {
            LibraryError::NotFound(message) => CoreError::NotFound { message },
            LibraryError::Storage(e) => e.into(),
            LibraryError::Sync(message) => CoreError::Sync { message },
            other => CoreError::Library {
                message: other.to_string(),
            },
        }
    }
}

impl From<EngineError> for CoreError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::Storage(e) => e.into(),
            other => CoreError::Internal {
                message: other.to_string(),
            },
        }
    }
}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::Internal {
            message: e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_not_found_maps_to_not_found() {
        let e: CoreError = StorageError::NotFound("music/a.flac".into()).into();
        assert!(matches!(
            e,
            CoreError::NotFound { ref message } if message == "music/a.flac"
        ));
    }

    #[test]
    fn storage_config_errors_map_to_invalid_config() {
        let e: CoreError = StorageError::MissingEnv("SONORUS_S3_BUCKET").into();
        assert!(matches!(e, CoreError::InvalidConfig { .. }));
        let e: CoreError = StorageError::InvalidConfig("endpoint".into()).into();
        assert!(matches!(e, CoreError::InvalidConfig { .. }));
    }

    #[test]
    fn other_storage_errors_map_to_storage() {
        let e: CoreError = StorageError::Network("timeout".into()).into();
        assert!(matches!(e, CoreError::Storage { ref message } if message.contains("timeout")));
    }

    #[test]
    fn library_sync_maps_to_sync() {
        let e: CoreError = LibraryError::Sync("bad json".into()).into();
        assert!(matches!(e, CoreError::Sync { ref message } if message == "bad json"));
    }

    #[test]
    fn library_not_found_and_storage_pass_through() {
        let e: CoreError = LibraryError::NotFound("track 9".into()).into();
        assert!(matches!(e, CoreError::NotFound { ref message } if message == "track 9"));
        let e: CoreError = LibraryError::Storage(StorageError::NotFound("k".into())).into();
        assert!(matches!(e, CoreError::NotFound { ref message } if message == "k"));
    }

    #[test]
    fn engine_storage_maps_through_storage_and_rest_is_internal() {
        let e: CoreError = EngineError::Storage(StorageError::MissingEnv("X")).into();
        assert!(matches!(e, CoreError::InvalidConfig { .. }));
        let e: CoreError = EngineError::Decode("bad frame".into()).into();
        assert!(matches!(e, CoreError::Internal { .. }));
    }

    #[test]
    fn io_error_maps_to_internal() {
        let e: CoreError = std::io::Error::other("disk gone").into();
        assert!(matches!(e, CoreError::Internal { ref message } if message.contains("disk gone")));
    }
}
