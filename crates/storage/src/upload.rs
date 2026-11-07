use std::path::Path;
use std::time::Duration;

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use bytes::Bytes;
use tokio::io::AsyncReadExt;

use crate::client::{Storage, content_type_for_key};
use crate::error::{Result, StorageError, from_sdk};

/// Size of every multipart part except possibly the last (8 MiB).
pub const PART_SIZE: usize = 8 * 1024 * 1024;
/// Maximum number of attempts per part for retryable errors.
pub const MAX_PART_ATTEMPTS: u32 = 4;

/// Progress snapshot passed to the `upload_file` callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadProgress {
    pub bytes_uploaded: u64,
    pub total_bytes: u64,
    pub parts_done: u32,
    pub parts_total: u32,
}

/// One successfully uploaded multipart part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedPart {
    pub part_number: i32,
    pub etag: String,
}

/// Number of `PART_SIZE` parts needed to hold `size` bytes (ceiling division).
fn part_count(size: u64) -> u32 {
    size.div_ceil(PART_SIZE as u64) as u32
}

impl Storage {
    /// Starts a multipart upload and returns its upload id.
    pub async fn create_multipart(&self, key: &str, content_type: &str) -> Result<String> {
        let output = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .send()
            .await
            .map_err(|e| from_sdk(e, key))?;
        output
            .upload_id()
            .map(str::to_string)
            .ok_or_else(|| StorageError::S3 {
                status: None,
                message: "missing upload id".into(),
            })
    }

    /// Uploads one part. Retries up to `MAX_PART_ATTEMPTS` while the error is retryable,
    /// sleeping 500 ms * 2^(attempt-1) plus up to 250 ms of jitter between attempts.
    /// A response without an ETag is an S3 error and is not retried.
    pub async fn upload_part(
        &self,
        key: &str,
        upload_id: &str,
        part_number: i32,
        data: Bytes,
    ) -> Result<UploadedPart> {
        for attempt in 1..=MAX_PART_ATTEMPTS {
            let result = self
                .client
                .upload_part()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(upload_id)
                .part_number(part_number)
                .content_length(data.len() as i64)
                .body(ByteStream::from(data.clone()))
                .send()
                .await;
            match result {
                Ok(output) => {
                    return match output.e_tag() {
                        Some(etag) => Ok(UploadedPart {
                            part_number,
                            etag: etag.to_string(),
                        }),
                        None => Err(StorageError::S3 {
                            status: None,
                            message: format!("missing ETag for part {part_number}"),
                        }),
                    };
                }
                Err(err) => {
                    let e = from_sdk(err, key);
                    if !e.is_retryable() || attempt == MAX_PART_ATTEMPTS {
                        return Err(e);
                    }
                    log::warn!("upload part {part_number} attempt {attempt} failed: {e}; retrying");
                    let backoff = 500 * 2u64.pow(attempt - 1) + fastrand::u64(0..250);
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                }
            }
        }
        Err(StorageError::S3 {
            status: None,
            message: format!("part {part_number} exhausted retry attempts"),
        })
    }

    /// Completes a multipart upload. `parts` may be in any order; they are sorted by part number.
    /// An empty slice is rejected without a request.
    pub async fn complete_multipart(
        &self,
        key: &str,
        upload_id: &str,
        parts: &[UploadedPart],
    ) -> Result<()> {
        if parts.is_empty() {
            return Err(StorageError::InvalidConfig("no parts to complete".into()));
        }
        let mut sorted = parts.to_vec();
        sorted.sort_by_key(|p| p.part_number);
        let completed: Vec<CompletedPart> = sorted
            .into_iter()
            .map(|p| {
                CompletedPart::builder()
                    .part_number(p.part_number)
                    .e_tag(p.etag)
                    .build()
            })
            .collect();
        let multipart = CompletedMultipartUpload::builder()
            .set_parts(Some(completed))
            .build();
        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(multipart)
            .send()
            .await
            .map_err(|e| from_sdk(e, key))?;
        Ok(())
    }

    /// Aborts a multipart upload, discarding any uploaded parts.
    pub async fn abort_multipart(&self, key: &str, upload_id: &str) -> Result<()> {
        self.client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
            .map_err(|e| from_sdk(e, key))?;
        Ok(())
    }

    /// Uploads a local file. Files of at most `PART_SIZE` bytes use a single PUT; larger files
    /// use multipart upload, reading one part at a time. `on_progress` is called before the first
    /// part and after each part. Any failure after the multipart upload starts aborts it and
    /// returns `StorageError::UploadAborted`. `content_type` None uses `content_type_for_key`.
    pub async fn upload_file<F>(
        &self,
        path: &Path,
        key: &str,
        content_type: Option<&str>,
        mut on_progress: F,
    ) -> Result<()>
    where
        F: FnMut(UploadProgress) + Send,
    {
        let ct = content_type.unwrap_or_else(|| content_type_for_key(key));
        let size = tokio::fs::metadata(path).await?.len();

        if size <= PART_SIZE as u64 {
            let data = tokio::fs::read(path).await?;
            self.put_bytes(key, Bytes::from(data), Some(ct)).await?;
            on_progress(UploadProgress {
                bytes_uploaded: size,
                total_bytes: size,
                parts_done: 1,
                parts_total: 1,
            });
            return Ok(());
        }

        let parts_total = part_count(size);
        on_progress(UploadProgress {
            bytes_uploaded: 0,
            total_bytes: size,
            parts_done: 0,
            parts_total,
        });
        let upload_id = self.create_multipart(key, ct).await?;

        let outcome = match self
            .upload_parts(key, &upload_id, path, size, parts_total, &mut on_progress)
            .await
        {
            Ok(parts) => self.complete_multipart(key, &upload_id, &parts).await,
            Err(e) => Err(e),
        };

        match outcome {
            Ok(()) => Ok(()),
            Err(e) => {
                if let Err(abort_err) = self.abort_multipart(key, &upload_id).await {
                    log::error!("abort of multipart upload for {key} failed: {abort_err}");
                }
                Err(StorageError::UploadAborted(e.to_string()))
            }
        }
    }

    /// Reads `path` sequentially in `PART_SIZE` chunks and uploads each as one part.
    async fn upload_parts<F>(
        &self,
        key: &str,
        upload_id: &str,
        path: &Path,
        size: u64,
        parts_total: u32,
        on_progress: &mut F,
    ) -> Result<Vec<UploadedPart>>
    where
        F: FnMut(UploadProgress) + Send,
    {
        let mut file = tokio::fs::File::open(path).await?;
        let mut parts = Vec::with_capacity(parts_total as usize);
        let mut bytes_uploaded: u64 = 0;

        for part_number in 1..=(parts_total as i32) {
            let mut buf = vec![0u8; PART_SIZE];
            let mut filled = 0usize;
            while filled < PART_SIZE {
                let n = file.read(&mut buf[filled..]).await?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            buf.truncate(filled);
            if buf.is_empty() {
                break;
            }

            let len = buf.len() as u64;
            let part = self
                .upload_part(key, upload_id, part_number, Bytes::from(buf))
                .await?;
            parts.push(part);
            bytes_uploaded += len;
            on_progress(UploadProgress {
                bytes_uploaded,
                total_bytes: size,
                parts_done: parts.len() as u32,
                parts_total,
            });
        }

        Ok(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_size_is_eight_mib() {
        assert_eq!(PART_SIZE, 8 * 1024 * 1024);
    }

    #[test]
    fn max_part_attempts_is_four() {
        assert_eq!(MAX_PART_ATTEMPTS, 4);
    }

    #[test]
    fn part_count_rounds_up() {
        let mib = 1024 * 1024;
        assert_eq!(part_count(8 * mib + 1), 2);
        assert_eq!(part_count(16 * mib), 2);
        assert_eq!(part_count(16 * mib + 1), 3);
    }
}
