//! The error type every exported `sonorus-core` method returns, and its conversions from the
//! lower crates' errors.
//!
//! Messages never contain credentials: the source error types do not carry them, and configs
//! are never formatted into messages.

use sonorus_engine::EngineError;
use sonorus_library::LibraryError;
use sonorus_storage::StorageError;

/// Failure reported to the Android shell. Each variant carries a human-readable `detail` (not `message`: that
/// would clash with `Throwable.message` in the generated Kotlin exception classes).
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    /// No bucket settings were supplied, so the bucket backend is unavailable.
    #[error("the bucket is not configured")]
    NotConfigured,
    /// Settings were rejected (missing or malformed field, bad endpoint).
    #[error("invalid configuration: {detail}")]
    InvalidConfig {
        /// What is wrong, without any secret values.
        detail: String,
    },
    /// The bucket refused the credentials (wrong key id or secret, or no access to the bucket).
    #[error("access denied: {detail}")]
    AccessDenied {
        /// Display text of the service's answer, without any secret values.
        detail: String,
    },
    /// A requested row or object does not exist.
    #[error("not found: {detail}")]
    NotFound {
        /// The missing item (track id, key, playlist id, ...).
        detail: String,
    },
    /// The bucket or local storage failed.
    #[error("storage error: {detail}")]
    Storage {
        /// Display text of the underlying error.
        detail: String,
    },
    /// The catalogue database failed.
    #[error("library error: {detail}")]
    Library {
        /// Display text of the underlying error.
        detail: String,
    },
    /// A remote user-data document is corrupt or too new, so sync was refused.
    #[error("sync error: {detail}")]
    Sync {
        /// Display text of the underlying error.
        detail: String,
    },
    /// The operation cannot run right now (for example, a scan is already in progress).
    #[error("busy: {detail}")]
    Busy {
        /// Why the operation was refused.
        detail: String,
    },
    /// An unexpected failure inside the core (playback engine, I/O, or a bug).
    #[error("internal error: {detail}")]
    Internal {
        /// Display text of the underlying error.
        detail: String,
    },
}

/// Result alias used by every exported `sonorus-core` function.
pub type CoreResult<T> = std::result::Result<T, CoreError>;

impl From<StorageError> for CoreError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::MissingEnv(_) | StorageError::InvalidConfig(_) => {
                CoreError::InvalidConfig {
                    detail: e.to_string(),
                }
            }
            StorageError::NotFound(key) => CoreError::NotFound { detail: key },
            StorageError::AccessDenied(detail) => CoreError::AccessDenied { detail },
            other => CoreError::Storage {
                detail: other.to_string(),
            },
        }
    }
}

impl From<LibraryError> for CoreError {
    fn from(e: LibraryError) -> Self {
        match e {
            LibraryError::NotFound(detail) => CoreError::NotFound { detail },
            LibraryError::Storage(e) => e.into(),
            LibraryError::Sync(detail) => CoreError::Sync { detail },
            other => CoreError::Library {
                detail: other.to_string(),
            },
        }
    }
}

impl From<EngineError> for CoreError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::Storage(e) => e.into(),
            other => CoreError::Internal {
                detail: other.to_string(),
            },
        }
    }
}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::Internal {
            detail: e.to_string(),
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
            CoreError::NotFound { ref detail } if detail == "music/a.flac"
        ));
    }

    #[test]
    fn storage_access_denied_maps_to_access_denied() {
        let e: CoreError = StorageError::AccessDenied("SignatureDoesNotMatch".into()).into();
        assert!(matches!(
            e,
            CoreError::AccessDenied { ref detail } if detail == "SignatureDoesNotMatch"
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
        assert!(matches!(e, CoreError::Storage { ref detail } if detail.contains("timeout")));
    }

    #[test]
    fn library_sync_maps_to_sync() {
        let e: CoreError = LibraryError::Sync("bad json".into()).into();
        assert!(matches!(e, CoreError::Sync { ref detail } if detail == "bad json"));
    }

    #[test]
    fn library_not_found_and_storage_pass_through() {
        let e: CoreError = LibraryError::NotFound("track 9".into()).into();
        assert!(matches!(e, CoreError::NotFound { ref detail } if detail == "track 9"));
        let e: CoreError = LibraryError::Storage(StorageError::NotFound("k".into())).into();
        assert!(matches!(e, CoreError::NotFound { ref detail } if detail == "k"));
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
        assert!(matches!(e, CoreError::Internal { ref detail } if detail.contains("disk gone")));
    }
}
