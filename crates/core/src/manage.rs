//! Bucket management: browsing the bucket (or local folder) and renaming, moving, deleting and
//! re-tagging objects, and setting an album's cover.
//!
//! Every key or folder an operation touches goes through [`KeyGuard`], which confines it to the
//! configured prefix. Each change to the objects is followed by the matching catalogue change, so
//! favorites, playlists and history keep pointing at the right keys. The operations are blocking:
//! they run on the caller's thread and block on the core runtime for S3 calls.
//!
//! Object changes are copy, then verify, then remove, so an object is never lost to a failed copy.
//! A failed move removes the copies it made. Where the object is removed before the catalogue is
//! updated (a move), the catalogue update is the last step, and a failure there is returned.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use sonorus_engine::cache::TrackCache;
use sonorus_library::rekey::{forget_uploads, rekey_track, set_album_cover as set_cover_row};
use sonorus_library::{Library, MetaSource, ObjectStamp, TagEdit, scan_key, uploads, write_tags};
use sonorus_storage::{FolderListing, ObjectInfo, Storage, is_audio_key};
use tokio::runtime::Runtime;

use crate::error::{CoreError, CoreResult};
use crate::types::{BucketEntryInfo, BucketFolderInfo, DeleteReport, FailureInfo, MoveReport};

/// The folder under the prefix that holds internal sync data. No write may touch it.
pub(crate) const INTERNAL_DIR: &str = "_sonorus";
/// Longest file name a rename accepts, in bytes.
const MAX_NAME_BYTES: usize = 255;
/// Largest cover image `set_album_cover` accepts.
pub(crate) const MAX_COVER_BYTES: u64 = 20 * 1024 * 1024;
/// Bytes requested per range read when a track is downloaded for a tag edit.
const DOWNLOAD_CHUNK: u64 = 4 * 1024 * 1024;
/// JPEG files start with this start-of-image marker.
const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8, 0xFF];
/// PNG files start with this signature.
const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn refused(detail: impl Into<String>) -> CoreError {
    CoreError::Refused {
        detail: detail.into(),
    }
}

/// Confines an operation to the configured prefix: `""` for a local folder, otherwise the bucket
/// prefix, which is empty or ends with `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyGuard {
    prefix: String,
}

impl KeyGuard {
    /// A guard for keys under `prefix`.
    pub(crate) fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_owned(),
        }
    }

    /// The prefix this guard confines keys to.
    pub(crate) fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The key of the folder at `path`, which is relative to the prefix. `""` is the root (the
    /// prefix itself). A missing trailing `/` is added. A leading `/` is refused.
    pub(crate) fn folder_key(&self, path: &str) -> CoreResult<String> {
        if path.is_empty() {
            return Ok(self.prefix.clone());
        }
        if path.starts_with('/') {
            return Err(refused(format!("{path:?} is absolute")));
        }
        if path.ends_with('/') {
            Ok(format!("{}{path}", self.prefix))
        } else {
            Ok(format!("{}{path}/", self.prefix))
        }
    }

    /// The part of `key` after the prefix. `key` must be under the prefix (checked by `read`).
    pub(crate) fn relative<'a>(&self, key: &'a str) -> &'a str {
        key.strip_prefix(self.prefix.as_str()).unwrap_or(key)
    }

    /// Checks a key that an operation reads: it is under the prefix, does not start with `/`, has
    /// no NUL byte, and no segment is empty, `.` or `..`. The prefix itself (the root) passes.
    pub(crate) fn read(&self, key: &str) -> CoreResult<()> {
        if key.starts_with('/') {
            return Err(refused(format!("{key:?} starts with '/'")));
        }
        if key.contains('\0') {
            return Err(refused("a key contains a NUL byte"));
        }
        let rest = key
            .strip_prefix(self.prefix.as_str())
            .ok_or_else(|| refused(format!("{key:?} is outside the bucket prefix")))?;
        if rest.is_empty() {
            return Ok(());
        }
        let body = rest.strip_suffix('/').unwrap_or(rest);
        if body
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(refused(format!(
                "{key:?} has an empty, '.' or '..' segment"
            )));
        }
        Ok(())
    }

    /// Checks a key that an operation writes or removes: [`read`](Self::read), and not under the
    /// internal `_sonorus/` folder.
    pub(crate) fn write(&self, key: &str) -> CoreResult<()> {
        self.read(key)?;
        if self.relative(key).split('/').next() == Some(INTERNAL_DIR) {
            return Err(refused(format!("{INTERNAL_DIR}/ is internal sync data")));
        }
        Ok(())
    }

    /// Checks an object key to write or remove: [`write`](Self::write), and not a folder.
    pub(crate) fn object(&self, key: &str) -> CoreResult<()> {
        self.write(key)?;
        if key.is_empty() || key.ends_with('/') {
            return Err(refused(format!("{key:?} is a folder, not a file")));
        }
        Ok(())
    }

    /// Checks a folder to remove, rename or move: [`write`](Self::write), ends with `/`, and is
    /// not the root. The root can never be removed, renamed or moved.
    pub(crate) fn folder(&self, key: &str) -> CoreResult<()> {
        if key == self.prefix {
            return Err(refused(
                "the bucket root cannot be deleted, renamed or moved",
            ));
        }
        if !key.ends_with('/') {
            return Err(refused(format!("{key:?} is not a folder")));
        }
        self.write(key)
    }

    /// Checks a folder that files are written into (a move target, or an album's folder). The
    /// root is allowed.
    pub(crate) fn dir(&self, key: &str) -> CoreResult<()> {
        if key != self.prefix && !key.ends_with('/') {
            return Err(refused(format!("{key:?} is not a folder")));
        }
        self.write(key)
    }
}

/// Checks a plain file name for a rename: not blank, at most 255 bytes, no `/` or NUL byte, and
/// not `.` or `..`.
pub(crate) fn check_name(name: &str) -> CoreResult<()> {
    if name.trim().is_empty() {
        return Err(refused("the new name is empty"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(refused(format!(
            "the new name is longer than {MAX_NAME_BYTES} bytes"
        )));
    }
    if name.contains('/') || name.contains('\0') {
        return Err(refused("the new name contains '/' or a NUL byte"));
    }
    if name == "." || name == ".." {
        return Err(refused("the new name is '.' or '..'"));
    }
    Ok(())
}

/// The folder part of `key` (up to and including its last `/`), `""` for a key without one.
pub(crate) fn parent_of(key: &str) -> &str {
    key.rfind('/').map_or("", |i| &key[..=i])
}

/// The last path segment of `key`, ignoring a trailing `/`.
pub(crate) fn name_of(key: &str) -> &str {
    let trimmed = key.strip_suffix('/').unwrap_or(key);
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// The file name's extension (after its last dot, not a leading dot), if any.
fn ext_of(key: &str) -> Option<&str> {
    let name = name_of(key);
    match name.rfind('.') {
        Some(dot) if dot > 0 => Some(&name[dot + 1..]),
        _ => None,
    }
}

/// The key without its extension. A key with no extension is returned whole.
pub(crate) fn stem_of(key: &str) -> &str {
    let name_start = key.rfind('/').map_or(0, |i| i + 1);
    match key[name_start..].rfind('.') {
        Some(dot) if dot > 0 => &key[..name_start + dot],
        _ => key,
    }
}

/// Pairs each audio object in `objects` with its `.lrc` sidecar: the object with the same stem
/// and a lowercase or uppercase `lrc` extension. Within a stem the lowest key is the sidecar, and
/// a sidecar is claimed by one audio object only (the first by key), so it moves or is removed
/// with exactly one track. Keys are the audio object's key.
pub(crate) fn claim_sidecars(objects: &[ObjectInfo]) -> HashMap<String, ObjectInfo> {
    let mut lrcs: Vec<&ObjectInfo> = objects
        .iter()
        .filter(|o| ext_of(&o.key).is_some_and(|e| e.eq_ignore_ascii_case("lrc")))
        .collect();
    lrcs.sort_by(|a, b| a.key.cmp(&b.key));
    let mut by_stem: HashMap<&str, &ObjectInfo> = HashMap::new();
    for lrc in lrcs {
        by_stem.entry(stem_of(&lrc.key)).or_insert(lrc);
    }
    let mut audio: Vec<&ObjectInfo> = objects.iter().filter(|o| is_audio_key(&o.key)).collect();
    audio.sort_by(|a, b| a.key.cmp(&b.key));
    let mut claimed: HashSet<&str> = HashSet::new();
    let mut claims = HashMap::new();
    for track in audio {
        let Some(lrc) = by_stem.get(stem_of(&track.key)) else {
            continue;
        };
        if claimed.insert(lrc.key.as_str()) {
            claims.insert(track.key.clone(), (*lrc).clone());
        }
    }
    claims
}

/// Where the objects are: the bucket, or a local folder whose files are the objects (keys are
/// paths relative to it).
#[derive(Clone, Copy)]
pub(crate) enum Target<'a> {
    /// The S3-compatible bucket.
    Bucket(&'a Storage),
    /// A local folder.
    Local(&'a Path),
}

/// What a HEAD request reports about one object.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Head {
    size: u64,
    etag: Option<String>,
    last_modified: Option<i64>,
}

/// Everything a management operation needs. The core builds one per call.
pub(crate) struct Ctx<'a> {
    /// The catalogue.
    pub(crate) library: &'a Library,
    /// The runtime that S3 calls and catalogue re-reads block on.
    pub(crate) rt: &'a Runtime,
    /// Where the objects are.
    pub(crate) target: Target<'a>,
    /// Confines every key to the prefix.
    pub(crate) guard: KeyGuard,
    /// The bucket's offline cache, `None` for a local folder.
    pub(crate) cache: Option<Arc<TrackCache>>,
    /// Where catalogue re-reads fetch tags and sidecars from.
    pub(crate) source: Arc<dyn MetaSource>,
    /// Folder for the temporary copy a tag edit works on.
    pub(crate) temp_dir: &'a Path,
}

impl Ctx<'_> {
    /// HEAD of one object. `None` when it does not exist.
    fn head(&self, key: &str) -> CoreResult<Option<Head>> {
        match self.target {
            Target::Bucket(storage) => {
                let meta = self.rt.block_on(storage.head_object_meta(key))?;
                Ok(meta.map(|m| Head {
                    size: m.size,
                    etag: m.etag,
                    last_modified: m.last_modified,
                }))
            }
            Target::Local(root) => match fs::metadata(root.join(key)) {
                Ok(meta) if meta.is_file() => Ok(Some(Head {
                    size: meta.len(),
                    etag: None,
                    last_modified: mtime_secs(&meta),
                })),
                Ok(_) => Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            },
        }
    }

    /// The direct contents of the folder `folder` (a key ending with `/`, or `""` for the root).
    fn list_folder(&self, folder: &str) -> CoreResult<FolderListing> {
        match self.target {
            Target::Bucket(storage) => Ok(self.rt.block_on(storage.list_folder(folder))?),
            Target::Local(root) => local_folder(root, folder),
        }
    }

    /// Every object under `folder`, in any subfolder, sorted by key. Folders are not included.
    fn list_all(&self, folder: &str) -> CoreResult<Vec<ObjectInfo>> {
        match self.target {
            Target::Bucket(storage) => Ok(self.rt.block_on(storage.list(folder))?),
            Target::Local(root) => local_walk(root, folder),
        }
    }

    /// Copies `src` to `dst`, which must not exist.
    fn copy(&self, src: &str, dst: &str) -> CoreResult<()> {
        match self.target {
            Target::Bucket(storage) => {
                self.rt.block_on(storage.copy_object(src, dst))?;
                Ok(())
            }
            Target::Local(root) => {
                let to = root.join(dst);
                create_parent(&to)?;
                fs::copy(root.join(src), to)?;
                Ok(())
            }
        }
    }

    /// Removes the objects `keys`. Returns the keys that could not be removed, with the reason.
    /// Missing objects count as removed. Local folders left empty by the removal are removed too.
    fn delete(&self, keys: &[String]) -> CoreResult<Vec<FailureInfo>> {
        match self.target {
            Target::Bucket(storage) => {
                let failed = self.rt.block_on(storage.delete_objects(keys))?;
                Ok(failed
                    .into_iter()
                    .map(|(key, message)| FailureInfo { key, message })
                    .collect())
            }
            Target::Local(root) => {
                let mut failed = Vec::new();
                for key in keys {
                    let path = root.join(key);
                    match fs::remove_file(&path) {
                        Ok(()) => remove_empty_parents(root, &path),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => failed.push(FailureInfo {
                            key: key.clone(),
                            message: e.to_string(),
                        }),
                    }
                }
                Ok(failed)
            }
        }
    }

    /// Writes the whole object `key` (of `size` bytes) to the local file `dest`. A bucket object
    /// is streamed in `DOWNLOAD_CHUNK` range reads, so memory stays bounded.
    fn download(&self, key: &str, size: u64, dest: &Path) -> CoreResult<()> {
        match self.target {
            Target::Bucket(storage) => {
                let mut file = File::create(dest)?;
                let mut offset: u64 = 0;
                while offset < size {
                    let len = (size - offset).min(DOWNLOAD_CHUNK);
                    let chunk = self.rt.block_on(storage.get_range(key, offset, len))?;
                    if chunk.is_empty() {
                        return Err(CoreError::Storage {
                            detail: "an object is shorter than its listed size".to_owned(),
                        });
                    }
                    file.write_all(&chunk)?;
                    offset += chunk.len() as u64;
                }
                Ok(())
            }
            Target::Local(root) => {
                fs::copy(root.join(key), dest)?;
                Ok(())
            }
        }
    }

    /// Stores the local file `src` as the object `key`, replacing an object with that key.
    fn upload(&self, src: &Path, key: &str) -> CoreResult<()> {
        match self.target {
            Target::Bucket(storage) => {
                self.rt
                    .block_on(storage.upload_file(src, key, None, |_| {}))?;
                Ok(())
            }
            Target::Local(root) => {
                let to = root.join(key);
                create_parent(&to)?;
                fs::copy(src, to)?;
                Ok(())
            }
        }
    }

    /// Stores `bytes` as the object `key` with the given content type, replacing any object there.
    fn put(&self, key: &str, bytes: Vec<u8>, content_type: &str) -> CoreResult<()> {
        match self.target {
            Target::Bucket(storage) => {
                self.rt
                    .block_on(storage.put_bytes(key, Bytes::from(bytes), Some(content_type)))?;
                Ok(())
            }
            Target::Local(root) => {
                let to = root.join(key);
                create_parent(&to)?;
                fs::write(to, bytes)?;
                Ok(())
            }
        }
    }

    /// Re-reads one audio object into the catalogue (see `scan_key`).
    fn rescan(&self, key: &str) -> CoreResult<bool> {
        let source = Arc::clone(&self.source);
        Ok(self.rt.block_on(scan_key(self.library, source, key))?)
    }
}

/// Modification time of `meta` in Unix seconds.
fn mtime_secs(meta: &fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

/// Creates the parent folders of `path`.
fn create_parent(path: &Path) -> CoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Removes the folders above `file`, up to but not including `root`, while they are empty.
fn remove_empty_parents(root: &Path, file: &Path) {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// The direct contents of a local folder, with keys relative to `root`.
fn local_folder(root: &Path, folder: &str) -> CoreResult<FolderListing> {
    let mut listing = FolderListing::default();
    let entries = match fs::read_dir(root.join(folder)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(listing),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            listing.folders.push(format!("{folder}{name}/"));
        } else if file_type.is_file() {
            let meta = entry.metadata()?;
            listing.objects.push(ObjectInfo {
                key: format!("{folder}{name}"),
                size: meta.len(),
                etag: None,
                last_modified: mtime_secs(&meta),
            });
        }
    }
    listing.folders.sort();
    listing.objects.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(listing)
}

/// Every file under the local folder `folder`, in any subfolder, sorted by key.
fn local_walk(root: &Path, folder: &str) -> CoreResult<Vec<ObjectInfo>> {
    let mut out = Vec::new();
    let mut pending = vec![folder.to_owned()];
    while let Some(dir) = pending.pop() {
        let listing = local_folder(root, &dir)?;
        out.extend(listing.objects);
        pending.extend(listing.folders);
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

/// A key's sidecar, if it is an audio object with one in its folder.
fn sidecar_of(ctx: &Ctx<'_>, key: &str) -> CoreResult<Option<ObjectInfo>> {
    if !is_audio_key(key) {
        return Ok(None);
    }
    let listing = ctx.list_folder(parent_of(key))?;
    Ok(claim_sidecars(&listing.objects).remove(key))
}

/// Sorts entries by name, case-insensitively, ties broken by the exact name.
fn sort_entries(entries: &mut [BucketEntryInfo]) {
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// The bucket browser's view of the folder `path` (relative to the prefix).
pub(crate) fn browse(ctx: &Ctx<'_>, path: &str) -> CoreResult<BucketFolderInfo> {
    let folder = ctx.guard.folder_key(path)?;
    ctx.guard.read(&folder)?;
    let listing = ctx.list_folder(&folder)?;
    let at_root = folder == ctx.guard.prefix();

    let mut folders = Vec::new();
    for key in listing.folders {
        let name = name_of(&key).to_owned();
        if at_root && name == INTERNAL_DIR {
            continue;
        }
        folders.push(BucketEntryInfo {
            path: ctx.guard.relative(&key).to_owned(),
            name,
            key,
            is_folder: true,
            size: 0,
            modified_ms: None,
            track_id: None,
        });
    }

    let mut files = Vec::new();
    for object in listing.objects {
        let track_id = ctx.library.track_by_key(&object.key)?.map(|t| t.id);
        files.push(BucketEntryInfo {
            name: name_of(&object.key).to_owned(),
            path: ctx.guard.relative(&object.key).to_owned(),
            modified_ms: object.last_modified.map(|s| s.saturating_mul(1000)),
            key: object.key,
            is_folder: false,
            size: object.size,
            track_id,
        });
    }

    sort_entries(&mut folders);
    sort_entries(&mut files);
    Ok(BucketFolderInfo {
        path: ctx.guard.relative(&folder).to_owned(),
        folders,
        files,
    })
}

/// Renames the object `key` to `new_name` in the same folder and returns its new key. A rename to
/// the same name changes nothing. Refuses a name that turns an audio file into a non-audio one.
/// The track's `.lrc` sidecar moves with it.
pub(crate) fn rename(ctx: &Ctx<'_>, key: &str, new_name: &str) -> CoreResult<String> {
    ctx.guard.object(key)?;
    check_name(new_name)?;
    let dst = format!("{}{new_name}", parent_of(key));
    ctx.guard.object(&dst)?;
    if dst == key {
        return Ok(dst);
    }
    if is_audio_key(key) && !is_audio_key(&dst) {
        return Err(refused("a track must keep an audio file extension"));
    }
    let sidecar = sidecar_of(ctx, key)?;
    relocate(ctx, key, &dst, sidecar.as_ref())?;
    Ok(dst)
}

/// Moves the objects `keys` into the folder `folder` (relative to the prefix, `""` for the root),
/// keeping each folder's own structure. Each track takes its `.lrc` sidecar with it. Objects that
/// fail are listed and stay where they were; the rest still move.
///
/// Refused before anything moves: a key outside the prefix, the root, the internal folder, or a
/// folder moved into itself or into one of its own subfolders. A key inside a listed folder moves
/// with that folder.
pub(crate) fn move_keys(ctx: &Ctx<'_>, keys: &[String], folder: &str) -> CoreResult<MoveReport> {
    let target = ctx.guard.folder_key(folder)?;
    ctx.guard.dir(&target)?;
    let mut sources: BTreeSet<String> = BTreeSet::new();
    for key in keys {
        if key.ends_with('/') {
            ctx.guard.folder(key)?;
            if target.starts_with(key.as_str()) {
                return Err(refused(
                    "a folder cannot be moved into itself or into one of its subfolders",
                ));
            }
        } else {
            ctx.guard.object(key)?;
        }
        sources.insert(key.clone());
    }
    let folders: Vec<String> = sources
        .iter()
        .filter(|k| k.ends_with('/'))
        .cloned()
        .collect();
    let sources: Vec<String> = sources
        .into_iter()
        .filter(|k| k.ends_with('/') || !folders.iter().any(|f| k.starts_with(f.as_str())))
        .collect();

    let mut report = MoveReport {
        moved: 0,
        failed: Vec::new(),
    };
    for source in &sources {
        if source.ends_with('/') {
            move_folder(ctx, source, &target, &mut report);
        } else {
            move_file(ctx, source, &target, &mut report);
        }
    }
    Ok(report)
}

/// Records a failed object in `report`.
fn fail(report: &mut MoveReport, key: &str, error: &CoreError) {
    report.failed.push(FailureInfo {
        key: key.to_owned(),
        message: error.to_string(),
    });
}

/// Moves one file into `target`, unless it is already there.
fn move_file(ctx: &Ctx<'_>, source: &str, target: &str, report: &mut MoveReport) {
    let dst = format!("{target}{}", name_of(source));
    if dst == source {
        return;
    }
    match sidecar_of(ctx, source).and_then(|sidecar| relocate(ctx, source, &dst, sidecar.as_ref()))
    {
        Ok(n) => report.moved += n,
        Err(e) => fail(report, source, &e),
    }
}

/// Moves every object under the folder `folder` into `target`, keeping its subfolders. The
/// folder's own name is kept as well: `A/B/` moved into `X/` lands at `X/B/`.
fn move_folder(ctx: &Ctx<'_>, folder: &str, target: &str, report: &mut MoveReport) {
    let dest_root = format!("{target}{}/", name_of(folder));
    if dest_root == folder {
        return;
    }
    let objects = match ctx.list_all(folder) {
        Ok(objects) => objects,
        Err(e) => return fail(report, folder, &e),
    };
    let claims = claim_sidecars(&objects);
    let claimed: HashSet<&str> = claims.values().map(|l| l.key.as_str()).collect();
    for object in &objects {
        // A sidecar moves with its track, below. Skipping it here keeps it from moving twice.
        if claimed.contains(object.key.as_str()) {
            continue;
        }
        let dst = format!("{dest_root}{}", &object.key[folder.len()..]);
        let sidecar = claims.get(&object.key);
        match relocate(ctx, &object.key, &dst, sidecar) {
            Ok(n) => report.moved += n,
            Err(e) => fail(report, &object.key, &e),
        }
    }
}

/// Moves object `src` to `dst`, and its `.lrc` sidecar (when given) to the same stem.
///
/// Copies first and checks each copy's size. Then it removes the originals, and last it re-keys
/// the catalogue (see `rekey_track`). A failure before the originals are removed deletes the
/// copies, so the object is never in two places. Returns the number of objects moved: 1, or 2
/// with a sidecar.
fn relocate(ctx: &Ctx<'_>, src: &str, dst: &str, sidecar: Option<&ObjectInfo>) -> CoreResult<u32> {
    ctx.guard.object(src)?;
    ctx.guard.object(dst)?;
    for key in [src, dst] {
        if uploads::key_is_reserved(ctx.library, key)? {
            return Err(refused(format!(
                "an upload that writes {key} is pending; finish or cancel it first"
            )));
        }
    }
    let src_head = ctx.head(src)?.ok_or_else(|| CoreError::NotFound {
        detail: src.to_owned(),
    })?;
    if ctx.head(dst)?.is_some() || ctx.library.track_by_key(dst)?.is_some() {
        return Err(refused(format!("{dst} already exists")));
    }
    let sidecar_dst = match sidecar {
        Some(lrc) => {
            let key = format!("{}.{}", stem_of(dst), ext_of(&lrc.key).unwrap_or("lrc"));
            ctx.guard.object(&key)?;
            if ctx.head(&key)?.is_some() {
                return Err(refused(format!("{key} already exists")));
            }
            Some(key)
        }
        None => None,
    };

    let mut made: Vec<String> = Vec::new();
    let copies = copy_pair(
        ctx,
        src,
        src_head.size,
        dst,
        sidecar,
        sidecar_dst.as_deref(),
        &mut made,
    );
    let (dst_head, sidecar_head) = match copies {
        Ok(heads) => heads,
        Err(e) => {
            discard(ctx, &made);
            return Err(e);
        }
    };

    let mut originals = vec![src.to_owned()];
    originals.extend(sidecar.map(|l| l.key.clone()));
    let failed = match ctx.delete(&originals) {
        Ok(failed) => failed,
        Err(e) => {
            discard(ctx, &made);
            return Err(e);
        }
    };
    if let Some(original) = failed.iter().find(|f| f.key == src) {
        discard(ctx, &made);
        return Err(CoreError::Storage {
            detail: format!("could not remove the old object: {}", original.message),
        });
    }
    for left_behind in failed.iter().filter(|f| f.key != src) {
        log::warn!(
            "an old sidecar could not be removed after a move: {}",
            left_behind.message
        );
    }

    let stamp = ObjectStamp {
        key: dst.to_owned(),
        size: dst_head.size,
        etag: dst_head.etag,
        last_modified: dst_head.last_modified,
    };
    let sidecar_stamp = match (sidecar_head, sidecar_dst) {
        (Some(head), Some(key)) => Some(ObjectStamp {
            key,
            size: head.size,
            etag: head.etag,
            last_modified: head.last_modified,
        }),
        _ => None,
    };
    rekey_track(ctx.library, src, dst, &stamp, sidecar_stamp.as_ref())?;
    if let Some(cache) = &ctx.cache
        && let Err(e) = cache.remove(src, src_head.size)
    {
        log::warn!("could not drop the offline cache entry of a moved track: {e}");
    }
    Ok(1 + u32::from(sidecar_stamp.is_some()))
}

/// Copies `src` to `dst`, and the sidecar to `sidecar_dst`, checking each copy's size. Every key
/// created is pushed to `made` first, so the caller can remove it on failure.
fn copy_pair(
    ctx: &Ctx<'_>,
    src: &str,
    src_size: u64,
    dst: &str,
    sidecar: Option<&ObjectInfo>,
    sidecar_dst: Option<&str>,
    made: &mut Vec<String>,
) -> CoreResult<(Head, Option<Head>)> {
    ctx.copy(src, dst)?;
    made.push(dst.to_owned());
    let dst_head = verified_copy(ctx, dst, src_size)?;
    let sidecar_head = match (sidecar, sidecar_dst) {
        (Some(lrc), Some(to)) => {
            ctx.copy(&lrc.key, to)?;
            made.push(to.to_owned());
            Some(verified_copy(ctx, to, lrc.size)?)
        }
        _ => None,
    };
    Ok((dst_head, sidecar_head))
}

/// HEAD of a fresh copy, which must exist and have `size` bytes.
fn verified_copy(ctx: &Ctx<'_>, key: &str, size: u64) -> CoreResult<Head> {
    let head = ctx.head(key)?.ok_or_else(|| CoreError::Storage {
        detail: "a copy is missing right after it was made".to_owned(),
    })?;
    if head.size != size {
        return Err(CoreError::Storage {
            detail: format!("a copy has {} bytes, expected {size}", head.size),
        });
    }
    Ok(head)
}

/// Removes copies left behind by a failed move. Failures are logged by count, never by key.
fn discard(ctx: &Ctx<'_>, keys: &[String]) {
    if keys.is_empty() {
        return;
    }
    match ctx.delete(keys) {
        Ok(failed) if failed.is_empty() => {}
        Ok(failed) => log::warn!(
            "{} copies could not be removed after a failed move",
            failed.len()
        ),
        Err(e) => log::warn!("removing copies after a failed move failed: {e}"),
    }
}

/// Deletes the catalogue rows of tracks whose objects were removed: the track rows (with their
/// sidecar rows and search rows), the finished upload rows of those keys, and the offline cache
/// entries of their audio. Playlist items, favorites and history keep the old key, so a later
/// upload of the same key brings them back. Then the catalogue is pruned.
fn forget_removed(ctx: &Ctx<'_>, keys: &[String]) -> CoreResult<()> {
    let mut removed: Vec<(String, u64)> = Vec::new();
    for key in keys {
        if let Some(track) = ctx.library.track_by_key(key)? {
            removed.push((key.clone(), track.size));
        }
        forget_uploads(ctx.library, key)?;
    }
    if let Some(cache) = &ctx.cache {
        for (key, size) in &removed {
            if let Err(e) = cache.remove(key, *size) {
                log::warn!("could not drop an offline cache entry of a deleted track: {e}");
            }
        }
    }
    if !keys.is_empty() {
        ctx.library.remove_tracks(keys)?;
        ctx.library.prune()?;
    }
    Ok(())
}

/// The `.lrc` sidecar that goes with each track in `folder`, keyed by the track, cached per folder
/// so a folder is listed once per call.
fn sidecar_claims<'m>(
    ctx: &Ctx<'_>,
    cache: &'m mut HashMap<String, HashMap<String, ObjectInfo>>,
    folder: &str,
) -> CoreResult<&'m HashMap<String, ObjectInfo>> {
    use std::collections::hash_map::Entry;
    Ok(match cache.entry(folder.to_owned()) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            let listing = ctx.list_folder(folder)?;
            entry.insert(claim_sidecars(&listing.objects))
        }
    })
}

/// Deletes the objects `keys`, and the `.lrc` sidecar of each track among them. Each key must be
/// an object (a file), not a folder: use `delete_folder` for folders. The catalogue rows of tracks
/// that were deleted are removed (see `forget_removed`). Tracks that could not be deleted stay
/// catalogued.
pub(crate) fn delete_keys(ctx: &Ctx<'_>, keys: &[String]) -> CoreResult<DeleteReport> {
    let mut unique: BTreeSet<&str> = BTreeSet::new();
    for key in keys {
        ctx.guard.object(key)?;
        unique.insert(key.as_str());
    }
    let mut claims: HashMap<String, HashMap<String, ObjectInfo>> = HashMap::new();
    let mut targets: Vec<String> = Vec::new();
    let mut tracks: Vec<String> = Vec::new();
    for key in unique {
        targets.push(key.to_owned());
        if is_audio_key(key) {
            tracks.push(key.to_owned());
            let folder = parent_of(key).to_owned();
            let sidecar = sidecar_claims(ctx, &mut claims, &folder)?
                .get(key)
                .map(|l| l.key.clone());
            targets.extend(sidecar);
        }
    }
    let failed = ctx.delete(&targets)?;
    let failed_keys: HashSet<&str> = failed.iter().map(|f| f.key.as_str()).collect();
    let gone: Vec<String> = tracks
        .into_iter()
        .filter(|k| !failed_keys.contains(k.as_str()))
        .collect();
    forget_removed(ctx, &gone)?;
    Ok(DeleteReport {
        deleted: deleted_count(targets.len(), &failed),
        failed,
    })
}

/// Deletes every object under the folder `path` (relative to the prefix), in any subfolder, then
/// cleans up the catalogue as `delete_keys` does. The root can never be deleted.
pub(crate) fn delete_folder(ctx: &Ctx<'_>, path: &str) -> CoreResult<DeleteReport> {
    let folder = ctx.guard.folder_key(path)?;
    ctx.guard.folder(&folder)?;
    let objects = ctx.list_all(&folder)?;
    let mut keys = Vec::with_capacity(objects.len());
    for object in &objects {
        ctx.guard.object(&object.key)?;
        keys.push(object.key.clone());
    }
    let failed = ctx.delete(&keys)?;
    let failed_keys: HashSet<&str> = failed.iter().map(|f| f.key.as_str()).collect();
    let gone: Vec<String> = keys
        .iter()
        .filter(|k| is_audio_key(k) && !failed_keys.contains(k.as_str()))
        .cloned()
        .collect();
    forget_removed(ctx, &gone)?;
    Ok(DeleteReport {
        deleted: deleted_count(keys.len(), &failed),
        failed,
    })
}

/// How many of `total` requested deletions succeeded, given the failures.
fn deleted_count(total: usize, failed: &[FailureInfo]) -> u32 {
    u32::try_from(total.saturating_sub(failed.len())).unwrap_or(u32::MAX)
}

/// A temporary local file, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    /// A file name in `dir` unique to this process and moment, with extension `ext`.
    fn new(dir: &Path, stem: &str, ext: &str) -> CoreResult<Self> {
        fs::create_dir_all(dir)?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Ok(Self(dir.join(format!(
            "{stem}-{}-{nanos}.{ext}",
            std::process::id()
        ))))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.0)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("could not remove a temporary tag-edit file: {e}");
        }
    }
}

/// Writes `edit` into the tags of catalogued track `track_id`, and stores the result under the
/// same key.
///
/// The object is downloaded to a temporary file in `ctx.temp_dir`, streamed in range reads. The
/// tags are written there, the file is uploaded back to the same key (multipart above 8 MiB),
/// and the track is re-read into the catalogue. Both cache entries of the key (the old size and
/// the new one) are dropped, and the temporary file is removed on every path. An edit with no
/// field set changes nothing. Refused while an upload of the same key is pending, since that
/// upload would overwrite the edit.
pub(crate) fn edit_tags(ctx: &Ctx<'_>, track_id: i64, edit: &TagEdit) -> CoreResult<()> {
    let track = ctx.library.track(track_id)?;
    ctx.guard.object(&track.key)?;
    let head = ctx.head(&track.key)?.ok_or_else(|| CoreError::NotFound {
        detail: format!("track {track_id}"),
    })?;
    if *edit == TagEdit::default() {
        return Ok(());
    }
    if uploads::key_is_reserved(ctx.library, &track.key)? {
        return Err(refused(
            "an upload that writes this file is pending; finish or cancel it first",
        ));
    }
    let ext = ext_of(&track.key)
        .filter(|e| e.bytes().all(|b| b.is_ascii_alphanumeric()))
        .unwrap_or("bin");
    let temp = TempFile::new(ctx.temp_dir, &format!("tag-edit-{track_id}"), ext)?;
    ctx.download(&track.key, head.size, temp.path())?;
    write_tags(temp.path(), edit)?;
    ctx.upload(temp.path(), &track.key)?;
    let new_size = ctx.head(&track.key)?.map_or(head.size, |h| h.size);
    ctx.rescan(&track.key)?;
    // A new title or album can leave the old album without tracks.
    ctx.library.prune()?;
    if let Some(cache) = &ctx.cache {
        for size in [head.size, new_size] {
            if let Err(e) = cache.remove(&track.key, size) {
                log::warn!("could not drop the offline cache entry of an edited track: {e}");
            }
        }
    }
    Ok(())
}

/// A validated cover image, from [`read_cover`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cover {
    /// The image bytes.
    pub(crate) bytes: Vec<u8>,
    /// The folder file name the image is stored under: `cover.jpg` or `cover.png`.
    pub(crate) name: &'static str,
    /// Its content type.
    pub(crate) mime: &'static str,
}

/// Reads a cover image of at most 20 MiB from `reader` and checks its magic bytes: JPEG or PNG.
/// At most 20 MiB + 1 byte is read, so an oversized image is detected without reading all of it.
/// Refuses an empty image, an oversized one, and any other format.
pub(crate) fn read_cover<R: Read>(reader: R) -> CoreResult<Cover> {
    let mut bytes = Vec::new();
    reader.take(MAX_COVER_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        return Err(refused("the cover image is empty"));
    }
    if bytes.len() as u64 > MAX_COVER_BYTES {
        return Err(refused("the cover image is larger than 20 MiB"));
    }
    if bytes.starts_with(JPEG_MAGIC) {
        Ok(Cover {
            bytes,
            name: "cover.jpg",
            mime: "image/jpeg",
        })
    } else if bytes.starts_with(PNG_MAGIC) {
        Ok(Cover {
            bytes,
            name: "cover.png",
            mime: "image/png",
        })
    } else {
        Err(refused("the file is neither a JPEG nor a PNG image"))
    }
}

/// Sets album `album_id`'s cover to `cover`. The image is stored as `cover.jpg` or `cover.png` in
/// the album's folder, and the other type's `cover.*` (and any other spelling of the two names,
/// such as `Cover.PNG`) is removed, so one name wins in the folder. The album's catalogue cover is
/// then set to the image (see `set_album_cover` in the library), and tracks without their own
/// cover follow it.
///
/// Refused when the album has no tracks, or when its tracks are in more than one folder.
pub(crate) fn set_album_cover(ctx: &Ctx<'_>, album_id: i64, cover: &Cover) -> CoreResult<()> {
    ctx.library.album(album_id)?;
    let tracks = ctx.library.album_tracks(album_id)?;
    let first = tracks
        .first()
        .ok_or_else(|| refused("the album has no tracks"))?;
    let folder = parent_of(&first.key).to_owned();
    if tracks.iter().any(|t| parent_of(&t.key) != folder) {
        return Err(refused("the album's tracks are in more than one folder"));
    }
    ctx.guard.dir(&folder)?;
    let cover_key = format!("{folder}{}", cover.name);
    let listing = ctx.list_folder(&folder)?;
    let stale: Vec<String> = listing
        .objects
        .iter()
        .filter(|o| {
            let lower = name_of(&o.key).to_ascii_lowercase();
            (lower == "cover.jpg" || lower == "cover.png") && o.key != cover_key
        })
        .map(|o| o.key.clone())
        .collect();

    ctx.put(&cover_key, cover.bytes.clone(), cover.mime)?;
    let failed = ctx.delete(&stale)?;
    let cover_id = ctx.library.store_cover(&cover.bytes, cover.mime)?;
    set_cover_row(ctx.library, album_id, cover_id)?;
    ctx.library.prune()?;
    match failed.first() {
        None => Ok(()),
        Some(first) => Err(CoreError::Storage {
            detail: format!(
                "the new cover is stored, but {} other cover image(s) could not be removed: {}",
                failed.len(),
                first.message
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonorus_engine::cache::CACHE_CHUNK;
    use sonorus_library::DirSource;
    use sonorus_library::{LyricsSource, scan};

    /// A directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "sonorus-core-manage-{tag}-{}-{nanos}",
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

    fn fixtures() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../library/tests/fixtures"
        ))
    }

    fn copy_dir(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &to);
            } else {
                fs::copy(entry.path(), &to).unwrap();
            }
        }
    }

    /// A catalogue, a local music folder (a copy of the fixtures, scanned) and a runtime.
    struct Env {
        _dir: TempDir,
        root: PathBuf,
        temp: PathBuf,
        library: Library,
        rt: Runtime,
    }

    impl Env {
        fn new(tag: &str) -> Self {
            Self::with(tag, |_| {})
        }

        /// Like `new`, but `prepare` changes the copied folder before it is scanned.
        fn with(tag: &str, prepare: impl FnOnce(&Path)) -> Self {
            let dir = TempDir::new(tag);
            let root = dir.path().join("music");
            copy_dir(&fixtures(), &root);
            prepare(&root);
            let library = Library::open_in_memory(&dir.path().join("covers")).unwrap();
            let rt = Runtime::new().unwrap();
            let env = Self {
                temp: dir.path().join("tmp"),
                _dir: dir,
                root,
                library,
                rt,
            };
            let source: Arc<dyn MetaSource> = Arc::new(DirSource::new(&env.root));
            env.rt
                .block_on(scan(&env.library, source, "", 2, |_| {}))
                .unwrap();
            env
        }

        fn ctx(&self, cache: Option<Arc<TrackCache>>) -> Ctx<'_> {
            Ctx {
                library: &self.library,
                rt: &self.rt,
                target: Target::Local(&self.root),
                guard: KeyGuard::new(""),
                cache,
                source: Arc::new(DirSource::new(&self.root)),
                temp_dir: &self.temp,
            }
        }

        fn track_id(&self, key: &str) -> i64 {
            self.library.track_by_key(key).unwrap().expect(key).id
        }

        fn exists(&self, key: &str) -> bool {
            self.root.join(key).is_file()
        }
    }

    const FLAC: &str = "Artist A/Album One/01 - First Song.flac";
    const M4A: &str = "Artist A/Album One/02 - Second Song.m4a";
    const WAV: &str = "Artist B/Album Two/01 - Wave Song.wav";
    const LRC: &str = "Artist A/Album One/01 - First Song.lrc";

    fn strings(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| (*k).to_owned()).collect()
    }

    /// Writes a sidecar next to the FLAC fixture before the scan.
    fn with_lyrics(root: &Path) {
        fs::write(root.join(LRC), b"[00:00.00]hello").unwrap();
    }

    #[test]
    fn guard_accepts_keys_under_the_prefix() {
        let g = KeyGuard::new("music/");
        assert!(g.read("music/").is_ok(), "root");
        assert!(g.object("music/Artist/Album/01.flac").is_ok());
        assert!(g.folder("music/Artist/").is_ok());
        assert!(g.dir("music/").is_ok(), "root is a valid target folder");
        assert!(
            g.read("music/a b/c&d#e/é.flac").is_ok(),
            "unusual but legal names"
        );
    }

    #[test]
    fn guard_refuses_keys_outside_the_prefix() {
        let g = KeyGuard::new("music/");
        for key in ["other/a.flac", "music", "x", "", "musicx/a.flac"] {
            assert!(g.read(key).is_err(), "{key:?} must be refused");
            assert!(g.object(key).is_err(), "{key:?} must be refused");
        }
        assert!(
            matches!(g.read("other/a"), Err(CoreError::Refused { .. })),
            "refusals are Refused"
        );
    }

    #[test]
    fn guard_refuses_dot_dot_empty_and_dot_segments() {
        let g = KeyGuard::new("music/");
        for key in [
            "music/../secret.flac",
            "music/a/../../x.flac",
            "music/./a.flac",
            "music/a//b.flac",
            "music//a.flac",
            "music/..",
            "music/a/.",
            "music/a/..",
        ] {
            assert!(g.read(key).is_err(), "{key:?} must be refused");
            assert!(g.object(key).is_err(), "{key:?} must be refused");
        }
        let root = KeyGuard::new("");
        assert!(root.read("../etc/passwd").is_err());
        assert!(root.read("a/../b").is_err());
    }

    #[test]
    fn guard_refuses_leading_slash_and_nul() {
        let g = KeyGuard::new("");
        assert!(g.read("/a.flac").is_err());
        assert!(g.object("a\0b.flac").is_err());
        let prefixed = KeyGuard::new("music/");
        assert!(prefixed.read("/music/a.flac").is_err());
    }

    #[test]
    fn root_folder_can_never_be_removed_renamed_or_moved() {
        let g = KeyGuard::new("music/");
        assert!(matches!(g.folder("music/"), Err(CoreError::Refused { .. })));
        let root = KeyGuard::new("");
        assert!(root.folder("").is_err());
        assert!(root.folder("/").is_err());
        assert!(g.folder("music/Artist/").is_ok());
    }

    #[test]
    fn folder_keys_must_end_with_a_slash_and_objects_must_not() {
        let g = KeyGuard::new("music/");
        assert!(g.folder("music/Artist").is_err());
        assert!(g.object("music/Artist/").is_err(), "a folder is not a file");
    }

    #[test]
    fn internal_folder_is_refused_for_writes_but_readable() {
        let g = KeyGuard::new("music/");
        for key in [
            "music/_sonorus/favorites.json",
            "music/_sonorus/",
            "music/_sonorus",
        ] {
            assert!(g.read(key).is_ok(), "{key:?} may be read");
            assert!(g.write(key).is_err(), "{key:?} must not be written");
            assert!(g.object(key).is_err(), "{key:?} must not be written");
        }
        assert!(g.dir("music/_sonorus/").is_err(), "not a move target");
        assert!(g.folder("music/_sonorus/").is_err(), "not deletable");
        assert!(
            g.object("music/Artist/_sonorus/x.flac").is_ok(),
            "only the top-level folder is internal"
        );
        let root = KeyGuard::new("");
        assert!(root.object("_sonorus/a.json").is_err());
    }

    #[test]
    fn folder_path_helpers() {
        let g = KeyGuard::new("music/");
        assert_eq!(g.folder_key("").unwrap(), "music/");
        assert_eq!(g.folder_key("A/B").unwrap(), "music/A/B/");
        assert_eq!(g.folder_key("A/B/").unwrap(), "music/A/B/");
        assert!(g.folder_key("/A").is_err());
        assert_eq!(g.relative("music/A/x.flac"), "A/x.flac");
        assert_eq!(parent_of("music/A/x.flac"), "music/A/");
        assert_eq!(parent_of("x.flac"), "");
        assert_eq!(name_of("music/A/"), "A");
        assert_eq!(name_of("music/A/x.flac"), "x.flac");
        assert_eq!(stem_of("a/b.c/d.flac"), "a/b.c/d");
        assert_eq!(stem_of("a/.hidden"), "a/.hidden");
        assert_eq!(ext_of("a/b.LRC"), Some("LRC"));
        assert_eq!(ext_of("a/.hidden"), None);
    }

    #[test]
    fn new_names_are_plain_file_names() {
        assert!(check_name("song.flac").is_ok());
        assert!(check_name("  ").is_err());
        assert!(check_name("").is_err());
        assert!(check_name("a/b.flac").is_err());
        assert!(check_name("a\0.flac").is_err());
        assert!(check_name(".").is_err());
        assert!(check_name("..").is_err());
        assert!(check_name(&"x".repeat(255)).is_ok());
        assert!(check_name(&"x".repeat(256)).is_err());
        let accented = "é".repeat(128);
        assert!(check_name(&accented).is_err(), "bytes, not characters");
    }

    #[test]
    fn sidecars_pair_with_one_track_each() {
        let obj = |key: &str| ObjectInfo {
            key: key.to_owned(),
            size: 1,
            etag: None,
            last_modified: None,
        };
        let objects = vec![
            obj("a/x.flac"),
            obj("a/x.m4a"),
            obj("a/x.LRC"),
            obj("a/y.wav"),
            obj("a/cover.jpg"),
        ];
        let claims = claim_sidecars(&objects);
        assert_eq!(
            claims.get("a/x.flac").map(|l| l.key.as_str()),
            Some("a/x.LRC")
        );
        assert!(!claims.contains_key("a/x.m4a"), "one sidecar, one track");
        assert!(!claims.contains_key("a/y.wav"));
    }

    #[test]
    fn cover_magic_and_size_are_checked() {
        let mut png = PNG_MAGIC.to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]);
        let cover = read_cover(png.as_slice()).unwrap();
        assert_eq!((cover.name, cover.mime), ("cover.png", "image/png"));

        let mut jpeg = JPEG_MAGIC.to_vec();
        jpeg.extend_from_slice(&[0xE0, 0x10]);
        let cover = read_cover(jpeg.as_slice()).unwrap();
        assert_eq!((cover.name, cover.mime), ("cover.jpg", "image/jpeg"));

        assert!(read_cover(&b"GIF89a..."[..]).is_err(), "GIF is refused");
        assert!(read_cover(&b""[..]).is_err(), "empty is refused");
        assert!(read_cover(&b"\x89PNG"[..]).is_err(), "truncated signature");
        let mut huge_jpeg = JPEG_MAGIC.to_vec();
        huge_jpeg.resize((MAX_COVER_BYTES + 1) as usize, 0);
        assert!(read_cover(huge_jpeg.as_slice()).is_err(), "over 20 MiB");
        let mut at_limit = JPEG_MAGIC.to_vec();
        at_limit.resize(MAX_COVER_BYTES as usize, 0);
        assert!(
            read_cover(at_limit.as_slice()).is_ok(),
            "exactly 20 MiB is accepted"
        );
    }

    #[test]
    fn browse_lists_folders_files_and_track_ids_and_hides_internal_folder() {
        let env = Env::new("browse");
        fs::create_dir_all(env.root.join("_sonorus")).unwrap();
        fs::write(env.root.join("_sonorus/favorites.json"), b"{}").unwrap();
        let ctx = env.ctx(None);

        let root = browse(&ctx, "").unwrap();
        assert_eq!(root.path, "");
        let names: Vec<&str> = root.folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Artist A", "Artist B"], "_sonorus is hidden");
        assert!(root.folders.iter().all(|f| f.is_folder && f.size == 0));

        let album = browse(&ctx, "Artist B/Album Two").unwrap();
        assert_eq!(album.path, "Artist B/Album Two/");
        let wav = album
            .files
            .iter()
            .find(|f| f.name == "01 - Wave Song.wav")
            .unwrap();
        assert_eq!(wav.key, WAV);
        assert_eq!(wav.path, "Artist B/Album Two/01 - Wave Song.wav");
        assert_eq!(wav.track_id, Some(env.track_id(WAV)));
        let cover = album.files.iter().find(|f| f.name == "cover.jpg").unwrap();
        assert_eq!(cover.track_id, None);
        assert!(cover.modified_ms.is_some());

        let sub = browse(&ctx, "_sonorus").unwrap();
        assert!(
            sub.files.iter().any(|f| f.name == "favorites.json"),
            "readable"
        );
        assert!(browse(&ctx, "../etc").is_err());
    }

    #[test]
    fn rename_moves_the_object_and_keeps_user_rows() {
        let env = Env::with("rename", with_lyrics);
        let id = env.track_id(FLAC);
        let lib = &env.library;
        lib.set_favorite(FLAC, true).unwrap();
        let playlist = lib.create_playlist("mix").unwrap();
        lib.add_to_playlist(playlist.id, &[FLAC.to_owned()])
            .unwrap();
        lib.record_play(FLAC, 5_000).unwrap();
        let ctx = env.ctx(None);

        let new_key = rename(&ctx, FLAC, "Renamed.flac").unwrap();
        assert_eq!(new_key, "Artist A/Album One/Renamed.flac");
        assert!(!env.exists(FLAC));
        assert!(env.exists(&new_key));
        assert!(
            env.root.join("Artist A/Album One/Renamed.lrc").is_file(),
            "sidecar moved"
        );
        assert!(!env.root.join(LRC).exists());

        assert_eq!(lib.track(id).unwrap().key, new_key, "same row");
        assert!(lib.is_favorite(&new_key).unwrap());
        let entries = lib.playlist_entries(playlist.id).unwrap();
        assert_eq!(entries[0].track.as_ref().map(|t| t.id), Some(id));
        assert_eq!(
            lib.recently_played(5).unwrap()[0].key,
            new_key,
            "history follows"
        );
        let lyrics = lib.track_lyrics(id).unwrap().expect("lyrics kept");
        assert_eq!(lyrics.source, LyricsSource::Sidecar);
    }

    #[test]
    fn rename_refuses_to_overwrite_and_keeps_audio_extension() {
        let env = Env::new("refuse");
        let ctx = env.ctx(None);
        let before = fs::read(env.root.join(FLAC)).unwrap();

        // A name that another object already has: the source is untouched.
        assert!(matches!(
            rename(&ctx, FLAC, "02 - Second Song.m4a"),
            Err(CoreError::Refused { .. })
        ));
        assert!(env.exists(M4A));
        assert!(env.exists(FLAC));

        // An audio file may not become a non-audio one.
        assert!(matches!(
            rename(&ctx, FLAC, "notes.txt"),
            Err(CoreError::Refused { .. })
        ));
        assert!(matches!(
            rename(&ctx, FLAC, "a/b.flac"),
            Err(CoreError::Refused { .. })
        ));
        assert_eq!(fs::read(env.root.join(FLAC)).unwrap(), before);

        // Folders are not renamed here.
        assert!(rename(&ctx, "Artist A/Album One/", "x.flac").is_err());
    }

    #[test]
    fn rename_to_the_same_name_changes_nothing() {
        let env = Env::new("same-name");
        let ctx = env.ctx(None);
        let name = name_of(FLAC).to_owned();
        assert_eq!(rename(&ctx, FLAC, &name).unwrap(), FLAC);
        assert!(env.exists(FLAC));
    }

    #[test]
    fn rename_refuses_while_an_upload_is_pending() {
        let env = Env::new("pending");
        uploads::insert(
            &env.library,
            &uploads::NewUpload {
                source_uri: "content://x".into(),
                display_name: "x.flac".into(),
                size: 1,
                blake3: "h".into(),
                key: FLAC.into(),
                content_type: "audio/flac".into(),
                state: uploads::UploadState::Queued,
            },
        )
        .unwrap();
        let ctx = env.ctx(None);
        assert!(matches!(
            rename(&ctx, FLAC, "Other.flac"),
            Err(CoreError::Refused { .. })
        ));
        assert!(env.exists(FLAC));
    }

    #[test]
    fn move_keeps_folder_structure_and_refuses_moving_into_itself() {
        let env = Env::new("move");
        let ctx = env.ctx(None);
        fs::write(
            env.root.join("Artist A/Album One/cover.jpg"),
            b"\xFF\xD8\xFFx",
        )
        .unwrap();

        let report = move_keys(&ctx, &strings(&["Artist A/"]), "Target/").unwrap();
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(report.moved, 3, "two tracks and the cover");
        assert!(env.exists("Target/Artist A/Album One/01 - First Song.flac"));
        assert!(env.exists("Target/Artist A/Album One/02 - Second Song.m4a"));
        assert!(env.exists("Target/Artist A/Album One/cover.jpg"));
        assert!(
            !env.root.join("Artist A").exists(),
            "empty source folders are removed"
        );
        assert!(
            env.library
                .track_by_key("Target/Artist A/Album One/01 - First Song.flac")
                .unwrap()
                .is_some(),
            "catalogue follows the move"
        );

        assert!(
            matches!(
                move_keys(&ctx, &strings(&["Target/"]), "Target/Sub/"),
                Err(CoreError::Refused { .. })
            ),
            "into itself"
        );
        assert!(
            matches!(
                move_keys(&ctx, &strings(&["Target/"]), "Target/Sub/Deeper"),
                Err(CoreError::Refused { .. })
            ),
            "into a descendant"
        );
        assert!(
            env.exists("Target/Artist A/Album One/01 - First Song.flac"),
            "nothing moved"
        );
        assert!(
            move_keys(&ctx, &strings(&["Target/"]), "").is_ok(),
            "into the root is a no-op"
        );
    }

    #[test]
    fn move_moves_a_track_with_its_sidecar() {
        let env = Env::with("move-lrc", with_lyrics);
        let id = env.track_id(FLAC);
        let ctx = env.ctx(None);
        let report = move_keys(&ctx, &strings(&[FLAC]), "Moved/").unwrap();
        assert_eq!(report.moved, 2, "track and sidecar");
        assert!(env.exists("Moved/01 - First Song.flac"));
        assert!(env.root.join("Moved/01 - First Song.lrc").is_file());
        assert!(!env.root.join(LRC).exists());
        assert_eq!(
            env.library.track(id).unwrap().key,
            "Moved/01 - First Song.flac"
        );
        assert!(
            env.library.track_lyrics(id).unwrap().is_some(),
            "lyrics follow"
        );
    }

    #[test]
    fn move_refuses_the_root_and_bad_targets_before_moving_anything() {
        let env = Env::new("move-refuse");
        let ctx = env.ctx(None);
        assert!(
            move_keys(&ctx, &strings(&[FLAC, ""]), "X/").is_err(),
            "root in keys"
        );
        assert!(
            move_keys(&ctx, &strings(&[FLAC]), "../x").is_err(),
            "bad target"
        );
        assert!(
            move_keys(&ctx, &strings(&[FLAC]), "_sonorus").is_err(),
            "internal target"
        );
        assert!(
            move_keys(&ctx, &strings(&["../x"]), "X/").is_err(),
            "bad key"
        );
        assert!(env.exists(FLAC));
    }

    #[test]
    fn move_reports_collisions_and_keeps_going() {
        let env = Env::new("move-collide");
        let ctx = env.ctx(None);
        fs::create_dir_all(env.root.join("Dest")).unwrap();
        fs::write(env.root.join("Dest/01 - First Song.flac"), b"other").unwrap();
        let report = move_keys(&ctx, &strings(&[FLAC, WAV]), "Dest/").unwrap();
        assert_eq!(report.moved, 1, "the wav still moves");
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].key, FLAC);
        assert!(report.failed[0].message.contains("already exists"));
        assert!(env.exists(FLAC), "the colliding source stays");
        assert_eq!(
            fs::read(env.root.join("Dest/01 - First Song.flac")).unwrap(),
            b"other"
        );
    }

    #[test]
    fn moving_a_file_into_its_own_folder_is_a_no_op() {
        let env = Env::new("move-noop");
        let ctx = env.ctx(None);
        let report = move_keys(&ctx, &strings(&[FLAC]), "Artist A/Album One/").unwrap();
        assert_eq!(
            report,
            MoveReport {
                moved: 0,
                failed: vec![]
            }
        );
        assert!(env.exists(FLAC));
    }

    #[test]
    fn delete_removes_catalogue_row_and_sidecar_but_keeps_playlist_items() {
        let env = Env::with("delete", with_lyrics);
        let lib = &env.library;
        let playlist = lib.create_playlist("keep").unwrap();
        lib.add_to_playlist(playlist.id, &[FLAC.to_owned()])
            .unwrap();
        lib.set_favorite(FLAC, true).unwrap();
        let ctx = env.ctx(None);

        let report = delete_keys(&ctx, &strings(&[FLAC])).unwrap();
        assert_eq!(
            report,
            DeleteReport {
                deleted: 2,
                failed: vec![]
            }
        );
        assert!(!env.exists(FLAC));
        assert!(!env.root.join(LRC).exists());
        assert!(
            lib.track_by_key(FLAC).unwrap().is_none(),
            "catalogue row removed"
        );
        let items = lib.playlist_entries(playlist.id).unwrap();
        assert_eq!(items.len(), 1, "playlist slot kept");
        assert!(items[0].track.is_none(), "and shown as unavailable");
        assert!(
            lib.is_favorite(FLAC).unwrap(),
            "favorite kept for a re-upload"
        );
    }

    #[test]
    fn delete_refuses_folders_and_internal_keys() {
        let env = Env::new("delete-refuse");
        let ctx = env.ctx(None);
        assert!(delete_keys(&ctx, &strings(&["Artist A/"])).is_err());
        assert!(delete_keys(&ctx, &strings(&["_sonorus/x.json"])).is_err());
        assert!(env.exists(FLAC));
    }

    #[test]
    fn delete_folder_removes_everything_below_and_refuses_the_root() {
        let env = Env::new("delete-folder");
        let ctx = env.ctx(None);
        assert!(matches!(
            delete_folder(&ctx, ""),
            Err(CoreError::Refused { .. })
        ));
        assert!(env.exists(FLAC), "root delete changed nothing");

        let report = delete_folder(&ctx, "Artist B").unwrap();
        assert_eq!(report.deleted, 3, "wav, untagged flac, cover");
        assert!(report.failed.is_empty());
        assert!(!env.root.join("Artist B").exists());
        assert!(env.library.track_by_key(WAV).unwrap().is_none());
        assert_eq!(env.library.stats().unwrap().albums, 1, "empty album pruned");
    }

    /// Stores a complete cache entry for `key` of `size` bytes through the public writer API.
    fn fill_cache(cache: &TrackCache, key: &str, size: u64) {
        let writer = cache.writer(key, size).unwrap();
        let mut offset = 0u64;
        while offset < size {
            let len = (size - offset).min(CACHE_CHUNK);
            writer
                .write_chunk(offset, &vec![7u8; len as usize])
                .unwrap();
            offset += len;
        }
        assert!(writer.is_complete());
    }

    #[test]
    fn delete_purges_the_offline_cache_entry() {
        let env = Env::new("cache-delete");
        let cache = TrackCache::open(env.temp.join("cache"), 10 * 1024 * 1024, None).unwrap();
        let size = fs::metadata(env.root.join(FLAC)).unwrap().len();
        fill_cache(&cache, FLAC, size);
        assert!(cache.is_complete(FLAC, size));
        let ctx = env.ctx(Some(Arc::clone(&cache)));
        delete_keys(&ctx, &strings(&[FLAC])).unwrap();
        assert!(!cache.is_complete(FLAC, size), "entry removed");
    }

    #[test]
    fn rename_purges_the_old_cache_entry() {
        let env = Env::new("cache-rename");
        let cache = TrackCache::open(env.temp.join("cache"), 10 * 1024 * 1024, None).unwrap();
        let size = fs::metadata(env.root.join(FLAC)).unwrap().len();
        fill_cache(&cache, FLAC, size);
        let ctx = env.ctx(Some(Arc::clone(&cache)));
        let new_key = rename(&ctx, FLAC, "Moved.flac").unwrap();
        assert!(!cache.is_complete(FLAC, size), "old key no longer cached");
        assert!(!cache.is_complete(&new_key, size));
    }

    #[test]
    fn edit_tags_changes_the_catalogued_title_and_removes_the_temp_file() {
        let env = Env::new("edit");
        let id = env.track_id(FLAC);
        let ctx = env.ctx(None);
        let edit = TagEdit {
            title: Some("Edited Title".into()),
            ..Default::default()
        };
        edit_tags(&ctx, id, &edit).unwrap();
        assert_eq!(env.library.track(id).unwrap().title, "Edited Title");
        assert_eq!(env.library.track(id).unwrap().key, FLAC, "same key");
        let leftovers: Vec<_> = fs::read_dir(&env.temp)
            .map(|rd| rd.filter_map(Result::ok).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "temp file removed");
    }

    #[test]
    fn edit_tags_with_nothing_to_change_does_not_touch_the_file() {
        let env = Env::new("edit-noop");
        let id = env.track_id(FLAC);
        let before = fs::read(env.root.join(FLAC)).unwrap();
        edit_tags(&env.ctx(None), id, &TagEdit::default()).unwrap();
        assert_eq!(fs::read(env.root.join(FLAC)).unwrap(), before);
    }

    #[test]
    fn edit_tags_refuses_while_an_upload_of_the_file_is_pending() {
        let env = Env::new("edit-pending");
        let id = env.track_id(FLAC);
        uploads::insert(
            &env.library,
            &uploads::NewUpload {
                source_uri: "content://x".into(),
                display_name: "x.flac".into(),
                size: 1,
                blake3: "pending".into(),
                key: FLAC.into(),
                content_type: "audio/flac".into(),
                state: uploads::UploadState::Failed,
            },
        )
        .unwrap();
        let edit = TagEdit {
            title: Some("Nope".into()),
            ..Default::default()
        };
        assert!(matches!(
            edit_tags(&env.ctx(None), id, &edit),
            Err(CoreError::Refused { .. })
        ));
        assert_eq!(env.library.track(id).unwrap().title, "First Song");
    }

    #[test]
    fn edit_tags_on_a_missing_track_is_not_found() {
        let env = Env::new("edit-missing");
        assert!(matches!(
            edit_tags(&env.ctx(None), 9_999, &TagEdit::default()),
            Err(CoreError::NotFound { .. })
        ));
    }

    /// A 1x1 PNG-shaped byte string: the signature and an IHDR chunk. Only the magic is checked.
    fn png_bytes() -> Vec<u8> {
        let mut out = PNG_MAGIC.to_vec();
        out.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        out.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        out
    }

    #[test]
    fn album_cover_writes_png_removes_the_other_type_and_sets_the_album_cover() {
        let env = Env::new("cover");
        let album = env.library.track(env.track_id(WAV)).unwrap().album_id;
        let folder = env.root.join("Artist B/Album Two");
        fs::write(folder.join("cover.jpg"), b"\xFF\xD8\xFFold").unwrap();
        fs::write(folder.join("Cover.JPG"), b"\xFF\xD8\xFFold2").unwrap();
        let png = png_bytes();
        let cover = read_cover(png.as_slice()).unwrap();

        set_album_cover(&env.ctx(None), album, &cover).unwrap();

        assert_eq!(fs::read(folder.join("cover.png")).unwrap(), png);
        assert!(!folder.join("cover.jpg").exists(), "other type removed");
        assert!(!folder.join("Cover.JPG").exists(), "other spelling removed");
        let stored = env
            .library
            .album(album)
            .unwrap()
            .cover_id
            .expect("cover set");
        let path = env.library.cover_path(stored).unwrap().expect("cover file");
        assert_eq!(fs::read(path).unwrap(), png);
    }

    #[test]
    fn album_cover_refuses_albums_across_folders_and_unknown_albums() {
        let env = Env::new("cover-refuse");
        let album = env.library.track(env.track_id(FLAC)).unwrap().album_id;
        fs::create_dir_all(env.root.join("Elsewhere")).unwrap();
        fs::copy(env.root.join(FLAC), env.root.join("Elsewhere/x.flac")).unwrap();
        let source: Arc<dyn MetaSource> = Arc::new(DirSource::new(&env.root));
        env.rt
            .block_on(scan(&env.library, source, "", 2, |_| {}))
            .unwrap();
        assert_eq!(
            env.library.album_tracks(album).unwrap().len(),
            3,
            "two folders now"
        );
        let cover = read_cover(png_bytes().as_slice()).unwrap();
        assert!(matches!(
            set_album_cover(&env.ctx(None), album, &cover),
            Err(CoreError::Refused { .. })
        ));
        assert!(!env.root.join("Artist A/Album One/cover.png").exists());
        assert!(matches!(
            set_album_cover(&env.ctx(None), 9_999, &cover),
            Err(CoreError::NotFound { .. })
        ));
    }

    #[test]
    fn tag_edit_from_the_api_record_maps_every_field() {
        let info = crate::types::TagEditInfo {
            title: Some("t".into()),
            artist: Some("a".into()),
            album: Some("al".into()),
            album_artist: Some("aa".into()),
            year: Some(2001),
            track_no: Some(3),
            disc_no: Some(1),
            genre: Some("g".into()),
        };
        let edit = TagEdit::from(info);
        assert_eq!(edit.album_artist.as_deref(), Some("aa"));
        assert_eq!(edit.year, Some(2001));
        assert_eq!(edit.genre.as_deref(), Some("g"));
        assert_eq!(edit.track_no, Some(3));
    }
}
