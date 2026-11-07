//! The upload pipeline behind the `SonorusCore::upload_*` methods: reading tags and hashing a file
//! that the Android shell hands over as a descriptor, choosing its object key, and the resumable
//! multipart upload.
//!
//! Everything here is blocking. Tag reading and hashing run on the calling thread; only the S3
//! calls go through the core runtime (`block_on`).

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
use std::sync::{Mutex, MutexGuard, PoisonError};

use bytes::Bytes;
use sonorus_library::uploads::{self, NewUpload, UploadRow, UploadState};
use sonorus_library::{Library, TrackMeta, tags};
use sonorus_storage::{
    ObjectMeta, PART_SIZE, Storage, StorageError, UploadedPart, content_type_for_key, is_audio_key,
};
use tokio::runtime::Runtime;

use crate::error::{CoreError, CoreResult};
use crate::events::ListenerSlot;
use crate::types::{CoreEvent, UploadJobInfo};

/// Bytes read per step while hashing a source file.
const HASH_BUFFER: usize = 64 * 1024;
/// Longest object-key path segment (a folder name or a whole file name), in bytes.
pub(crate) const MAX_NAME_BYTES: usize = 120;
/// Object-key candidates tried before giving up on finding a free key.
const MAX_KEY_ATTEMPTS: u32 = 50;
/// Longest error text stored on a job, in bytes.
const MAX_ERROR_BYTES: usize = 200;
/// User-metadata name under which an uploaded object stores the BLAKE3 hash of its content.
pub(crate) const HASH_META: &str = "blake3";

const UNKNOWN_ARTIST: &str = "Unknown Artist";
const UNKNOWN_ALBUM: &str = "Unknown Album";
const UNTITLED: &str = "Untitled";

/// Looks up the object at a key: `None` when the key is free.
pub(crate) type HeadLookup<'a> = dyn FnMut(&str) -> CoreResult<Option<ObjectMeta>> + 'a;

/// Takes ownership of the raw file descriptor `fd`. The returned `File` closes it when dropped,
/// so every error path after this call closes the descriptor too.
#[cfg(unix)]
pub(crate) fn take_fd(fd: i32) -> CoreResult<File> {
    use std::os::fd::FromRawFd;

    if fd < 0 {
        return Err(CoreError::Internal {
            detail: format!("invalid file descriptor {fd}"),
        });
    }
    // SAFETY: the Android shell detached `fd` from its ParcelFileDescriptor (`detachFd`) and
    // hands over ownership with this call. No other owner exists in this process, and the
    // returned `File` is the only owner from now on, so it is closed exactly once.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Uploads need POSIX descriptors; other platforms have none.
#[cfg(not(unix))]
pub(crate) fn take_fd(_fd: i32) -> CoreResult<File> {
    Err(CoreError::Internal {
        detail: "uploads need a unix file descriptor".to_owned(),
    })
}

/// Makes `raw` safe as one path segment. Whitespace runs collapse to one space; illegal path
/// characters (`/ \ : * ? " < > |`) and control characters become `_`; spaces and dots are
/// trimmed from both ends; the result is capped at [`MAX_NAME_BYTES`] on a char boundary.
/// Returns an empty string when nothing is left.
pub(crate) fn sanitize_component(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_whitespace() {
                ' '
            } else if c.is_control()
                || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    let collapsed = mapped.split_whitespace().collect::<Vec<_>>().join(" ");
    trim_edges(truncate_bytes(trim_edges(&collapsed), MAX_NAME_BYTES)).to_owned()
}

/// The longest prefix of `s` that is at most `max` bytes and ends on a char boundary.
fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Removes leading and trailing spaces and dots.
fn trim_edges(s: &str) -> &str {
    s.trim_matches(|c: char| c == ' ' || c == '.')
}

/// Trimmed text, or `None` when it is blank.
fn present(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.trim().is_empty())
}

/// The first candidate that is non-empty after sanitizing, or `fallback`.
fn first_name<'a>(candidates: impl IntoIterator<Item = Option<&'a str>>, fallback: &str) -> String {
    candidates
        .into_iter()
        .flatten()
        .map(sanitize_component)
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

/// The file name `{stem}.{ext}`. `attempt` 2 and up append ` (n)` to the stem. The whole name,
/// suffix and extension included, stays within [`MAX_NAME_BYTES`].
pub(crate) fn file_name(stem: &str, ext: &str, attempt: u32) -> String {
    let suffix = if attempt <= 1 {
        String::new()
    } else {
        format!(" ({attempt})")
    };
    let budget = MAX_NAME_BYTES.saturating_sub(ext.len() + 1 + suffix.len());
    let base = trim_edges(truncate_bytes(stem, budget));
    let base = if base.is_empty() { UNTITLED } else { base };
    format!("{base}{suffix}.{ext}")
}

/// The object key for a track: `{prefix}{Artist}/{Album}/{NN - }{Title}.{ext}`.
///
/// Artist is the album artist, else the artist, else "Unknown Artist". Album is "Unknown Album"
/// when missing. Title is the tag, else the file name without its extension. `NN` is the
/// two-digit track number when known; with more than one disc it is `D-NN`. `prefix` is the
/// configured bucket prefix (normalized, possibly empty). `attempt` 1 is the plain name; see
/// [`file_name`] for the suffixes. `ext` is lowercase and comes from the audio file.
pub(crate) fn object_key(
    prefix: &str,
    meta: &TrackMeta,
    display_name: &str,
    ext: &str,
    attempt: u32,
) -> String {
    let artist = first_name(
        [
            present(meta.album_artist.as_deref()),
            present(meta.artist.as_deref()),
        ],
        UNKNOWN_ARTIST,
    );
    let album = first_name([present(meta.album.as_deref())], UNKNOWN_ALBUM);
    let title = first_name(
        [
            present(meta.title.as_deref()),
            Some(display_stem(display_name)),
        ],
        UNTITLED,
    );
    let number = match meta.track_no {
        Some(track) => {
            let disc = match (meta.disc_total, meta.disc_no) {
                (Some(total), Some(disc)) if total > 1 => format!("{disc}-"),
                _ => String::new(),
            };
            format!("{disc}{track:02} - ")
        }
        None => String::new(),
    };
    let file = file_name(&format!("{number}{title}"), ext, attempt);
    format!("{prefix}{artist}/{album}/{file}")
}

/// The file name without its directory and last extension (`"a/b.c.flac"` gives `"b.c"`).
fn display_stem(display_name: &str) -> &str {
    let name = display_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(display_name);
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

/// The lowercase extension of `display_name` when it names a supported audio file.
fn audio_extension(display_name: &str) -> Option<String> {
    if !is_audio_key(display_name) {
        return None;
    }
    display_name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
}

/// BLAKE3 hash of the whole file as lowercase hex. Rewinds to the start first, then reads in
/// [`HASH_BUFFER`] steps.
pub(crate) fn hash_file(file: &mut File) -> io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; HASH_BUFFER];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Reads the tags and content hash of a local audio file, dedupes it, chooses its object key
/// and adds a queue row for it. Blocking; `head` looks up a key in the bucket.
///
/// The row's state is `Duplicate` when a queued, uploading or done row has the same hash, or
/// when the chosen key already holds the same content. Otherwise it is `Queued`.
pub(crate) fn prepare_job(
    library: &Library,
    prefix: &str,
    file: &mut File,
    source_uri: &str,
    display_name: &str,
    head: &mut HeadLookup<'_>,
) -> CoreResult<UploadJobInfo> {
    let ext = audio_extension(display_name).ok_or_else(|| CoreError::InvalidConfig {
        detail: format!("not a supported audio file: {display_name}"),
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(CoreError::InvalidConfig {
            detail: format!("not a regular file: {display_name}"),
        });
    }
    let size = metadata.len();
    if size == 0 {
        return Err(CoreError::InvalidConfig {
            detail: format!("empty file: {display_name}"),
        });
    }

    let meta = tags::read_meta(&mut *file, Some(&ext)).map_err(|e| CoreError::Library {
        detail: format!("cannot read the tags of {display_name}: {e}"),
    })?;
    file.seek(SeekFrom::Start(0))?;
    let hash = hash_file(file)?;

    let (key, state) = match uploads::find_by_hash(library, &hash)? {
        Some(existing) => (existing.key, UploadState::Duplicate),
        None => choose_key(library, prefix, &meta, display_name, &ext, &hash, head)?,
    };

    let content_type = content_type_for_key(display_name).to_owned();
    let id = uploads::insert(
        library,
        &NewUpload {
            source_uri: source_uri.to_owned(),
            display_name: display_name.to_owned(),
            size,
            blake3: hash,
            key: key.clone(),
            content_type,
            state,
        },
    )?;
    Ok(UploadJobInfo {
        id,
        display_name: display_name.to_owned(),
        key,
        size,
        bytes_done: 0,
        state: state.into(),
        error: None,
        source_uri: source_uri.to_owned(),
    })
}

/// The first free object key for a file with `hash`, with the state it gets: `Duplicate` when
/// the key already holds the same content, else `Queued`. A key that holds other content, or
/// that a queued, uploading or failed job will write, is skipped for the next ` (n)` name.
fn choose_key(
    library: &Library,
    prefix: &str,
    meta: &TrackMeta,
    display_name: &str,
    ext: &str,
    hash: &str,
    head: &mut HeadLookup<'_>,
) -> CoreResult<(String, UploadState)> {
    for attempt in 1..=MAX_KEY_ATTEMPTS {
        let key = object_key(prefix, meta, display_name, ext, attempt);
        match head(&key)? {
            Some(existing) => {
                if existing.metadata.get(HASH_META).is_some_and(|h| h == hash) {
                    return Ok((key, UploadState::Duplicate));
                }
            }
            None => {
                if !uploads::key_is_reserved(library, &key)? {
                    return Ok((key, UploadState::Queued));
                }
            }
        }
    }
    Err(CoreError::Internal {
        detail: format!("no free object key for {display_name}"),
    })
}

/// Fails with `NotFound` when the job does not exist.
pub(crate) fn load(library: &Library, job_id: i64) -> CoreResult<UploadRow> {
    uploads::get(library, job_id)?.ok_or_else(|| CoreError::NotFound {
        detail: format!("upload job {job_id}"),
    })
}

/// Number of parts for `size` bytes: `PART_SIZE` each, the last one possibly shorter.
fn part_count(size: u64) -> i32 {
    i32::try_from(size.div_ceil(PART_SIZE as u64)).unwrap_or(i32::MAX)
}

/// Byte offset of part `part_number` (1-based) in the source.
fn part_offset(part_number: i32) -> u64 {
    u64::try_from(part_number - 1).unwrap_or(0) * PART_SIZE as u64
}

/// Length of part `part_number` of a `size`-byte source.
fn part_len(size: u64, part_number: i32) -> usize {
    size.saturating_sub(part_offset(part_number))
        .min(PART_SIZE as u64) as usize
}

/// Reads part `part_number` of `file` at its offset, without moving the file position.
#[cfg(unix)]
fn read_part(file: &File, part_number: i32, len: usize) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt;

    let mut buf = vec![0u8; len];
    file.read_exact_at(&mut buf, part_offset(part_number))?;
    Ok(buf)
}

#[cfg(not(unix))]
fn read_part(_file: &File, _part_number: i32, _len: usize) -> io::Result<Vec<u8>> {
    Err(io::Error::other("uploads need a unix file descriptor"))
}

/// Run bookkeeping shared by the upload calls: which jobs have a run or a cancel in progress,
/// and which running jobs were asked to stop. Never held across I/O.
#[derive(Debug, Default)]
pub(crate) struct UploadControl {
    running: HashSet<i64>,
    cancel: HashSet<i64>,
}

fn lock_control(control: &Mutex<UploadControl>) -> MutexGuard<'_, UploadControl> {
    control.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Holds one job for a run or a direct cancel. Releasing the job also forgets any cancel
/// request for it, on every exit path.
pub(crate) struct UploadClaim<'a> {
    control: &'a Mutex<UploadControl>,
    job_id: i64,
}

impl<'a> UploadClaim<'a> {
    /// Claims `job_id` for a run. `None` when another run or cancel holds it.
    pub(crate) fn acquire(control: &'a Mutex<UploadControl>, job_id: i64) -> Option<Self> {
        let mut ctl = lock_control(control);
        if !ctl.running.insert(job_id) {
            return None;
        }
        drop(ctl);
        Some(UploadClaim { control, job_id })
    }
}

impl Drop for UploadClaim<'_> {
    fn drop(&mut self) {
        let mut ctl = lock_control(self.control);
        ctl.running.remove(&self.job_id);
        ctl.cancel.remove(&self.job_id);
    }
}

/// What a cancel request did.
pub(crate) enum CancelRoute<'a> {
    /// A run holds the job. It stops at its next part boundary.
    Requested,
    /// No run held the job, so the caller holds it now and cancels it directly.
    Claimed(UploadClaim<'a>),
}

/// Asks the run of `job_id` to stop, or claims the job when nothing runs it. The check and the
/// claim happen under one lock, so a run cannot start in between.
pub(crate) fn request_cancel(control: &Mutex<UploadControl>, job_id: i64) -> CancelRoute<'_> {
    let mut ctl = lock_control(control);
    if ctl.running.contains(&job_id) {
        ctl.cancel.insert(job_id);
        return CancelRoute::Requested;
    }
    ctl.running.insert(job_id);
    drop(ctl);
    CancelRoute::Claimed(UploadClaim { control, job_id })
}

/// Whether a cancel was requested for the job that is running.
fn cancel_requested(control: &Mutex<UploadControl>, job_id: i64) -> bool {
    lock_control(control).cancel.contains(&job_id)
}

/// Everything a run needs from the core.
pub(crate) struct RunEnv<'a> {
    pub(crate) library: &'a Library,
    pub(crate) rt: &'a Runtime,
    pub(crate) storage: &'a Storage,
    pub(crate) listener: &'a ListenerSlot,
    pub(crate) control: &'a Mutex<UploadControl>,
}

/// How the transfer of one job ended without a storage or read error.
enum Stop {
    Done,
    Cancelled,
}

/// Marks the job failed with `message` (truncated) and tells the listener.
fn fail(env: &RunEnv<'_>, job_id: i64, message: &str) -> CoreResult<()> {
    let message = truncate_bytes(message, MAX_ERROR_BYTES);
    uploads::set_state(env.library, job_id, UploadState::Failed, Some(message))?;
    env.listener.emit(CoreEvent::UploadFinished {
        job_id,
        state: UploadState::Failed.into(),
    });
    Ok(())
}

/// Runs job `job_id` from wherever it stopped, reading the source through `file`. Returns the
/// row as it is afterwards. Final jobs return unchanged. Storage and read errors leave the job
/// `Failed` and keep its upload id, so a later run resumes it; they are not returned as errors.
/// Only catalogue errors and a missing job are returned as errors.
///
/// The caller holds the job's [`UploadClaim`].
pub(crate) fn run_job(env: &RunEnv<'_>, job_id: i64, file: &File) -> CoreResult<UploadRow> {
    let row = load(env.library, job_id)?;
    if row.state.is_terminal() {
        return Ok(row);
    }
    let size = file.metadata()?.len();
    if size != row.size {
        fail(env, job_id, "source changed")?;
        return load(env.library, job_id);
    }

    uploads::set_state(env.library, job_id, UploadState::Uploading, None)?;
    match transfer(env, job_id, &row, file, size) {
        Ok(Stop::Done) => {
            log::info!("upload job {job_id}: done ({size} bytes)");
            uploads::set_progress(env.library, job_id, size)?;
            uploads::set_state(env.library, job_id, UploadState::Done, None)?;
            env.listener.emit(CoreEvent::UploadFinished {
                job_id,
                state: UploadState::Done.into(),
            });
        }
        Ok(Stop::Cancelled) => {
            uploads::set_state(env.library, job_id, UploadState::Cancelled, None)?;
            env.listener.emit(CoreEvent::UploadFinished {
                job_id,
                state: UploadState::Cancelled.into(),
            });
        }
        Err(e) => {
            log::warn!("upload job {job_id}: failed: {e}");
            fail(env, job_id, &e.to_string())?;
        }
    }
    load(env.library, job_id)
}

/// Uploads the parts of `row` that are not stored yet, then completes the upload.
fn transfer(
    env: &RunEnv<'_>,
    job_id: i64,
    row: &UploadRow,
    file: &File,
    size: u64,
) -> CoreResult<Stop> {
    let (upload_id, mut parts) = open_upload(env, job_id, row)?;
    let stored: HashSet<i32> = parts.iter().map(|p| p.part_number).collect();
    log::info!(
        "upload job {job_id}: {} of {} parts already stored",
        stored.len(),
        part_count(size)
    );
    let mut bytes_done: u64 = (1..=part_count(size))
        .filter(|n| stored.contains(n))
        .map(|n| part_len(size, n) as u64)
        .sum();

    for part_number in 1..=part_count(size) {
        if stored.contains(&part_number) {
            continue;
        }
        if cancel_requested(env.control, job_id) {
            abort_upload(env, job_id, &row.key, &upload_id);
            return Ok(Stop::Cancelled);
        }
        let len = part_len(size, part_number);
        let data = read_part(file, part_number, len).map_err(|e| CoreError::Internal {
            detail: format!("reading the source file: {e}"),
        })?;
        let part = env.rt.block_on(env.storage.upload_part(
            &row.key,
            &upload_id,
            part_number,
            Bytes::from(data),
        ))?;
        parts.push(part);
        bytes_done += len as u64;
        uploads::set_progress(env.library, job_id, bytes_done)?;
        env.listener.emit(CoreEvent::UploadProgress {
            job_id,
            bytes_done,
            total: size,
        });
    }

    env.rt
        .block_on(env.storage.complete_multipart(&row.key, &upload_id, &parts))?;
    Ok(Stop::Done)
}

/// The multipart upload id for `row` and the parts it already has. Resumes the stored upload when
/// it still exists. Otherwise starts a new one, with the content hash as object metadata, and
/// stores its id.
fn open_upload(
    env: &RunEnv<'_>,
    job_id: i64,
    row: &UploadRow,
) -> CoreResult<(String, Vec<UploadedPart>)> {
    if let Some(existing) = row.upload_id.as_deref() {
        match env.rt.block_on(env.storage.list_parts(&row.key, existing)) {
            Ok(parts) => return Ok((existing.to_owned(), parts)),
            // The upload was aborted or has expired: start a new one below.
            Err(StorageError::NotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let upload_id = env.rt.block_on(env.storage.create_multipart_with_meta(
        &row.key,
        &row.content_type,
        &[(HASH_META, row.blake3.as_str())],
    ))?;
    uploads::set_upload_id(env.library, job_id, Some(&upload_id))?;
    Ok((upload_id, Vec::new()))
}

/// Aborts the multipart upload, best effort. Failures are logged; the caller carries on.
fn abort_upload(env: &RunEnv<'_>, job_id: i64, key: &str, upload_id: &str) {
    match env.rt.block_on(env.storage.abort_multipart(key, upload_id)) {
        Ok(()) | Err(StorageError::NotFound(_)) => {}
        Err(e) => log::warn!("aborting the upload of job {job_id} failed: {e}"),
    }
}

/// Cancels a job that no run holds (the caller holds its claim). Final jobs are left alone. A
/// partial multipart upload is aborted when a bucket is available (`storage`); otherwise it is
/// left in place and the job is still marked cancelled.
pub(crate) fn cancel_job(
    library: &Library,
    listener: &ListenerSlot,
    rt: &Runtime,
    storage: Option<&Storage>,
    job_id: i64,
) -> CoreResult<()> {
    let row = load(library, job_id)?;
    if row.state.is_terminal() {
        return Ok(());
    }
    if let Some(upload_id) = row.upload_id.as_deref() {
        match storage {
            Some(storage) => match rt.block_on(storage.abort_multipart(&row.key, upload_id)) {
                Ok(()) | Err(StorageError::NotFound(_)) => {}
                Err(e) => log::warn!("aborting the upload of job {job_id} failed: {e}"),
            },
            None => log::warn!("job {job_id} cancelled without a bucket; upload not aborted"),
        }
    }
    uploads::set_state(library, job_id, UploadState::Cancelled, None)?;
    listener.emit(CoreEvent::UploadFinished {
        job_id,
        state: UploadState::Cancelled.into(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    /// A directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "sonorus-core-upload-{tag}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Fixture with tags: artist "Artist B", album "Album Two", title "Wave Song", track 1.
    fn wav_fixture() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../library/tests/fixtures/Artist B/Album Two/01 - Wave Song.wav"
        ))
    }

    /// Fixture with no tags at all.
    fn untagged_fixture() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../library/tests/fixtures/Artist B/Album Two/03 - Untagged Track.flac"
        ))
    }

    fn meta() -> TrackMeta {
        TrackMeta::default()
    }

    fn no_bucket(_key: &str) -> CoreResult<Option<ObjectMeta>> {
        Ok(None)
    }

    fn library(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).expect("library")
    }

    fn object_meta(key: &str, hash: Option<&str>) -> ObjectMeta {
        let mut metadata = std::collections::HashMap::new();
        if let Some(hash) = hash {
            metadata.insert(HASH_META.to_owned(), hash.to_owned());
        }
        ObjectMeta {
            key: key.to_owned(),
            size: 1,
            etag: None,
            content_type: None,
            last_modified: None,
            metadata,
        }
    }

    #[test]
    fn key_for_fully_tagged_track() {
        let m = TrackMeta {
            title: Some("Wave Song".into()),
            artist: Some("Artist B".into()),
            album_artist: Some("Artist B".into()),
            album: Some("Album Two".into()),
            track_no: Some(1),
            ..Default::default()
        };
        assert_eq!(
            object_key("", &m, "01 - Wave Song.wav", "wav", 1),
            "Artist B/Album Two/01 - Wave Song.wav"
        );
    }

    #[test]
    fn key_uses_album_artist_before_artist() {
        let m = TrackMeta {
            title: Some("Duet".into()),
            artist: Some("Guest".into()),
            album_artist: Some("Main".into()),
            album: Some("Album".into()),
            ..Default::default()
        };
        assert_eq!(
            object_key("", &m, "x.flac", "flac", 1),
            "Main/Album/Duet.flac"
        );
        let only_artist = TrackMeta {
            artist: Some("Solo".into()),
            ..m.clone()
        };
        let only_artist = TrackMeta {
            album_artist: None,
            ..only_artist
        };
        assert_eq!(
            object_key("", &only_artist, "x.flac", "flac", 1),
            "Solo/Album/Duet.flac"
        );
    }

    #[test]
    fn key_missing_tags_fall_back_to_placeholders_and_file_name() {
        assert_eq!(
            object_key("", &meta(), "My Song.m4a", "m4a", 1),
            "Unknown Artist/Unknown Album/My Song.m4a"
        );
        let blank = TrackMeta {
            title: Some("   ".into()),
            artist: Some("  ".into()),
            album: Some("".into()),
            ..Default::default()
        };
        assert_eq!(
            object_key("", &blank, "Plain.flac", "flac", 1),
            "Unknown Artist/Unknown Album/Plain.flac"
        );
    }

    #[test]
    fn key_numbers_tracks_with_and_without_discs() {
        let single = TrackMeta {
            title: Some("T".into()),
            track_no: Some(5),
            disc_no: Some(2),
            disc_total: Some(1),
            ..Default::default()
        };
        assert_eq!(
            object_key("", &single, "x.flac", "flac", 1),
            "Unknown Artist/Unknown Album/05 - T.flac"
        );

        let multi = TrackMeta {
            disc_total: Some(2),
            ..single.clone()
        };
        assert_eq!(
            object_key("", &multi, "x.flac", "flac", 1),
            "Unknown Artist/Unknown Album/2-05 - T.flac"
        );

        let no_disc = TrackMeta {
            disc_no: None,
            ..multi.clone()
        };
        assert_eq!(
            object_key("", &no_disc, "x.flac", "flac", 1),
            "Unknown Artist/Unknown Album/05 - T.flac"
        );

        let no_track = TrackMeta {
            track_no: None,
            ..multi
        };
        assert_eq!(
            object_key("", &no_track, "x.flac", "flac", 1),
            "Unknown Artist/Unknown Album/T.flac"
        );
    }

    #[test]
    fn key_has_track_number_for_track_100_and_up() {
        let m = TrackMeta {
            title: Some("T".into()),
            track_no: Some(123),
            ..Default::default()
        };
        assert_eq!(
            object_key("", &m, "x.flac", "flac", 1),
            "Unknown Artist/Unknown Album/123 - T.flac"
        );
    }

    #[test]
    fn key_title_falls_back_to_display_stem_without_extension() {
        let m = TrackMeta {
            title: None,
            ..Default::default()
        };
        assert_eq!(
            object_key("", &m, "Track.One.flac", "flac", 1),
            "Unknown Artist/Unknown Album/Track.One.flac"
        );
    }

    #[test]
    fn key_uses_the_bucket_prefix() {
        assert_eq!(
            object_key("music/", &meta(), "x.flac", "flac", 1),
            "music/Unknown Artist/Unknown Album/x.flac"
        );
        assert_eq!(
            object_key("", &meta(), "x.flac", "flac", 1),
            "Unknown Artist/Unknown Album/x.flac"
        );
    }

    #[test]
    fn key_suffixes_from_the_second_attempt() {
        let m = TrackMeta {
            title: Some("Song".into()),
            artist: Some("A".into()),
            album: Some("B".into()),
            ..Default::default()
        };
        assert_eq!(object_key("", &m, "x.flac", "flac", 1), "A/B/Song.flac");
        assert_eq!(object_key("", &m, "x.flac", "flac", 2), "A/B/Song (2).flac");
        assert_eq!(
            object_key("", &m, "x.flac", "flac", 50),
            "A/B/Song (50).flac"
        );
    }

    #[test]
    fn sanitize_replaces_illegal_and_control_characters() {
        assert_eq!(sanitize_component("AC/DC"), "AC_DC");
        assert_eq!(sanitize_component(r"a\b:c*d?e"), "a_b_c_d_e");
        assert_eq!(sanitize_component("say \"hi\" <now> |"), "say _hi_ _now_ _");
        assert_eq!(sanitize_component("bell\u{0007}char"), "bell_char");
        assert_eq!(sanitize_component("nul\0byte"), "nul_byte");
    }

    #[test]
    fn sanitize_collapses_whitespace_and_trims_dots_and_spaces() {
        assert_eq!(sanitize_component("  a   b  "), "a b");
        assert_eq!(sanitize_component("tab\tand\nnewline"), "tab and newline");
        assert_eq!(sanitize_component("..hidden.."), "hidden");
        assert_eq!(sanitize_component(" . . "), "");
        assert_eq!(sanitize_component("..."), "");
        assert_eq!(sanitize_component("Mr. Dot."), "Mr. Dot");
    }

    #[test]
    fn sanitize_keeps_unicode() {
        assert_eq!(sanitize_component("Björk"), "Björk");
        assert_eq!(sanitize_component("坂本龍一"), "坂本龍一");
        assert_eq!(sanitize_component("Ünïcode ✓ 🎵"), "Ünïcode ✓ 🎵");
    }

    #[test]
    fn sanitize_caps_length_on_char_boundary() {
        let long = "é".repeat(100); // 200 bytes
        let out = sanitize_component(&long);
        assert!(out.len() <= MAX_NAME_BYTES, "len {}", out.len());
        assert_eq!(out.len(), 120, "whole characters only: 60 of them");
        assert!(out.chars().all(|c| c == 'é'));

        let ascii = "a".repeat(300);
        assert_eq!(sanitize_component(&ascii).len(), MAX_NAME_BYTES);
    }

    #[test]
    fn long_names_are_capped_per_component_including_extension() {
        let long_title = "T".repeat(400);
        let m = TrackMeta {
            title: Some(long_title),
            artist: Some("A".repeat(300)),
            album: Some("é".repeat(70)),
            track_no: Some(3),
            ..Default::default()
        };
        let key = object_key("p/", &m, "x.flac", "flac", 1);
        let parts: Vec<&str> = key.split('/').collect();
        assert_eq!(parts.len(), 4, "prefix, artist, album, file: {key}");
        assert_eq!(parts[0], "p");
        assert!(parts[1].len() <= MAX_NAME_BYTES);
        assert!(parts[2].len() <= MAX_NAME_BYTES);
        assert!(parts[3].len() <= MAX_NAME_BYTES, "file {}", parts[3].len());
        assert!(parts[3].ends_with(".flac"));
        assert!(parts[3].starts_with("03 - TTT"));

        // The suffix counts against the budget too.
        let suffixed = object_key("", &m, "x.flac", "flac", 50);
        let file = suffixed.rsplit('/').next().unwrap();
        assert!(file.len() <= MAX_NAME_BYTES, "file {}", file.len());
        assert!(file.ends_with(" (50).flac"));
    }

    #[test]
    fn illegal_characters_in_tags_never_create_path_segments() {
        let m = TrackMeta {
            title: Some("a/b".into()),
            artist: Some("x/y".into()),
            album: Some("../..".into()),
            ..Default::default()
        };
        let key = object_key("", &m, "x.flac", "flac", 1);
        // "../.." keeps one underscore once its dots are trimmed.
        assert_eq!(key, "x_y/_/a_b.flac");
        assert_eq!(key.matches('/').count(), 2);
    }

    #[test]
    fn empty_components_fall_back_to_placeholders() {
        let m = TrackMeta {
            title: Some("...".into()),
            artist: Some("..".into()),
            album: Some("   ".into()),
            ..Default::default()
        };
        let key = object_key("", &m, "....flac", "flac", 1);
        assert_eq!(key, "Unknown Artist/Unknown Album/Untitled.flac");
    }

    #[test]
    fn file_name_never_empty() {
        assert_eq!(file_name("", "flac", 1), "Untitled.flac");
        assert_eq!(file_name("ok", "m4a", 2), "ok (2).m4a");
    }

    #[test]
    fn audio_extension_is_lowercase_and_requires_audio() {
        assert_eq!(audio_extension("A.FLAC"), Some("flac".to_owned()));
        assert_eq!(audio_extension("x.m4a"), Some("m4a".to_owned()));
        assert_eq!(audio_extension("cover.jpg"), None);
        assert_eq!(audio_extension("noext"), None);
    }

    #[test]
    fn display_stem_strips_path_and_last_extension() {
        assert_eq!(display_stem("Song.v2.flac"), "Song.v2");
        assert_eq!(display_stem("dir/Song.flac"), "Song");
        assert_eq!(display_stem("dir\\Song.flac"), "Song");
        assert_eq!(display_stem("Song"), "Song");
    }

    #[test]
    fn part_arithmetic() {
        let size = 2 * PART_SIZE as u64 + 5;
        assert_eq!(part_count(size), 3);
        assert_eq!(part_count(PART_SIZE as u64), 1);
        assert_eq!(part_len(size, 1), PART_SIZE);
        assert_eq!(part_len(size, 2), PART_SIZE);
        assert_eq!(part_len(size, 3), 5);
        assert_eq!(part_offset(2), PART_SIZE as u64);
        assert_eq!(part_offset(1), 0);
    }

    #[test]
    fn take_fd_rejects_negative_descriptors() {
        assert!(matches!(take_fd(-1), Err(CoreError::Internal { .. })));
    }

    #[test]
    fn hash_matches_blake3_of_the_bytes() {
        let path = wav_fixture();
        let expected = blake3::hash(&fs::read(&path).unwrap()).to_hex().to_string();
        let mut file = File::open(&path).unwrap();
        assert_eq!(hash_file(&mut file).unwrap(), expected);
        // Hashing starts from the beginning even when the position is elsewhere.
        file.seek(SeekFrom::Start(100)).unwrap();
        assert_eq!(hash_file(&mut file).unwrap(), expected);
    }

    #[test]
    fn hash_handles_content_larger_than_the_buffer() {
        let dir = TempDir::new("big-hash");
        let path = dir.path().join("big.flac");
        let data: Vec<u8> = (0..(3 * HASH_BUFFER + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        fs::write(&path, &data).unwrap();
        let mut file = File::open(&path).unwrap();
        assert_eq!(
            hash_file(&mut file).unwrap(),
            blake3::hash(&data).to_hex().to_string()
        );
    }

    #[test]
    fn prepare_queues_a_new_file_with_tags_and_hash() {
        let dir = TempDir::new("prepare");
        let lib = library(&dir);
        let path = wav_fixture();
        let mut file = File::open(&path).unwrap();
        let mut head = no_bucket;
        let info = prepare_job(
            &lib,
            "music/",
            &mut file,
            "content://picked/1",
            "01 - Wave Song.wav",
            &mut head,
        )
        .unwrap();

        let bytes = fs::read(&path).unwrap();
        assert_eq!(info.state, crate::types::UploadState::Queued);
        assert_eq!(info.size, bytes.len() as u64);
        assert_eq!(info.bytes_done, 0);
        assert_eq!(info.error, None);
        assert_eq!(info.key, "music/Artist B/Album Two/01 - Wave Song.wav");

        let row = uploads::get(&lib, info.id).unwrap().unwrap();
        assert_eq!(row.blake3, blake3::hash(&bytes).to_hex().to_string());
        assert_eq!(row.source_uri, "content://picked/1");
        assert_eq!(row.content_type, "audio/wav");
        assert_eq!(row.state, UploadState::Queued);
    }

    #[test]
    fn prepare_uses_untagged_file_name_and_placeholders() {
        let dir = TempDir::new("untagged");
        let lib = library(&dir);
        let mut file = File::open(untagged_fixture()).unwrap();
        let mut head = no_bucket;
        let info = prepare_job(
            &lib,
            "",
            &mut file,
            "uri",
            "03 - Untagged Track.flac",
            &mut head,
        )
        .unwrap();
        assert_eq!(
            info.key,
            "Unknown Artist/Unknown Album/03 - Untagged Track.flac"
        );
    }

    #[test]
    fn prepare_rejects_non_audio_names_without_a_row() {
        let dir = TempDir::new("nonaudio");
        let lib = library(&dir);
        let mut file = File::open(wav_fixture()).unwrap();
        let mut head = no_bucket;
        let err = prepare_job(&lib, "", &mut file, "uri", "cover.jpg", &mut head).unwrap_err();
        assert!(matches!(err, CoreError::InvalidConfig { .. }));
        assert!(uploads::list(&lib).unwrap().is_empty());
    }

    #[test]
    fn prepare_rejects_garbage_content_without_a_row() {
        let dir = TempDir::new("garbage");
        let lib = library(&dir);
        let path = dir.path().join("fake.flac");
        fs::write(&path, vec![0u8; 256]).unwrap();
        let mut file = File::open(&path).unwrap();
        let mut head = no_bucket;
        assert!(prepare_job(&lib, "", &mut file, "uri", "fake.flac", &mut head).is_err());
        assert!(uploads::list(&lib).unwrap().is_empty());
    }

    #[test]
    fn prepare_rejects_empty_files() {
        let dir = TempDir::new("empty");
        let lib = library(&dir);
        let path = dir.path().join("empty.flac");
        fs::write(&path, b"").unwrap();
        let mut file = File::open(&path).unwrap();
        let mut head = no_bucket;
        let err = prepare_job(&lib, "", &mut file, "uri", "empty.flac", &mut head).unwrap_err();
        assert!(matches!(err, CoreError::InvalidConfig { .. }));
    }

    #[test]
    fn prepare_dedupes_identical_content_through_the_queue() {
        let dir = TempDir::new("dedupe-table");
        let lib = library(&dir);
        let mut head = no_bucket;
        let first = prepare_job(
            &lib,
            "",
            &mut File::open(wav_fixture()).unwrap(),
            "uri/1",
            "01 - Wave Song.wav",
            &mut head,
        )
        .unwrap();
        assert_eq!(first.state, crate::types::UploadState::Queued);

        // The same bytes under another name: found by hash in the queue.
        let second = prepare_job(
            &lib,
            "",
            &mut File::open(wav_fixture()).unwrap(),
            "uri/2",
            "copy of wave.wav",
            &mut head,
        )
        .unwrap();
        assert_eq!(second.state, crate::types::UploadState::Duplicate);
        assert_eq!(
            second.key, first.key,
            "duplicate points at the existing key"
        );
    }

    #[test]
    fn prepare_dedupes_against_existing_bucket_object_with_same_hash() {
        let dir = TempDir::new("dedupe-bucket");
        let lib = library(&dir);
        let bytes = fs::read(wav_fixture()).unwrap();
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let key = "Artist B/Album Two/01 - Wave Song.wav";
        let mut head = |k: &str| -> CoreResult<Option<ObjectMeta>> {
            Ok((k == key).then(|| object_meta(key, Some(&hash))))
        };
        let info = prepare_job(
            &lib,
            "",
            &mut File::open(wav_fixture()).unwrap(),
            "uri",
            "01 - Wave Song.wav",
            &mut head,
        )
        .unwrap();
        assert_eq!(info.state, crate::types::UploadState::Duplicate);
        assert_eq!(info.key, key);
    }

    #[test]
    fn prepare_suffixes_when_key_holds_other_content() {
        let dir = TempDir::new("suffix");
        let lib = library(&dir);
        let taken = "Artist B/Album Two/01 - Wave Song.wav";
        let taken_2 = "Artist B/Album Two/01 - Wave Song (2).wav";
        let mut head = |k: &str| -> CoreResult<Option<ObjectMeta>> {
            Ok(match k {
                k if k == taken => Some(object_meta(k, Some("someone-else"))),
                k if k == taken_2 => Some(object_meta(k, None)),
                _ => None,
            })
        };
        let info = prepare_job(
            &lib,
            "",
            &mut File::open(wav_fixture()).unwrap(),
            "uri",
            "01 - Wave Song.wav",
            &mut head,
        )
        .unwrap();
        assert_eq!(info.state, crate::types::UploadState::Queued);
        assert_eq!(info.key, "Artist B/Album Two/01 - Wave Song (3).wav");
    }

    #[test]
    fn prepare_skips_keys_reserved_by_queued_jobs() {
        let dir = TempDir::new("reserved");
        let lib = library(&dir);
        // Another file with different content already queued for the plain key.
        uploads::insert(
            &lib,
            &NewUpload {
                source_uri: "uri/other".into(),
                display_name: "other.wav".into(),
                size: 10,
                blake3: "not-this-one".into(),
                key: "Artist B/Album Two/01 - Wave Song.wav".into(),
                content_type: "audio/wav".into(),
                state: UploadState::Queued,
            },
        )
        .unwrap();
        let mut head = no_bucket;
        let info = prepare_job(
            &lib,
            "",
            &mut File::open(wav_fixture()).unwrap(),
            "uri",
            "01 - Wave Song.wav",
            &mut head,
        )
        .unwrap();
        assert_eq!(info.key, "Artist B/Album Two/01 - Wave Song (2).wav");
    }

    #[test]
    fn prepare_gives_up_after_fifty_taken_keys() {
        let dir = TempDir::new("exhausted");
        let lib = library(&dir);
        let mut head =
            |k: &str| -> CoreResult<Option<ObjectMeta>> { Ok(Some(object_meta(k, Some("other")))) };
        let err = prepare_job(
            &lib,
            "",
            &mut File::open(wav_fixture()).unwrap(),
            "uri",
            "01 - Wave Song.wav",
            &mut head,
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::Internal { .. }));
        assert!(uploads::list(&lib).unwrap().is_empty());
    }

    #[test]
    fn prepare_propagates_bucket_lookup_errors() {
        let dir = TempDir::new("lookup-error");
        let lib = library(&dir);
        let mut head = |_: &str| -> CoreResult<Option<ObjectMeta>> {
            Err(CoreError::Storage {
                detail: "network error: timeout".into(),
            })
        };
        let err = prepare_job(
            &lib,
            "",
            &mut File::open(wav_fixture()).unwrap(),
            "uri",
            "01 - Wave Song.wav",
            &mut head,
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::Storage { .. }));
        assert!(uploads::list(&lib).unwrap().is_empty());
    }

    #[test]
    fn claims_serialize_runs_and_cancels_per_job() {
        let control = Mutex::new(UploadControl::default());
        let run = UploadClaim::acquire(&control, 7).expect("first claim");
        assert!(UploadClaim::acquire(&control, 7).is_none(), "busy");
        assert!(
            UploadClaim::acquire(&control, 8).is_some(),
            "other job free"
        );

        match request_cancel(&control, 7) {
            CancelRoute::Requested => {}
            CancelRoute::Claimed(_) => panic!("a running job must not be claimed"),
        }
        assert!(cancel_requested(&control, 7));
        drop(run);
        assert!(!cancel_requested(&control, 7), "release forgets the cancel");
        assert!(UploadClaim::acquire(&control, 7).is_some(), "free again");
    }

    #[test]
    fn cancel_of_an_idle_job_claims_it_until_dropped() {
        let control = Mutex::new(UploadControl::default());
        let claim = match request_cancel(&control, 3) {
            CancelRoute::Claimed(claim) => claim,
            CancelRoute::Requested => panic!("nothing was running"),
        };
        assert!(UploadClaim::acquire(&control, 3).is_none());
        drop(claim);
        assert!(UploadClaim::acquire(&control, 3).is_some());
    }
}
