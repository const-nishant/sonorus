use std::time::Duration;

use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::config::{
    BehaviorVersion, Credentials, Region, RequestChecksumCalculation, ResponseChecksumValidation,
};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;

use crate::config::StorageConfig;
use crate::error::{Result, StorageError, from_sdk};

/// Handle to one bucket on an S3-compatible object store.
#[derive(Clone)]
pub struct Storage {
    pub(crate) client: aws_sdk_s3::Client,
    pub(crate) bucket: String,
}

/// Summary of one object as returned by a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
    pub etag: Option<String>,
    /// Unix seconds.
    pub last_modified: Option<i64>,
}

/// Metadata of one object as returned by a HEAD request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    pub key: String,
    pub size: u64,
    pub etag: Option<String>,
    pub content_type: Option<String>,
    /// Unix seconds.
    pub last_modified: Option<i64>,
}

/// Audio file extensions recognized by [`is_audio_key`] (lowercase, without the dot).
pub const AUDIO_EXTENSIONS: &[&str] = &["flac", "wav", "m4a", "alac", "aif", "aiff"];

/// Returns true when the key's final path segment has an extension in [`AUDIO_EXTENSIONS`]
/// (case-insensitive). Keys ending with `/` (directory markers) are never audio.
pub fn is_audio_key(key: &str) -> bool {
    if key.ends_with('/') {
        return false;
    }
    match extension(key) {
        Some(ext) => AUDIO_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(ext)),
        None => false,
    }
}

/// Maps a key's extension to a MIME type (case-insensitive); unknown → `application/octet-stream`.
pub fn content_type_for_key(key: &str) -> &'static str {
    let ext = extension(key).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "m4a" | "alac" => "audio/mp4",
        "aif" | "aiff" => "audio/aiff",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "json" => "application/json",
        "lrc" | "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Text after the last '.' in the final path segment; None if there is no '.'.
fn extension(key: &str) -> Option<&str> {
    let segment = key.rsplit('/').next().unwrap_or(key);
    segment.rsplit_once('.').map(|(_, ext)| ext)
}

fn clamp_size(size: Option<i64>) -> u64 {
    size.unwrap_or(0).max(0) as u64
}

const MIN_PRESIGN: Duration = Duration::from_secs(1);
const MAX_PRESIGN: Duration = Duration::from_secs(7 * 24 * 3600);

impl Storage {
    /// Validates `config` and builds the S3 client directly (no aws-config crate).
    /// Synchronous; does not require a running Tokio runtime.
    pub fn new(config: &StorageConfig) -> Result<Self> {
        config.validate()?;
        let endpoint = config.endpoint.trim().trim_end_matches('/');
        let credentials = Credentials::new(
            config.access_key_id.clone(),
            config.secret_access_key.clone(),
            None,
            None,
            "sonorus-env",
        );
        let s3_config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            .endpoint_url(endpoint)
            .credentials_provider(credentials)
            .force_path_style(config.path_style)
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
            .timeout_config(
                TimeoutConfig::builder()
                    .connect_timeout(Duration::from_secs(10))
                    .read_timeout(Duration::from_secs(30))
                    .build(),
            )
            .build();
        Ok(Self {
            client: aws_sdk_s3::Client::from_conf(s3_config),
            bucket: config.bucket.clone(),
        })
    }

    /// Name of the bucket this handle operates on.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Lists all objects under `prefix` (ListObjectsV2, paginated to completion).
    /// Keys ending with `/` are skipped.
    pub async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>> {
        let mut objects = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);
            if let Some(t) = token.take() {
                request = request.continuation_token(t);
            }
            let output = request.send().await.map_err(|e| from_sdk(e, prefix))?;
            for object in output.contents() {
                let Some(key) = object.key() else { continue };
                if key.ends_with('/') {
                    continue;
                }
                objects.push(ObjectInfo {
                    key: key.to_string(),
                    size: clamp_size(object.size()),
                    etag: object.e_tag().map(str::to_string),
                    last_modified: object.last_modified().map(|d| d.secs()),
                });
            }
            match (output.is_truncated(), output.next_continuation_token()) {
                (Some(true), Some(next)) if !next.is_empty() => token = Some(next.to_string()),
                _ => break,
            }
        }
        Ok(objects)
    }

    /// [`Storage::list`] filtered to audio keys per [`is_audio_key`].
    pub async fn list_audio(&self, prefix: &str) -> Result<Vec<ObjectInfo>> {
        let mut objects = self.list(prefix).await?;
        objects.retain(|o| is_audio_key(&o.key));
        Ok(objects)
    }

    /// HEAD request for one object. A missing key → `StorageError::NotFound`.
    pub async fn head(&self, key: &str) -> Result<ObjectMeta> {
        let output = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| from_sdk(e, key))?;
        Ok(ObjectMeta {
            key: key.to_string(),
            size: clamp_size(output.content_length()),
            etag: output.e_tag().map(str::to_string),
            content_type: output.content_type().map(str::to_string),
            last_modified: output.last_modified().map(|d| d.secs()),
        })
    }

    /// Fetches `len` bytes starting at `start` (HTTP Range `bytes=start-(start+len-1)`).
    /// `len == 0` returns empty without a request. HTTP 416 (start at or past EOF) returns empty.
    /// May return fewer than `len` bytes at EOF.
    pub async fn get_range(&self, key: &str, start: u64, len: u64) -> Result<Bytes> {
        if len == 0 {
            return Ok(Bytes::new());
        }
        let end = start.saturating_add(len - 1);
        let result = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .range(format!("bytes={start}-{end}"))
            .send()
            .await;
        let output = match result {
            Ok(output) => output,
            Err(err) => {
                if err.raw_response().map(|r| r.status().as_u16()) == Some(416) {
                    return Ok(Bytes::new());
                }
                return Err(from_sdk(err, key));
            }
        };
        let body = output
            .body
            .collect()
            .await
            .map_err(|e| StorageError::Network(e.to_string()))?;
        Ok(body.into_bytes())
    }

    /// Fetches the whole object.
    pub async fn get_bytes(&self, key: &str) -> Result<Bytes> {
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| from_sdk(e, key))?;
        let body = output
            .body
            .collect()
            .await
            .map_err(|e| StorageError::Network(e.to_string()))?;
        Ok(body.into_bytes())
    }

    /// Uploads `data` as a single PUT. `content_type` None → [`content_type_for_key`].
    pub async fn put_bytes(
        &self,
        key: &str,
        data: Bytes,
        content_type: Option<&str>,
    ) -> Result<()> {
        let content_type = content_type.unwrap_or_else(|| content_type_for_key(key));
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .body(ByteStream::from(data))
            .send()
            .await
            .map_err(|e| from_sdk(e, key))?;
        Ok(())
    }

    /// Returns a presigned GET URL valid for `expires`, clamped to 1 s ..= 7 days.
    /// The URL is a credential; never log it.
    pub async fn presign_get(&self, key: &str, expires: Duration) -> Result<String> {
        let expires = expires.clamp(MIN_PRESIGN, MAX_PRESIGN);
        let presign_config = PresigningConfig::expires_in(expires)
            .map_err(|e| StorageError::InvalidConfig(e.to_string()))?;
        let request = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(presign_config)
            .await
            .map_err(|e| from_sdk(e, key))?;
        Ok(request.uri().to_string())
    }

    /// Deletes one object. Deleting a missing key is Ok.
    pub async fn delete(&self, key: &str) -> Result<()> {
        match self
            .client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(err) => match from_sdk(err, key) {
                StorageError::NotFound(_) => Ok(()),
                other => Err(other),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_keys_are_detected_case_insensitively() {
        assert!(is_audio_key("A/B/01 - x.FLAC"));
        assert!(is_audio_key("track.m4a"));
        assert!(is_audio_key("x/y.AIFF"));
    }

    #[test]
    fn non_audio_keys_are_rejected() {
        assert!(!is_audio_key("a/cover.jpg"));
        assert!(!is_audio_key("dir/"));
        assert!(!is_audio_key("noext"));
        assert!(!is_audio_key("a.b/track"));
        assert!(!is_audio_key("a/track.mp3"));
    }

    #[test]
    fn content_types_map_by_extension() {
        assert_eq!(content_type_for_key("a/b.flac"), "audio/flac");
        assert_eq!(content_type_for_key("a/b.WAV"), "audio/wav");
        assert_eq!(content_type_for_key("a/b.m4a"), "audio/mp4");
        assert_eq!(content_type_for_key("a/b.alac"), "audio/mp4");
        assert_eq!(content_type_for_key("a/b.aif"), "audio/aiff");
        assert_eq!(content_type_for_key("a/b.aiff"), "audio/aiff");
        assert_eq!(content_type_for_key("a/cover.jpg"), "image/jpeg");
        assert_eq!(content_type_for_key("a/cover.JPEG"), "image/jpeg");
        assert_eq!(content_type_for_key("a/cover.png"), "image/png");
        assert_eq!(content_type_for_key("a/meta.json"), "application/json");
        assert_eq!(content_type_for_key("a/x.lrc"), "text/plain");
        assert_eq!(content_type_for_key("a/x.txt"), "text/plain");
        assert_eq!(content_type_for_key("a/x.bin"), "application/octet-stream");
        assert_eq!(content_type_for_key("noext"), "application/octet-stream");
    }

    #[test]
    fn new_builds_without_runtime_and_reports_bucket() {
        let config = StorageConfig::backblaze_b2("us-west-004", "music", "AKIAEXAMPLE1", "secret");
        let storage = Storage::new(&config).expect("client should build outside a runtime");
        assert_eq!(storage.bucket(), "music");
    }

    #[test]
    fn new_rejects_invalid_config() {
        let mut config =
            StorageConfig::backblaze_b2("us-west-004", "music", "AKIAEXAMPLE1", "secret");
        config.bucket = "  ".to_string();
        assert!(matches!(
            Storage::new(&config),
            Err(StorageError::InvalidConfig(_))
        ));
    }
}
