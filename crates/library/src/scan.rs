//! Bucket scan: diffs the object listing against the catalogue, reads tags for new or changed
//! audio objects concurrently, stores covers and upserts tracks.
//!
//! The diff and every database write run on the calling task. Only `MetaSource::read_meta` runs
//! in the background, with at most `concurrency` calls in flight.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use sonorus_storage::{Storage, is_audio_key};
use tokio::runtime::Handle;
use tokio::task::{Id, JoinError, JoinSet};

use crate::db::Library;
use crate::error::{LibraryError, Result};
use crate::model::{ObjectStamp, PruneStats, TrackMeta};
use crate::remote::RemoteReader;
use crate::tags;

/// A boxed, sendable future that may borrow from the value it was created from.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Running counters for one scan. `total` is the number of tracks to add or update; `done`
/// counts the ones finished so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanProgress {
    /// Tracks to add or update in this scan.
    pub total: u32,
    /// Tracks finished (stored or failed).
    pub done: u32,
    /// Tracks newly catalogued.
    pub added: u32,
    /// Existing tracks whose size or etag changed, or that were flagged for a re-read, and were
    /// read again.
    pub updated: u32,
    /// Catalogue tracks whose audio object is no longer listed.
    pub removed: u32,
    /// Tracks that could not be read or stored.
    pub failed: u32,
}

/// One object that could not be catalogued, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFailure {
    /// Object key of the failed track.
    pub key: String,
    /// Display text of the error.
    pub error: String,
}

/// Outcome of a completed scan.
#[derive(Debug, Clone)]
pub struct ScanReport {
    /// Final counters.
    pub progress: ScanProgress,
    /// Per-file failures, in completion order.
    pub failures: Vec<ScanFailure>,
    /// Rows removed by the final prune.
    pub prune: PruneStats,
}

/// Where a scan reads its objects from: the bucket in production, a local folder in tests and
/// the example.
pub trait MetaSource: Send + Sync + 'static {
    /// Lists ALL objects under `prefix` (audio and images).
    fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<ObjectStamp>>>;
    /// Reads tags, properties and embedded cover art of one audio object.
    fn read_meta(&self, stamp: &ObjectStamp) -> BoxFuture<'_, Result<TrackMeta>>;
    /// Fetches the whole object (used for folder cover images).
    fn get_bytes(&self, key: &str) -> BoxFuture<'_, Result<Vec<u8>>>;
}

/// A scan source backed by the S3-compatible bucket.
pub struct BucketSource {
    storage: Storage,
    rt: Handle,
}

impl BucketSource {
    /// Creates a source that lists and reads objects through `storage`. `rt` drives the range
    /// requests made by the blocking tag reader, so it must be the runtime running the scan.
    pub fn new(storage: Storage, rt: Handle) -> Self {
        Self { storage, rt }
    }
}

impl MetaSource for BucketSource {
    fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<ObjectStamp>>> {
        let prefix = prefix.to_owned();
        Box::pin(async move {
            let objects = self.storage.list(&prefix).await?;
            Ok(objects
                .into_iter()
                .map(|o| ObjectStamp {
                    key: o.key,
                    size: o.size,
                    etag: o.etag,
                    last_modified: o.last_modified,
                })
                .collect())
        })
    }

    fn read_meta(&self, stamp: &ObjectStamp) -> BoxFuture<'_, Result<TrackMeta>> {
        let storage = self.storage.clone();
        let rt = self.rt.clone();
        let key = stamp.key.clone();
        let size = stamp.size;
        Box::pin(async move {
            let ext = extension(&key);
            let joined = tokio::task::spawn_blocking(move || {
                tags::read_meta(RemoteReader::new(storage, key, size, rt), ext.as_deref())
            })
            .await;
            joined.map_err(join_error)?
        })
    }

    fn get_bytes(&self, key: &str) -> BoxFuture<'_, Result<Vec<u8>>> {
        let key = key.to_owned();
        Box::pin(async move { Ok(self.storage.get_bytes(&key).await?.to_vec()) })
    }
}

/// A scan source backed by a local folder. Keys are the '/'-joined paths relative to `root`.
pub struct DirSource {
    root: PathBuf,
}

impl DirSource {
    /// Creates a source rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl MetaSource for DirSource {
    fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<ObjectStamp>>> {
        let root = self.root.clone();
        let prefix = prefix.to_owned();
        Box::pin(async move {
            let listed = tokio::task::spawn_blocking(move || walk(&root, &prefix)).await;
            listed.map_err(join_error)?
        })
    }

    fn read_meta(&self, stamp: &ObjectStamp) -> BoxFuture<'_, Result<TrackMeta>> {
        let path = self.root.join(&stamp.key);
        let ext = extension(&stamp.key);
        Box::pin(async move {
            let read = tokio::task::spawn_blocking(move || -> Result<TrackMeta> {
                let file = fs::File::open(&path)?;
                tags::read_meta(file, ext.as_deref())
            })
            .await;
            read.map_err(join_error)?
        })
    }

    fn get_bytes(&self, key: &str) -> BoxFuture<'_, Result<Vec<u8>>> {
        let path = self.root.join(key);
        Box::pin(async move { Ok(tokio::fs::read(path).await?) })
    }
}

/// Lists every regular file under `root` whose relative key starts with `prefix`, sorted by key.
/// Directories are descended into; symlinks and special files are skipped.
fn walk(root: &Path, prefix: &str) -> Result<Vec<ObjectStamp>> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                dirs.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let Some(key) = relative_key(root, &path) else {
                log::warn!("skipping {}: name is not valid UTF-8", path.display());
                continue;
            };
            if !key.starts_with(prefix) {
                continue;
            }
            let meta = entry.metadata()?;
            let last_modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_secs()).ok());
            out.push(ObjectStamp {
                key,
                size: meta.len(),
                etag: None,
                last_modified,
            });
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

/// `path` relative to `root`, components joined with '/'. `None` if any component is not UTF-8.
fn relative_key(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

/// Lowercase extension of the last segment of `key`, if it has one.
fn extension(key: &str) -> Option<String> {
    let name = key.rsplit('/').next()?;
    let (_, ext) = name.rsplit_once('.')?;
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// MIME type for a folder cover, from its extension: PNG, otherwise JPEG.
fn mime_for_cover(key: &str) -> &'static str {
    match extension(key).as_deref() {
        Some("png") => "image/png",
        _ => "image/jpeg",
    }
}

fn join_error(err: JoinError) -> LibraryError {
    LibraryError::Io(io::Error::other(err))
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn record_failure(
    progress: &mut ScanProgress,
    failures: &mut Vec<ScanFailure>,
    key: String,
    error: &LibraryError,
) {
    progress.failed += 1;
    failures.push(ScanFailure {
        key,
        error: error.to_string(),
    });
}

/// Starts `read_meta` for `stamp` on the task set and remembers which key the task belongs to.
fn spawn_read(
    tasks: &mut JoinSet<(ObjectStamp, Result<TrackMeta>)>,
    task_keys: &mut HashMap<Id, String>,
    source: &Arc<dyn MetaSource>,
    stamp: ObjectStamp,
) {
    let key = stamp.key.clone();
    let src = Arc::clone(source);
    let handle = tasks.spawn(async move {
        let read = src.read_meta(&stamp).await;
        (stamp, read)
    });
    task_keys.insert(handle.id(), key);
}

/// Stores the track's cover (embedded, else folder image) and upserts the track row.
async fn store_track(
    library: &Library,
    source: &dyn MetaSource,
    all_keys: &HashSet<String>,
    folder_covers: &mut HashMap<String, Option<i64>>,
    stamp: &ObjectStamp,
    meta: &TrackMeta,
) -> Result<()> {
    let cover_id = match &meta.cover {
        Some(cover) => Some(library.store_cover(&cover.data, &cover.mime)?),
        None => match tags::folder_cover_key(&stamp.key, all_keys) {
            Some(cover_key) => folder_cover(library, source, &cover_key, folder_covers).await,
            None => None,
        },
    };
    library.upsert_track(stamp, meta, cover_id)?;
    Ok(())
}

/// Cover id for the folder image at `cover_key`, fetched and stored at most once per scan. A
/// failure is logged and cached as `None`, so the track is still catalogued without art.
async fn folder_cover(
    library: &Library,
    source: &dyn MetaSource,
    cover_key: &str,
    cache: &mut HashMap<String, Option<i64>>,
) -> Option<i64> {
    if let Some(cached) = cache.get(cover_key) {
        return *cached;
    }
    let id = match fetch_cover(library, source, cover_key).await {
        Ok(id) => Some(id),
        Err(err) => {
            log::warn!("folder cover {cover_key} skipped: {err}");
            None
        }
    };
    cache.insert(cover_key.to_owned(), id);
    id
}

async fn fetch_cover(library: &Library, source: &dyn MetaSource, cover_key: &str) -> Result<i64> {
    let data = source.get_bytes(cover_key).await?;
    library.store_cover(&data, mime_for_cover(cover_key))
}

/// Brings the catalogue in line with the objects under `prefix` on `source`.
///
/// Lists the source and removes catalogue tracks under `prefix` whose audio object is no longer
/// listed (tracks outside `prefix` are left alone). New
/// audio objects, existing ones whose size or etag changed, and existing ones flagged for a
/// re-read (see the v5 migration) are read with at most `concurrency` reads in flight (at least
/// one). Each result is stored in completion order, then
/// the catalogue is pruned of orphaned albums, artists and covers.
///
/// Per-file errors are recorded in the report and never abort the scan. Errors from listing,
/// the catalogue or pruning are returned.
///
/// `on_progress` is called after the diff, after each finished file, and once more after pruning.
pub async fn scan(
    library: &Library,
    source: Arc<dyn MetaSource>,
    prefix: &str,
    concurrency: usize,
    mut on_progress: impl FnMut(&ScanProgress) + Send,
) -> Result<ScanReport> {
    let concurrency = concurrency.max(1);

    let listed = source.list(prefix).await?;
    let all_keys: HashSet<String> = listed.iter().map(|s| s.key.clone()).collect();
    let audio: Vec<ObjectStamp> = listed
        .into_iter()
        .filter(|s| is_audio_key(&s.key))
        .collect();

    let existing = library.stamps()?;
    let stale = library.keys_needing_reread()?;
    let mut progress = ScanProgress::default();
    let mut failures = Vec::new();

    let listed_audio: HashSet<&str> = audio.iter().map(|s| s.key.as_str()).collect();
    // Only keys under `prefix` were listed, so only those can be judged as gone.
    let gone: Vec<String> = existing
        .keys()
        .filter(|k| k.starts_with(prefix) && !listed_audio.contains(k.as_str()))
        .cloned()
        .collect();
    if !gone.is_empty() {
        progress.removed = count(library.remove_tracks(&gone)?);
    }

    let pending: Vec<ObjectStamp> = audio
        .into_iter()
        .filter(|s| match existing.get(&s.key) {
            None => true,
            Some(old) => old.size != s.size || old.etag != s.etag || stale.contains(&s.key),
        })
        .collect();
    progress.total = count(pending.len());
    on_progress(&progress);

    let mut queue = pending.into_iter();
    let mut tasks: JoinSet<(ObjectStamp, Result<TrackMeta>)> = JoinSet::new();
    let mut task_keys: HashMap<Id, String> = HashMap::new();
    let mut folder_covers: HashMap<String, Option<i64>> = HashMap::new();

    for stamp in queue.by_ref().take(concurrency) {
        spawn_read(&mut tasks, &mut task_keys, &source, stamp);
    }

    while let Some(joined) = tasks.join_next_with_id().await {
        match joined {
            Ok((id, (stamp, read))) => {
                task_keys.remove(&id);
                let is_update = existing.contains_key(&stamp.key);
                let stored = match read {
                    Ok(meta) => {
                        store_track(
                            library,
                            source.as_ref(),
                            &all_keys,
                            &mut folder_covers,
                            &stamp,
                            &meta,
                        )
                        .await
                    }
                    Err(err) => Err(err),
                };
                match stored {
                    Ok(()) if is_update => progress.updated += 1,
                    Ok(()) => progress.added += 1,
                    Err(err) => record_failure(&mut progress, &mut failures, stamp.key, &err),
                }
            }
            Err(err) => {
                // The read task panicked; its stamp is lost, so recover the key by task id.
                let key = task_keys
                    .remove(&err.id())
                    .unwrap_or_else(|| "<unknown>".to_owned());
                record_failure(&mut progress, &mut failures, key, &join_error(err));
            }
        }
        progress.done += 1;
        on_progress(&progress);

        if let Some(stamp) = queue.next() {
            spawn_read(&mut tasks, &mut task_keys, &source, stamp);
        }
    }

    let prune = library.prune()?;
    on_progress(&progress);
    Ok(ScanReport {
        progress,
        failures,
        prune,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::TempDir;
    use crate::model::{Album, AlbumSort};

    fn fixtures() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"))
    }

    fn dir_source(root: &Path) -> Arc<dyn MetaSource> {
        Arc::new(DirSource::new(root))
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

    fn album(lib: &Library, title: &str, artist: &str) -> Album {
        lib.albums(AlbumSort::Title, 100, 0)
            .unwrap()
            .into_iter()
            .find(|a| a.title == title && a.artist == artist)
            .unwrap_or_else(|| panic!("no album {title:?} by {artist:?}"))
    }

    /// Wraps a `DirSource` but fails (or panics) for one key.
    struct FaultySource {
        inner: DirSource,
        bad_key: String,
        panic: bool,
    }

    impl MetaSource for FaultySource {
        fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<ObjectStamp>>> {
            self.inner.list(prefix)
        }

        fn read_meta(&self, stamp: &ObjectStamp) -> BoxFuture<'_, Result<TrackMeta>> {
            if stamp.key == self.bad_key {
                let key = stamp.key.clone();
                let panic = self.panic;
                return Box::pin(async move { faulty_read(&key, panic) });
            }
            self.inner.read_meta(stamp)
        }

        fn get_bytes(&self, key: &str) -> BoxFuture<'_, Result<Vec<u8>>> {
            self.inner.get_bytes(key)
        }
    }

    fn faulty_read(key: &str, panic: bool) -> Result<TrackMeta> {
        if panic {
            panic!("reader exploded for {key}");
        }
        Err(LibraryError::Tags(format!("cannot read {key}")))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn first_scan_catalogues_fixtures() {
        let tmp = TempDir::new();
        let lib = Library::open_in_memory(&tmp.path().join("covers")).unwrap();

        let mut seen = Vec::new();
        let report = scan(&lib, dir_source(&fixtures()), "", 4, |p| {
            seen.push(p.clone())
        })
        .await
        .unwrap();

        assert_eq!(report.progress.added, 4);
        assert_eq!(report.progress.failed, 0);
        assert_eq!(report.progress.removed, 0);
        assert_eq!(report.progress.total, 4);
        assert_eq!(report.progress.done, 4);
        assert!(report.failures.is_empty());
        assert_eq!(seen.first().map(|p| p.done), Some(0));
        assert_eq!(seen.last(), Some(&report.progress));

        let stats = lib.stats().unwrap();
        assert_eq!(stats.tracks, 4);

        let one = album(&lib, "Album One", "Artist A");
        assert_eq!(one.track_count, 2);
        assert!(one.cover_id.is_some(), "embedded cover");

        let two = album(&lib, "Album Two", "Artist B");
        assert_eq!(
            two.track_count, 2,
            "wav + untagged flac (artist from folder)"
        );
        let two_cover = two.cover_id.expect("folder cover");
        let cover_bytes = fs::read(fixtures().join("Artist B/Album Two/cover.jpg")).unwrap();
        let stored = lib.cover_path(two_cover).unwrap().expect("cover file");
        assert_eq!(fs::read(stored).unwrap(), cover_bytes);

        let untagged = lib
            .track_by_key("Artist B/Album Two/03 - Untagged Track.flac")
            .unwrap()
            .expect("untagged track");
        assert_eq!(untagged.title, "Untagged Track");
        assert_eq!(untagged.track_no, Some(3));
        assert_eq!(untagged.album, "Album Two");
        assert_eq!(untagged.album_artist, "Artist B");
        assert_eq!(untagged.album_id, two.id);
        assert_eq!(untagged.cover_id, two.cover_id);
        assert_eq!(stats.albums, 2);
        assert_eq!(stats.artists, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rescan_unchanged_is_a_no_op() {
        let tmp = TempDir::new();
        let lib = Library::open_in_memory(&tmp.path().join("covers")).unwrap();
        scan(&lib, dir_source(&fixtures()), "", 4, |_| {})
            .await
            .unwrap();

        let report = scan(&lib, dir_source(&fixtures()), "", 4, |_| {})
            .await
            .unwrap();
        assert_eq!(report.progress.added, 0);
        assert_eq!(report.progress.updated, 0);
        assert_eq!(report.progress.removed, 0);
        assert_eq!(report.progress.total, 0);
        assert_eq!(report.prune, PruneStats::default());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn changes_remove_and_update_tracks() {
        let tmp = TempDir::new();
        let lib = Library::open_in_memory(&tmp.path().join("covers")).unwrap();
        let music_dir = TempDir::new();
        let music = music_dir.path().join("music");
        copy_dir(&fixtures(), &music);

        let first = scan(&lib, dir_source(&music), "", 1, |_| {}).await.unwrap();
        assert_eq!(first.progress.added, 4);

        let untagged_bytes = fixtures().join("Artist B/Album Two/03 - Untagged Track.flac");
        let original = fixtures().join("Artist A/Album One/01 - First Song.flac");
        assert_ne!(
            fs::metadata(&untagged_bytes).unwrap().len(),
            fs::metadata(&original).unwrap().len(),
            "replacement must change the size"
        );
        fs::remove_file(music.join("Artist B/Album Two/01 - Wave Song.wav")).unwrap();
        fs::copy(
            &untagged_bytes,
            music.join("Artist A/Album One/01 - First Song.flac"),
        )
        .unwrap();

        let second = scan(&lib, dir_source(&music), "", 1, |_| {}).await.unwrap();
        assert_eq!(second.progress.removed, 1);
        assert_eq!(second.progress.updated, 1);
        assert_eq!(second.progress.added, 0);
        assert_eq!(second.progress.failed, 0);

        assert!(
            lib.track_by_key("Artist B/Album Two/01 - Wave Song.wav")
                .unwrap()
                .is_none()
        );
        let updated = lib
            .track_by_key("Artist A/Album One/01 - First Song.flac")
            .unwrap()
            .expect("updated track");
        assert_eq!(updated.title, "First Song");
        assert_eq!(updated.album, "Album One");
        assert_eq!(updated.album_artist, "Artist A");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_finds_scanned_track() {
        let tmp = TempDir::new();
        let lib = Library::open_in_memory(&tmp.path().join("covers")).unwrap();
        scan(&lib, dir_source(&fixtures()), "", 4, |_| {})
            .await
            .unwrap();

        let results = lib.search("wave", 10).unwrap();
        assert!(
            results
                .tracks
                .iter()
                .any(|t| t.key == "Artist B/Album Two/01 - Wave Song.wav"),
            "{results:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_failure_is_recorded_and_scan_continues() {
        let tmp = TempDir::new();
        let lib = Library::open_in_memory(&tmp.path().join("covers")).unwrap();
        let bad = "Artist A/Album One/02 - Second Song.m4a";
        let source = FaultySource {
            inner: DirSource::new(fixtures()),
            bad_key: bad.to_owned(),
            panic: false,
        };

        let report = scan(&lib, Arc::new(source), "", 4, |_| {}).await.unwrap();
        assert_eq!(report.progress.failed, 1);
        assert_eq!(report.progress.added, 3);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].key, bad);
        assert!(report.failures[0].error.contains(bad));
        assert_eq!(lib.stats().unwrap().tracks, 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn panicking_read_is_recorded_as_failure() {
        let tmp = TempDir::new();
        let lib = Library::open_in_memory(&tmp.path().join("covers")).unwrap();
        let bad = "Artist B/Album Two/01 - Wave Song.wav";
        let source = FaultySource {
            inner: DirSource::new(fixtures()),
            bad_key: bad.to_owned(),
            panic: true,
        };

        let report = scan(&lib, Arc::new(source), "", 2, |_| {}).await.unwrap();
        assert_eq!(report.progress.failed, 1);
        assert_eq!(report.progress.added, 3);
        assert_eq!(report.failures[0].key, bad);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prefix_limits_the_listing() {
        let tmp = TempDir::new();
        let lib = Library::open_in_memory(&tmp.path().join("covers")).unwrap();

        let report = scan(&lib, dir_source(&fixtures()), "Artist A/", 4, |_| {})
            .await
            .unwrap();
        assert_eq!(report.progress.added, 2);
        assert_eq!(report.progress.total, 2);
        assert_eq!(lib.stats().unwrap().tracks, 2);

        // A scan of another prefix must not remove the Artist A tracks.
        let report = scan(&lib, dir_source(&fixtures()), "Artist B/", 4, |_| {})
            .await
            .unwrap();
        assert_eq!(report.progress.added, 2);
        assert_eq!(report.progress.removed, 0);
        assert_eq!(lib.stats().unwrap().tracks, 4);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dir_source_lists_sorted_relative_keys() {
        let stamps = DirSource::new(fixtures()).list("Artist B/").await.unwrap();
        let keys: Vec<&str> = stamps.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "Artist B/Album Two/01 - Wave Song.wav",
                "Artist B/Album Two/03 - Untagged Track.flac",
                "Artist B/Album Two/cover.jpg",
            ]
        );
        assert!(stamps.iter().all(|s| s.etag.is_none() && s.size > 0));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replaygain_tags_round_trip_through_scan_and_query() {
        use crate::tags::write_test_tags;
        use lofty::tag::ItemKey;

        let tmp = TempDir::new();
        let lib = Library::open_in_memory(&tmp.path().join("covers")).unwrap();
        let music_dir = TempDir::new();
        let music = music_dir.path().join("music");
        copy_dir(&fixtures(), &music);
        let key = "Artist A/Album One/01 - First Song.flac";
        write_test_tags(
            &music.join(key),
            &[
                (ItemKey::ReplayGainTrackGain, "-6.54 dB"),
                (ItemKey::ReplayGainTrackPeak, "0.988525"),
                (ItemKey::ReplayGainAlbumGain, "+2.10 dB"),
                (ItemKey::ReplayGainAlbumPeak, "1.000000"),
            ],
        );

        let first = scan(&lib, dir_source(&music), "", 2, |_| {}).await.unwrap();
        assert_eq!(first.progress.added, 4);
        let t = lib.track_by_key(key).unwrap().expect("track");
        assert_eq!(t.track_gain_db, Some(-6.54));
        assert_eq!(t.track_peak, Some(0.988_525));
        assert_eq!(t.album_gain_db, Some(2.1));
        assert_eq!(t.album_peak, Some(1.0));
        let untagged = lib
            .track_by_key("Artist B/Album Two/03 - Untagged Track.flac")
            .unwrap()
            .expect("untagged track");
        assert_eq!(untagged.track_gain_db, None);
        assert_eq!(untagged.album_peak, None);

        // A changed file is re-read and the new values replace the old ones.
        write_test_tags(
            &music.join(key),
            &[
                (ItemKey::TrackTitle, "First Song (remastered edition)"),
                (ItemKey::ReplayGainTrackGain, "-7.00 dB"),
            ],
        );
        let second = scan(&lib, dir_source(&music), "", 2, |_| {}).await.unwrap();
        assert_eq!(second.progress.updated, 1);
        let t = lib.track_by_key(key).unwrap().expect("track");
        assert_eq!(t.track_gain_db, Some(-7.0));
        assert_eq!(t.track_peak, Some(0.988_525), "untouched values are kept");
    }

    #[test]
    fn extension_and_mime_helpers() {
        assert_eq!(extension("a/B/x.FLAC").as_deref(), Some("flac"));
        assert_eq!(extension("a.b/noext"), None);
        assert_eq!(extension("trailing."), None);
        assert_eq!(mime_for_cover("a/cover.PNG"), "image/png");
        assert_eq!(mime_for_cover("a/cover.jpg"), "image/jpeg");
    }
}
