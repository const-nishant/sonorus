//! Bucket management calls: one-level folder listings, server-side copies and batch deletes.

use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Output;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};

use crate::client::{ObjectInfo, ObjectMeta, Storage, content_type_for_key};
use crate::error::{Result, StorageError, from_sdk};
use crate::upload::UploadedPart;

/// Largest object CopyObject accepts (5 GiB). Larger sources are copied by multipart
/// UploadPartCopy.
pub(crate) const COPY_OBJECT_MAX: u64 = 5 * 1024 * 1024 * 1024;
/// Size of every UploadPartCopy part except possibly the last (512 MiB).
pub(crate) const COPY_PART_SIZE: u64 = 512 * 1024 * 1024;
/// Most keys one DeleteObjects request may name.
const DELETE_BATCH: usize = 1000;
/// Hex digits for percent-encoding.
const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// The direct contents of one folder: its child folders and the objects directly in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderListing {
    /// Child folder prefixes. Each one starts with the listed prefix and ends with `/`.
    pub folders: Vec<String>,
    /// Objects directly in the folder, not in its subfolders. Directory markers are skipped.
    pub objects: Vec<ObjectInfo>,
}

/// The value of the `x-amz-copy-source` header for `key` in `bucket`: the bucket name, a `/`,
/// then the key with every byte outside `A-Za-z0-9-_.~/` percent-encoded as `%XX` (uppercase).
/// Keys with spaces, `&`, `#`, `%`, `+` or non-ASCII text therefore survive the copy.
pub(crate) fn copy_source(bucket: &str, key: &str) -> String {
    let mut out = String::with_capacity(bucket.len() + key.len() + 1);
    out.push_str(bucket);
    out.push('/');
    for &byte in key.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX_UPPER[usize::from(byte >> 4)]));
            out.push(char::from(HEX_UPPER[usize::from(byte & 0x0F)]));
        }
    }
    out
}

/// Adds one ListObjectsV2 page (listed with delimiter `/` under `prefix`) to `listing`. Directory
/// markers, which are keys ending with `/`, are skipped, and so is the listed prefix itself.
fn absorb_page(listing: &mut FolderListing, prefix: &str, output: &ListObjectsV2Output) {
    for common in output.common_prefixes() {
        let Some(folder) = common.prefix() else {
            continue;
        };
        if folder != prefix && folder.ends_with('/') {
            listing.folders.push(folder.to_owned());
        }
    }
    for object in output.contents() {
        let Some(key) = object.key() else {
            continue;
        };
        if key.ends_with('/') {
            continue;
        }
        listing.objects.push(ObjectInfo {
            key: key.to_owned(),
            size: object.size().unwrap_or(0).max(0) as u64,
            etag: object.e_tag().map(str::to_owned),
            last_modified: object.last_modified().map(|d| d.secs()),
        });
    }
}

impl Storage {
    /// Lists the direct contents of the folder `prefix` (ListObjectsV2 with delimiter `/`,
    /// paginated to completion). Use `""` for the bucket root.
    pub async fn list_folder(&self, prefix: &str) -> Result<FolderListing> {
        let mut listing = FolderListing::default();
        let mut token: Option<String> = None;
        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix)
                .delimiter("/");
            if let Some(t) = token.take() {
                request = request.continuation_token(t);
            }
            let output = request.send().await.map_err(|e| from_sdk(e, prefix))?;
            absorb_page(&mut listing, prefix, &output);
            match (output.is_truncated(), output.next_continuation_token()) {
                (Some(true), Some(next)) if !next.is_empty() => token = Some(next.to_string()),
                _ => break,
            }
        }
        Ok(listing)
    }

    /// Copies `src` to `dst` on the server. A source of at most 5 GiB uses CopyObject; a larger
    /// one is copied in 512 MiB parts with multipart UploadPartCopy, keeping its content type and
    /// user metadata. A missing source is `StorageError::NotFound`. Does not check that `dst` is
    /// free: the caller does that, since a copy overwrites.
    pub async fn copy_object(&self, src: &str, dst: &str) -> Result<()> {
        let meta = self.head(src).await?;
        if meta.size <= COPY_OBJECT_MAX {
            self.client
                .copy_object()
                .bucket(&self.bucket)
                .key(dst)
                .copy_source(copy_source(&self.bucket, src))
                .send()
                .await
                .map_err(|e| from_sdk(e, src))?;
            return Ok(());
        }
        self.copy_multipart(src, dst, &meta).await
    }

    /// Multipart copy of `meta.size` bytes from `src` to `dst`. Aborts the upload on failure.
    async fn copy_multipart(&self, src: &str, dst: &str, meta: &ObjectMeta) -> Result<()> {
        let content_type = meta
            .content_type
            .clone()
            .unwrap_or_else(|| content_type_for_key(dst).to_owned());
        let pairs: Vec<(&str, &str)> = meta
            .metadata
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let upload_id = self
            .create_multipart_with_meta(dst, &content_type, &pairs)
            .await?;
        let source = copy_source(&self.bucket, src);

        let outcome: Result<()> = async {
            let mut parts: Vec<UploadedPart> = Vec::new();
            let mut start: u64 = 0;
            let mut part_number: i32 = 1;
            while start < meta.size {
                let end = (start + COPY_PART_SIZE).min(meta.size) - 1;
                let output = self
                    .client
                    .upload_part_copy()
                    .bucket(&self.bucket)
                    .key(dst)
                    .upload_id(&upload_id)
                    .part_number(part_number)
                    .copy_source(&source)
                    .copy_source_range(format!("bytes={start}-{end}"))
                    .send()
                    .await
                    .map_err(|e| from_sdk(e, src))?;
                let etag = output
                    .copy_part_result()
                    .and_then(|r| r.e_tag())
                    .ok_or_else(|| StorageError::S3 {
                        status: None,
                        message: format!("missing ETag for copied part {part_number}"),
                    })?;
                parts.push(UploadedPart {
                    part_number,
                    etag: etag.to_owned(),
                });
                start = end + 1;
                part_number += 1;
            }
            self.complete_multipart(dst, &upload_id, &parts).await
        }
        .await;

        if let Err(e) = outcome {
            if let Err(abort_err) = self.abort_multipart(dst, &upload_id).await {
                log::error!("abort of multipart copy failed: {abort_err}");
            }
            return Err(e);
        }
        Ok(())
    }

    /// Deletes the objects `keys` with batch DeleteObjects (quiet mode, 1000 keys per request).
    /// Returns the keys that could not be deleted, each with the service's message. A failed
    /// request marks every key of its batch as failed. Missing keys are not failures. An empty
    /// slice sends no request.
    pub async fn delete_objects(&self, keys: &[String]) -> Result<Vec<(String, String)>> {
        let mut failed = Vec::new();
        for batch in keys.chunks(DELETE_BATCH) {
            let identifiers = batch
                .iter()
                .map(|key| {
                    ObjectIdentifier::builder()
                        .key(key)
                        .build()
                        .map_err(|e| StorageError::InvalidConfig(format!("bad object key: {e}")))
                })
                .collect::<Result<Vec<_>>>()?;
            let delete = Delete::builder()
                .set_objects(Some(identifiers))
                .quiet(true)
                .build()
                .map_err(|e| StorageError::InvalidConfig(format!("bad delete request: {e}")))?;
            match self
                .client
                .delete_objects()
                .bucket(&self.bucket)
                .delete(delete)
                .send()
                .await
            {
                Ok(output) => {
                    for error in output.errors() {
                        failed.push((
                            error.key().unwrap_or_default().to_owned(),
                            format!(
                                "{}: {}",
                                error.code().unwrap_or("error"),
                                error.message().unwrap_or_default()
                            ),
                        ));
                    }
                }
                Err(err) => {
                    let message = from_sdk(err, "").to_string();
                    for key in batch {
                        failed.push((key.clone(), message.clone()));
                    }
                }
            }
        }
        Ok(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_s3::types::{CommonPrefix, Object};

    #[test]
    fn copy_source_keeps_plain_keys_and_slashes() {
        assert_eq!(
            copy_source("music", "Artist A/Album One/01-x_y.z~w.flac"),
            "music/Artist%20A/Album%20One/01-x_y.z~w.flac"
        );
        assert_eq!(copy_source("music", "a/b.flac"), "music/a/b.flac");
    }

    #[test]
    fn copy_source_escapes_reserved_characters() {
        assert_eq!(copy_source("b", "a b"), "b/a%20b");
        assert_eq!(copy_source("b", "Rock & Roll"), "b/Rock%20%26%20Roll");
        assert_eq!(copy_source("b", "C#/x"), "b/C%23/x");
        assert_eq!(copy_source("b", "100%"), "b/100%25");
        assert_eq!(copy_source("b", "a+b?c"), "b/a%2Bb%3Fc");
    }

    #[test]
    fn copy_source_encodes_unicode_as_utf8_bytes() {
        // "é" is C3 A9 in UTF-8; "日" is E6 97 A5.
        assert_eq!(copy_source("b", "é"), "b/%C3%A9");
        assert_eq!(copy_source("b", "日/x"), "b/%E6%97%A5/x");
    }

    #[test]
    fn copy_source_is_bucket_then_key() {
        assert_eq!(copy_source("bkt", ""), "bkt/");
    }

    #[test]
    fn copy_limits_match_the_s3_limits() {
        assert_eq!(COPY_OBJECT_MAX, 5_368_709_120);
        assert_eq!(COPY_PART_SIZE, 536_870_912);
    }

    fn page(prefixes: &[&str], keys: &[(&str, i64)]) -> ListObjectsV2Output {
        let mut builder = ListObjectsV2Output::builder();
        for p in prefixes {
            builder = builder.common_prefixes(CommonPrefix::builder().prefix(*p).build());
        }
        for (key, size) in keys {
            builder = builder.contents(Object::builder().key(*key).size(*size).build());
        }
        builder.build()
    }

    #[test]
    fn pages_fold_into_one_listing() {
        let mut listing = FolderListing::default();
        absorb_page(
            &mut listing,
            "music/",
            &page(
                &["music/A/", "music/B/"],
                &[("music/cover.jpg", 7), ("music/", 0), ("music/x/", 0)],
            ),
        );
        absorb_page(
            &mut listing,
            "music/",
            &page(&["music/C/"], &[("music/01.flac", 42)]),
        );
        assert_eq!(listing.folders, vec!["music/A/", "music/B/", "music/C/"]);
        let keys: Vec<(&str, u64)> = listing
            .objects
            .iter()
            .map(|o| (o.key.as_str(), o.size))
            .collect();
        assert_eq!(keys, vec![("music/cover.jpg", 7), ("music/01.flac", 42)]);
    }

    #[test]
    fn listed_prefix_itself_is_never_a_folder() {
        let mut listing = FolderListing::default();
        absorb_page(&mut listing, "", &page(&["", "A/"], &[]));
        assert_eq!(listing.folders, vec!["A/"]);
    }
}
