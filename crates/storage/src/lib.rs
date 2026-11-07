//! S3-compatible object storage client for Sonorus (Backblaze B2, Cloudflare R2, ...).

mod client;
mod config;
mod error;
mod tls;
mod upload;

pub use client::{
    AUDIO_EXTENSIONS, ObjectInfo, ObjectMeta, Storage, content_type_for_key, is_audio_key,
};
pub use config::{StorageConfig, normalize_prefix, region_from_endpoint};
pub use error::{Result, StorageError};
pub use upload::{MAX_PART_ATTEMPTS, PART_SIZE, UploadProgress, UploadedPart};
