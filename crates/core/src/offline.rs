//! The offline cache: a disk cache per bucket for streamed tracks, the pins that keep albums and
//! playlists on this device, and the background worker that downloads pinned tracks when the
//! network allows it.
//!
//! Each bucket gets its own directory, `{data_dir}/cache/{id}`, where `id` is derived from its
//! endpoint, bucket and prefix. Switching to another bucket, or leaving the bucket backend, drops
//! the cache handle and deletes the old directory. Cache keys are object keys, which are only
//! unique within one bucket, so this keeps one bucket's audio from being served for another.
//!
//! The download worker and the expiry purge run as tasks on the core runtime. Dropping the core
//! sets the stop signal and cancels the running download; the runtime's bounded shutdown then ends
//! both tasks.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use sonorus_engine::cache::{self, TrackCache};
use sonorus_engine::source::StorageObject;
use sonorus_library::{Library, pins};
use sonorus_storage::{Storage, StorageConfig};
use tokio::runtime::Handle;
use tokio::sync::{Notify, watch};
use tokio::time::{Instant, interval, sleep_until};

use crate::error::CoreResult;
use crate::events::ListenerSlot;
use crate::types::{CacheSettings, CoreEvent};

/// Name of the cache directory inside the core's data directory.
pub(crate) const CACHE_DIR: &str = "cache";
/// Hex characters of the bucket digest used as the cache directory name.
const CACHE_ID_LEN: usize = 16;
/// How often unpinned entries are checked for expiry.
const PURGE_INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// Wait before the next automatic download pass after a pass in which a track failed.
const RETRY_DELAY: Duration = Duration::from_secs(60);
const SECS_PER_DAY: u64 = 86_400;

/// Whether downloads may run: connected, and either not metered or allowed on metered networks.
pub(crate) fn downloads_allowed(connected: bool, metered: bool, download_on_metered: bool) -> bool {
    connected && (!metered || download_on_metered)
}

/// Name of the cache directory of a bucket: the first 16 hex characters of
/// `blake3(endpoint "\n" bucket "\n" prefix)`. Credentials are not part of it, so changing the
/// keys of the same bucket keeps its cache.
pub(crate) fn cache_id(endpoint: &str, bucket: &str, prefix: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(endpoint.as_bytes());
    hasher.update(b"\n");
    hasher.update(bucket.as_bytes());
    hasher.update(b"\n");
    hasher.update(prefix.as_bytes());
    let hex = hasher.finalize().to_hex();
    hex.as_str()[..CACHE_ID_LEN].to_owned()
}

/// The cache directory name for the bucket described by `config`.
pub(crate) fn cache_id_for(config: &StorageConfig) -> String {
    cache_id(&config.endpoint, &config.bucket, &config.prefix)
}

/// Expiry of unpinned entries for `ttl_days`. `None` and `Some(0)` both mean "never expire".
fn ttl_of(ttl_days: Option<u32>) -> Option<Duration> {
    ttl_days
        .filter(|days| *days > 0)
        .map(|days| Duration::from_secs(u64::from(days) * SECS_PER_DAY))
}

/// True for a name `cache_id` can produce: exactly 16 hex characters, so no path separators.
fn is_cache_id(name: &str) -> bool {
    name.len() == CACHE_ID_LEN && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Deletes the cache directory `id` under `root`. Names that are not cache ids are refused, so
/// nothing outside `root` can be touched. Best effort: failures are logged.
pub(crate) fn remove_cache_dir(root: &Path, id: &str) {
    if !is_cache_id(id) {
        log::warn!("refusing to remove a cache directory with an invalid name");
        return;
    }
    let dir = root.join(id);
    match fs::remove_dir_all(&dir) {
        Ok(()) => log::info!("removed offline cache of a bucket that is no longer used"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("could not remove offline cache {}: {e}", dir.display()),
    }
}

/// The bucket the cache belongs to.
struct Bucket {
    id: String,
    /// Client with SDK retries off: the download loop owns the retries.
    storage: Storage,
}

/// State guarded by a mutex. Never held across an await or a blocking download.
struct State {
    bucket: Option<Bucket>,
    /// The open cache of `bucket`. Dropped when the bucket changes.
    cache: Option<Arc<TrackCache>>,
    connected: bool,
    metered: bool,
    settings: CacheSettings,
}

/// Everything the worker tasks and the core share.
struct Shared {
    state: Mutex<State>,
    /// `{data_dir}/cache`. Bucket directories are direct children of it.
    root: PathBuf,
    library: Arc<Library>,
    listener: ListenerSlot,
    /// Wakes the download worker for a pass.
    wake: Notify,
    /// Runs the expiry purge now.
    purge_now: Notify,
    /// Set to abort the download in progress. The worker clears it before each track.
    cancel: AtomicBool,
}

/// The offline cache of one core. Methods are called from the core's blocking API.
pub(crate) struct Offline {
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
}

impl Offline {
    /// Starts the purge and download tasks on `rt`. The cache starts with no bucket; call
    /// [`Offline::set_bucket`] to attach one.
    pub(crate) fn new(
        rt: &Handle,
        root: PathBuf,
        library: Arc<Library>,
        listener: ListenerSlot,
        settings: CacheSettings,
    ) -> Offline {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                bucket: None,
                cache: None,
                // Until the app reports the network, assume the worst: no downloads.
                connected: true,
                metered: true,
                settings,
            }),
            root,
            library,
            listener,
            wake: Notify::new(),
            purge_now: Notify::new(),
            cancel: AtomicBool::new(false),
        });
        let (stop, stop_rx) = watch::channel(false);
        rt.spawn(purge_loop(Arc::clone(&shared), stop_rx.clone()));
        rt.spawn(download_loop(Arc::clone(&shared), stop_rx));
        Offline { shared, stop }
    }

    /// Attaches the bucket `(cache id, storage)`, or detaches it with `None`.
    ///
    /// The same id keeps the cache and only replaces the client. A different id (or `None`)
    /// drops the old cache handle, cancels its download and deletes its directory. The new
    /// bucket's cache is opened here.
    pub(crate) fn set_bucket(&self, bucket: Option<(String, Storage)>) {
        let removed = {
            let mut st = lock(&self.shared.state);
            let old_id = st.bucket.as_ref().map(|b| b.id.clone());
            let new_id = bucket.as_ref().map(|(id, _)| id.clone());
            let changed = old_id != new_id;
            if changed {
                st.cache = None;
            }
            st.bucket = bucket.map(|(id, storage)| Bucket {
                id,
                storage: storage.without_sdk_retries(),
            });
            if changed { old_id } else { None }
        };
        if let Some(old) = removed {
            self.shared.cancel.store(true, Ordering::Release);
            remove_cache_dir(&self.shared.root, &old);
        }
        self.shared.purge_now.notify_one();
        // Open now, so the bucket's directory exists and its pins are loaded.
        let _ = open_cache(&self.shared);
        self.shared.wake.notify_one();
    }

    /// The open cache of the current bucket. Opens it on first use. `None` without a bucket, or
    /// when the directory cannot be opened (logged; the next call tries again).
    pub(crate) fn cache(&self) -> Option<Arc<TrackCache>> {
        open_cache(&self.shared)
    }

    /// The current settings.
    pub(crate) fn settings(&self) -> CacheSettings {
        lock(&self.shared.state).settings
    }

    /// Stores new settings and applies them to the open cache. Turning downloads off cancels the
    /// one in progress.
    pub(crate) fn apply_settings(&self, settings: CacheSettings) {
        let (cache, allowed) = {
            let mut st = lock(&self.shared.state);
            st.settings = settings;
            (
                st.cache.clone(),
                downloads_allowed(st.connected, st.metered, settings.download_on_metered),
            )
        };
        if let Some(cache) = cache {
            cache.set_limit(settings.limit_bytes);
            cache.set_ttl(ttl_of(settings.ttl_days));
        }
        if !allowed {
            self.shared.cancel.store(true, Ordering::Release);
        }
        self.shared.wake.notify_one();
    }

    /// Records the connectivity the app reports. Downloads stop when they are no longer allowed.
    pub(crate) fn set_network(&self, connected: bool, metered: bool) {
        let allowed = {
            let mut st = lock(&self.shared.state);
            st.connected = connected;
            st.metered = metered;
            downloads_allowed(connected, metered, st.settings.download_on_metered)
        };
        if !allowed {
            self.shared.cancel.store(true, Ordering::Release);
        }
        self.shared.wake.notify_one();
    }

    /// Aborts the track being downloaded. The worker goes on with what is still pinned.
    pub(crate) fn cancel_downloads(&self) {
        self.shared.cancel.store(true, Ordering::Release);
    }

    /// Re-reads the pins after a change: removes pins to gone albums and deleted playlists,
    /// hands the pinned tracks to the cache so they are not evicted, and wakes the worker.
    pub(crate) fn refresh_pins(&self) -> CoreResult<()> {
        pins::prune_pins(&self.shared.library)?;
        if let Some(cache) = open_cache(&self.shared) {
            let tracks = pins::pinned_tracks(&self.shared.library)?;
            cache.set_pinned(tracks.into_iter().map(|(_, key, size)| (key, size)));
        }
        self.shared.wake.notify_one();
        Ok(())
    }

    /// Stops the worker and the purge, and aborts the download in progress. Called on drop. The
    /// tasks end when the runtime shuts down.
    pub(crate) fn stop(&self) {
        self.stop.send_replace(true);
        self.shared.cancel.store(true, Ordering::Release);
        self.shared.wake.notify_one();
        self.shared.purge_now.notify_one();
    }
}

/// Locks, ignoring poisoning: the state is only left consistent between steps.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The open cache, opened on first use. It starts with the pinned tracks already set.
fn open_cache(shared: &Shared) -> Option<Arc<TrackCache>> {
    let mut st = lock(&shared.state);
    if let Some(cache) = &st.cache {
        return Some(Arc::clone(cache));
    }
    let id = st.bucket.as_ref()?.id.clone();
    let dir = shared.root.join(&id);
    let limit = st.settings.limit_bytes;
    let ttl = ttl_of(st.settings.ttl_days);
    let cache = match TrackCache::open(dir.clone(), limit, ttl) {
        Ok(cache) => cache,
        Err(e) => {
            log::warn!(
                "offline cache unavailable at {}, streaming without it: {e}",
                dir.display()
            );
            return None;
        }
    };
    match pins::pinned_tracks(&shared.library) {
        Ok(tracks) => cache.set_pinned(tracks.into_iter().map(|(_, key, size)| (key, size))),
        Err(e) => log::warn!("could not load pins for the offline cache: {e}"),
    }
    st.cache = Some(Arc::clone(&cache));
    Some(cache)
}

/// The bucket client, if a bucket is attached.
fn storage_of(shared: &Shared) -> Option<Storage> {
    lock(&shared.state)
        .bucket
        .as_ref()
        .map(|b| b.storage.clone())
}

/// Whether downloads are allowed right now.
fn allowed(shared: &Shared) -> bool {
    let st = lock(&shared.state);
    downloads_allowed(st.connected, st.metered, st.settings.download_on_metered)
}

/// Expires old unpinned entries now and every [`PURGE_INTERVAL`] while the core lives. The first
/// tick is immediate, so this also runs at start.
async fn purge_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut ticker = interval(PURGE_INTERVAL);
    loop {
        if *stop.borrow() {
            return;
        }
        tokio::select! {
            _ = stop.changed() => return,
            _ = ticker.tick() => {}
            _ = shared.purge_now.notified() => {}
        }
        let Some(cache) = open_cache(&shared) else {
            continue;
        };
        match tokio::task::spawn_blocking(move || cache.purge_expired()).await {
            Ok(Ok(0)) => {}
            Ok(Ok(freed)) => log::info!("offline cache: expired {freed} bytes"),
            Ok(Err(e)) => log::warn!("offline cache expiry failed: {e}"),
            Err(e) => log::warn!("offline cache expiry task failed: {e}"),
        }
    }
}

/// Runs a download pass when woken, and again after [`RETRY_DELAY`] when the last pass had a
/// failed track.
async fn download_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut retry_at: Option<Instant> = None;
    loop {
        if *stop.borrow() {
            return;
        }
        tokio::select! {
            _ = stop.changed() => return,
            _ = shared.wake.notified() => {}
            _ = sleep_or_pending(retry_at) => {}
        }
        if *stop.borrow() {
            return;
        }
        let failed = download_pass(&shared, &stop).await;
        retry_at = failed.then(|| Instant::now() + RETRY_DELAY);
    }
}

/// Sleeps until `deadline`, or forever when there is none.
async fn sleep_or_pending(deadline: Option<Instant>) {
    match deadline {
        Some(at) => sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Downloads pinned tracks one at a time until none is left that may be downloaded now. A track
/// that fails is skipped for the rest of the pass. Re-reads the pins before each track. Ends with
/// `DownloadsIdle`. Returns whether a track failed.
async fn download_pass(shared: &Shared, stop: &watch::Receiver<bool>) -> bool {
    let mut failed_ids: HashSet<i64> = HashSet::new();
    let mut failed = false;
    loop {
        // Clear the flag before looking at the state: a change made after this point sets it
        // again and aborts the download below.
        shared.cancel.store(false, Ordering::Release);
        if *stop.borrow() || !allowed(shared) {
            break;
        }
        let (Some(cache), Some(storage)) = (open_cache(shared), storage_of(shared)) else {
            break;
        };
        let tracks = match pins::pinned_tracks(&shared.library) {
            Ok(tracks) => tracks,
            Err(e) => {
                log::warn!("download pass could not read the pins: {e}");
                failed = true;
                break;
            }
        };
        let total = tracks.len();
        let mut done = 0usize;
        let mut next = None;
        for (track_id, key, size) in &tracks {
            if cache.is_complete(key, *size) {
                done += 1;
            } else if next.is_none() && !failed_ids.contains(track_id) {
                next = Some((*track_id, key.clone(), *size));
            }
        }
        let Some((track_id, key, size)) = next else {
            break;
        };

        let object = Arc::new(StorageObject {
            storage,
            key: key.clone(),
        });
        match cache::download(&cache, object, &key, size, &shared.cancel).await {
            Ok(()) => {
                shared.listener.emit(CoreEvent::DownloadProgress {
                    track_id,
                    done_tracks: u32::try_from(done + 1).unwrap_or(u32::MAX),
                    total_tracks: u32::try_from(total).unwrap_or(u32::MAX),
                });
            }
            Err(e) => {
                if shared.cancel.load(Ordering::Acquire) {
                    // Aborted by a pin, network or stop change: the state is re-checked above.
                    continue;
                }
                log::warn!("download of {key} failed, will retry later: {e}");
                failed_ids.insert(track_id);
                failed = true;
            }
        }
    }
    let pending = pending_tracks(shared);
    shared.listener.emit(CoreEvent::DownloadsIdle {
        pending_tracks: pending,
    });
    failed
}

/// Pinned tracks not yet complete in the cache. Zero without a bucket, where nothing can download.
fn pending_tracks(shared: &Shared) -> u32 {
    let Some(cache) = open_cache(shared) else {
        return 0;
    };
    let Ok(tracks) = pins::pinned_tracks(&shared.library) else {
        return 0;
    };
    let pending = tracks
        .iter()
        .filter(|(_, key, size)| !cache.is_complete(key, *size))
        .count();
    u32::try_from(pending).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloads_follow_connection_and_metering() {
        // Not connected: never.
        assert!(!downloads_allowed(false, false, false));
        assert!(!downloads_allowed(false, false, true));
        // Unmetered: always when connected.
        assert!(downloads_allowed(true, false, false));
        assert!(downloads_allowed(true, false, true));
        // Metered: only when the user allowed it.
        assert!(!downloads_allowed(true, true, false));
        assert!(downloads_allowed(true, true, true));
    }

    #[test]
    fn cache_id_is_16_hex_and_depends_on_endpoint_bucket_and_prefix() {
        let base = cache_id("https://e.example", "music", "lib/");
        assert_eq!(base.len(), CACHE_ID_LEN);
        assert!(is_cache_id(&base));
        assert_eq!(base, cache_id("https://e.example", "music", "lib/"));
        assert_ne!(base, cache_id("https://f.example", "music", "lib/"));
        assert_ne!(base, cache_id("https://e.example", "other", "lib/"));
        assert_ne!(base, cache_id("https://e.example", "music", ""));
        // The separator keeps "ab"+"c" apart from "a"+"bc".
        assert_ne!(cache_id("x", "ab", "c"), cache_id("x", "a", "bc"));
    }

    #[test]
    fn ttl_zero_and_none_never_expire() {
        assert_eq!(ttl_of(None), None);
        assert_eq!(ttl_of(Some(0)), None);
        assert_eq!(
            ttl_of(Some(30)),
            Some(Duration::from_secs(30 * SECS_PER_DAY))
        );
    }

    #[test]
    fn removal_refuses_anything_but_a_cache_id() {
        let base =
            std::env::temp_dir().join(format!("sonorus-offline-remove-{}", std::process::id()));
        let root = base.join("cache");
        let outside = base.join("keep");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("file"), b"x").unwrap();

        remove_cache_dir(&root, "../keep");
        remove_cache_dir(&root, "keep");
        assert!(outside.join("file").exists());

        let id = cache_id("e", "b", "");
        fs::create_dir_all(root.join(&id)).unwrap();
        remove_cache_dir(&root, &id);
        assert!(!root.join(&id).exists());

        fs::remove_dir_all(&base).unwrap();
    }
}

#[cfg(test)]
mod core_tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use sonorus_library::{Library, ObjectStamp, TrackMeta};

    use super::{CACHE_DIR, cache_id_for};
    use crate::core::SonorusCore;
    use crate::error::CoreError;
    use crate::types::{CacheSettings, CoreConfig, S3Settings};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "sonorus-offline-test-{}-{n}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Bucket settings for an endpoint that refuses connections, so no test reaches a real bucket.
    fn bucket(name: &str, secret: &str) -> S3Settings {
        S3Settings {
            endpoint: "http://127.0.0.1:9".to_owned(),
            region: "us-east-1".to_owned(),
            bucket: name.to_owned(),
            access_key_id: "AKIAEXAMPLEKEY".to_owned(),
            secret_access_key: secret.to_owned(),
            path_style: true,
            prefix: String::new(),
        }
    }

    fn open_core(dir: &TempDir) -> Arc<SonorusCore> {
        SonorusCore::new(CoreConfig {
            data_dir: dir.path().display().to_string(),
            s3: None,
            local_music_dir: None,
        })
        .unwrap()
    }

    /// Catalogues two tracks of one album through a second connection to the core's database.
    /// Returns the album id.
    fn seed_album(dir: &TempDir) -> i64 {
        let lib =
            Library::open(&dir.path().join("sonorus.db"), &dir.path().join("covers")).unwrap();
        for (size, name) in [(1000u64, "01 - One.flac"), (1001, "02 - Two.flac")] {
            let stamp = ObjectStamp {
                key: format!("Artist/Album/{name}"),
                size,
                etag: None,
                last_modified: None,
            };
            let meta = TrackMeta {
                title: Some(name.to_owned()),
                artist: Some("Artist".to_owned()),
                album: Some("Album".to_owned()),
                ..Default::default()
            };
            lib.upsert_track(&stamp, &meta, None).unwrap();
        }
        lib.track_by_key("Artist/Album/01 - One.flac")
            .unwrap()
            .unwrap()
            .album_id
    }

    #[test]
    fn cache_settings_round_trip_and_reach_the_cache() {
        let dir = TempDir::new();
        let core = open_core(&dir);
        assert_eq!(core.cache_settings(), CacheSettings::default());

        let custom = CacheSettings {
            limit_bytes: 1 << 20,
            ttl_days: None,
            download_on_metered: true,
        };
        core.set_cache_settings(custom);
        assert_eq!(core.cache_settings(), custom);
        let usage = core.cache_usage();
        assert_eq!(usage.limit_bytes, 1 << 20);
        assert_eq!(usage.ttl_days, None);
        assert_eq!(usage.used_bytes, 0);

        // With a bucket, the cache is opened with the stored limit.
        core.configure_s3(bucket("music-a", "one")).unwrap();
        assert_eq!(core.cache_usage().limit_bytes, 1 << 20);
        let thirty = CacheSettings {
            ttl_days: Some(0),
            ..custom
        };
        core.set_cache_settings(thirty);
        assert_eq!(core.cache_usage().ttl_days, None, "0 days means never");
    }

    #[test]
    fn each_bucket_gets_its_own_cache_and_switching_removes_the_old_one() {
        let dir = TempDir::new();
        let core = open_core(&dir);
        let cache_root = dir.path().join(CACHE_DIR);
        let a = bucket("music-a", "one");
        let b = bucket("music-b", "one");
        let id_a = cache_id_for(&a.to_storage_config());
        let id_b = cache_id_for(&b.to_storage_config());
        assert_ne!(id_a, id_b, "different buckets, different cache directories");

        core.configure_s3(a.clone()).unwrap();
        assert!(cache_root.join(&id_a).is_dir());

        core.configure_s3(b.clone()).unwrap();
        assert!(!cache_root.join(&id_a).exists(), "old bucket cache removed");
        assert!(cache_root.join(&id_b).is_dir());

        // The same bucket with new credentials keeps its cache.
        core.configure_s3(bucket("music-b", "two")).unwrap();
        assert!(cache_root.join(&id_b).is_dir());

        // Disconnecting (leaving the bucket for a local folder) removes it.
        core.configure_local(dir.path().display().to_string())
            .unwrap();
        assert!(!cache_root.join(&id_b).exists());
    }

    #[test]
    fn pins_and_download_status_without_network() {
        let dir = TempDir::new();
        let album = seed_album(&dir);
        let core = open_core(&dir);
        core.configure_s3(bucket("music-a", "one")).unwrap();

        // Default network state is metered: nothing downloads.
        assert!(!core.is_album_pinned(album).unwrap());
        core.set_album_pinned(album, true).unwrap();
        assert!(core.is_album_pinned(album).unwrap());
        let status = core.download_status_album(album).unwrap();
        assert!(status.pinned);
        assert_eq!(status.total_tracks, 2);
        assert_eq!(status.cached_tracks, 0);
        assert_eq!(status.total_bytes, 2001);
        assert_eq!(status.cached_bytes, 0);

        core.set_album_pinned(album, false).unwrap();
        assert!(!core.is_album_pinned(album).unwrap());
        assert!(!core.download_status_album(album).unwrap().pinned);

        assert!(matches!(
            core.download_status_album(9_999),
            Err(CoreError::NotFound { .. })
        ));
        assert!(matches!(
            core.set_album_pinned(9_999, true),
            Err(CoreError::NotFound { .. })
        ));
    }

    #[test]
    fn playlist_pins_follow_the_playlist_lifetime() {
        let dir = TempDir::new();
        let album = seed_album(&dir);
        let core = open_core(&dir);
        core.configure_s3(bucket("music-a", "one")).unwrap();
        let track = core.album_tracks(album).unwrap()[0].id;
        let playlist = core.create_playlist("Mix".to_owned()).unwrap();
        core.add_to_playlist(playlist.id, vec![track]).unwrap();

        core.set_playlist_pinned(playlist.id, true).unwrap();
        assert!(core.is_playlist_pinned(playlist.id).unwrap());
        let status = core.download_status_playlist(playlist.id).unwrap();
        assert_eq!(status.total_tracks, 1);
        assert_eq!(status.total_bytes, 1000);
        assert_eq!(status.cached_tracks, 0);

        core.delete_playlist(playlist.id).unwrap();
        assert!(!core.is_playlist_pinned(playlist.id).unwrap());
        assert!(matches!(
            core.download_status_playlist(playlist.id),
            Err(CoreError::NotFound { .. })
        ));
    }

    #[test]
    fn dropping_the_core_stops_a_running_download_promptly() {
        let dir = TempDir::new();
        let album = seed_album(&dir);
        let core = open_core(&dir);
        core.configure_s3(bucket("music-a", "one")).unwrap();
        // Unmetered and connected: the worker starts and fails to reach the endpoint.
        core.set_network_state(true, false);
        core.set_album_pinned(album, true).unwrap();
        std::thread::sleep(Duration::from_millis(200));

        let started = Instant::now();
        drop(core);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "drop took {:?}",
            started.elapsed()
        );
    }
}
