use aws_sdk_s3::error::{DisplayErrorContext, SdkError};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("missing environment variable {0}")]
    MissingEnv(&'static str),
    #[error("invalid storage config: {0}")]
    InvalidConfig(String),
    #[error("object not found: {0}")]
    NotFound(String),
    #[error("network error: {0}")]
    Network(String),
    /// The service refused the credentials or the request (HTTP 401/403): wrong key id or secret,
    /// a key without access to the bucket, or a region/endpoint mismatch.
    #[error("access denied: {0}")]
    AccessDenied(String),
    #[error("S3 error (status {status:?}): {message}")]
    S3 {
        status: Option<u16>,
        message: String,
    },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("upload failed and was aborted: {0}")]
    UploadAborted(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;

impl StorageError {
    /// Network → true; S3 with status None, 408, 429, or 500..=599 → true; everything else false.
    pub fn is_retryable(&self) -> bool {
        match self {
            StorageError::Network(_) => true,
            StorageError::S3 { status, .. } => match status {
                None | Some(408 | 429) => true,
                Some(s) => (500..=599).contains(s),
            },
            _ => false,
        }
    }
}

/// Map any SDK error. Status 404 → NotFound(key). Status 401/403 → AccessDenied. TimeoutError/DispatchFailure/ResponseError → Network.
/// ServiceError/ConstructionFailure/other → S3{status from raw_response, message = DisplayErrorContext}.
pub(crate) fn from_sdk<E>(err: SdkError<E>, key: &str) -> StorageError
where
    E: std::error::Error + Send + Sync + 'static,
{
    let status = err.raw_response().map(|r| r.status().as_u16());
    if status == Some(404) {
        return StorageError::NotFound(key.to_string());
    }
    if matches!(status, Some(401 | 403)) {
        return StorageError::AccessDenied(DisplayErrorContext(&err).to_string());
    }
    match &err {
        SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) | SdkError::ResponseError(_) => {
            StorageError::Network(DisplayErrorContext(&err).to_string())
        }
        _ => StorageError::S3 {
            status,
            message: DisplayErrorContext(&err).to_string(),
        },
    }
}
