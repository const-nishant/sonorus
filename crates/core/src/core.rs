//! The exported `SonorusCore` object: one instance owns the library, the bucket or local backend,
//! the tokio runtime, the player and the event pump.
//!
//! Every exported method is blocking. Call them from a background thread (`Dispatchers.IO` on
//! Android), never from the main thread.
//!
//! Locking rules:
//! - Never drop or join a [`PlayerSlot`] while holding the `player` mutex. The event pump can be
//!   inside a listener that calls back into the core, so joining it under the lock can deadlock.
//!   Take the slot out of the mutex and drop it afterwards (see [`SonorusCore::stop_player`]).
//! - The `backend` lock is never held across a blocking call.

use std::fs;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(target_os = "android")]
use sonorus_engine::aaudio::{AAudioHandle, AAudioSink};
use sonorus_engine::aaudio::{DefaultPlanner, RoutePlanner};
use sonorus_engine::opener::{
    CachingOpener, FileOpener, ReplayGain, StorageOpener, TrackOpener, TrackRef,
};
#[cfg(not(target_os = "android"))]
use sonorus_engine::output::NullSink;
use sonorus_engine::output::OutputSink;
use sonorus_engine::player::{Player, PlayerConfig, SleepTimer};
use sonorus_engine::queue::RepeatMode as EngineRepeatMode;
use sonorus_engine::source::SourceConfig;
use sonorus_library::sync::sync_user_data as run_user_data_sync;
use sonorus_library::uploads;
use sonorus_library::{
    BucketSource, BucketStore, DirSource, Library, MetaSource, PinKind, ScanProgress, Track, pins,
    scan,
};
use sonorus_storage::{ObjectMeta, Storage};
use tokio::runtime::Runtime;

use crate::error::{CoreError, CoreResult};
use crate::events::{EventListener, ListenerSlot, PlayerStatus, spawn_pump};
use crate::offline::{CACHE_DIR, Offline, cache_id_for};
use crate::router::{OutputRouter, RouterPlanner};
#[cfg(target_os = "android")]
use crate::types::output_info_from;
use crate::types::{
    AlbumInfo, AlbumSort, ArtistInfo, CacheSettings, CacheUsageInfo, CatalogueFolderInfo,
    Converter, CoreConfig, CoreEvent, DownloadStatus, LibraryStatsInfo, ListeningStatsInfo,
    LyricsInfo, OutputInfo, OutputSettings, PlaylistEntryInfo, PlaylistInfo, QualityFilterInfo,
    RepeatMode, ReplayGainSettingsInfo, S3Settings, ScanProgressInfo, ScanSummary, SearchResult,
    SignalPathInfo, SleepTimerInfo, SleepTimerRequest, SmartPlaylistKind, StatsRangeInfo,
    SyncSummary, TrackInfo, UploadJobInfo,
};
use crate::upload::{
    CancelRoute, RunEnv, UploadClaim, UploadControl, cancel_job, prepare_job, request_cancel,
    run_job, take_fd,
};

/// Concurrent tag reads during a scan.
const SCAN_CONCURRENCY: usize = 4;
/// Minimum time between two `ScanProgress` events (the final one is never throttled).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// How long dropping the core waits for runtime tasks to finish.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);
/// ReplayGain preamp limit: the preamp is clamped to `-PREAMP_LIMIT_DB..=PREAMP_LIMIT_DB`.
const PREAMP_LIMIT_DB: f32 = 15.0;
/// Longest timed sleep, in minutes (24 h).
const MAX_SLEEP_MINUTES: u32 = 24 * 60;

/// Where the music comes from.
#[derive(Clone)]
enum Backend {
    /// S3-compatible bucket; `prefix` is normalized (empty, or ending with '/'). `cache_id` names
    /// the bucket's offline cache directory.
    Bucket {
        storage: Storage,
        prefix: String,
        cache_id: String,
    },
    /// A local folder, used for development and tests. Keys are paths relative to `root`.
    Local { root: PathBuf },
}

impl Backend {
    fn kind(&self) -> &'static str {
        match self {
            Backend::Bucket { .. } => "bucket",
            Backend::Local { .. } => "local",
        }
    }
}

/// The player and the thread that forwards its events.
struct PlayerSlot {
    /// Dropped before `_pump` (fields drop in declaration order), which closes the event channel.
    player: Player,
    /// Joins the event thread when the slot is dropped; never read directly.
    _pump: PumpHandle,
    /// Reads the AAudio stream state for `output_info`. Dropped after `player` and `_pump`.
    #[cfg(target_os = "android")]
    aaudio: AAudioHandle,
}

/// Joins the event thread when dropped. Declared after `player` in [`PlayerSlot`].
struct PumpHandle(Option<JoinHandle<()>>);

impl Drop for PumpHandle {
    fn drop(&mut self) {
        let Some(handle) = self.0.take() else {
            return;
        };
        // The pump runs the listener, which may itself drop the core. It must not join itself.
        if handle.thread().id() != thread::current().id() {
            let _ = handle.join();
        }
    }
}

/// Clears the scan flag when the scan ends, on every path.
struct ScanGuard<'a>(&'a AtomicBool);

impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The music library, backend and playback engine behind the Android app.
#[derive(uniffi::Object)]
pub struct SonorusCore {
    library: Arc<Library>,
    backend: RwLock<Option<Backend>>,
    player: Mutex<Option<PlayerSlot>>,
    listener: ListenerSlot,
    scanning: AtomicBool,
    /// Output settings applied to the next player.
    output: Mutex<OutputSettings>,
    /// ReplayGain settings applied to the next player and kept across player rebuilds. The preamp
    /// is stored clamped.
    replay_gain: Mutex<ReplayGainSettingsInfo>,
    /// Gain of the current track and the sleep timer, shared with the event pump.
    status: Arc<PlayerStatus>,
    /// The route planner (the [`RouterPlanner`] adapter) set by `set_output_router`. `None` means
    /// the default planner. Lock order: never held while the `player` mutex is taken.
    router: Mutex<Option<Arc<dyn RoutePlanner>>>,
    /// Option only so that `Drop` can shut it down. Always `Some` while the core is alive.
    rt: Option<Runtime>,
    /// Which upload jobs have a run or cancel in progress, and which were asked to stop.
    upload_control: Mutex<UploadControl>,
    /// The offline cache, its pins and the download worker.
    offline: Offline,
}

#[uniffi::export]
impl SonorusCore {
    /// Opens the core: the catalogue in `data_dir`, and the backend from `config` (the local
    /// folder when set, otherwise the bucket when configured, otherwise none). Blocking.
    #[uniffi::constructor]
    pub fn new(config: CoreConfig) -> CoreResult<Arc<Self>> {
        let data_dir = PathBuf::from(&config.data_dir);
        fs::create_dir_all(&data_dir)?;
        let library = Library::open(&data_dir.join("sonorus.db"), &data_dir.join("covers"))?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("sonorus-rt")
            .build()?;
        let backend = backend_from_config(&config)?;
        log::info!(
            "sonorus core opened (backend: {})",
            backend.as_ref().map_or("none", Backend::kind)
        );
        let library = Arc::new(library);
        let listener = ListenerSlot::default();
        let offline = Offline::new(
            rt.handle(),
            data_dir.join(CACHE_DIR),
            Arc::clone(&library),
            listener.clone(),
            CacheSettings::default(),
        );
        offline.set_bucket(bucket_identity(backend.as_ref()));
        let core = Arc::new(Self {
            library,
            backend: RwLock::new(backend),
            player: Mutex::new(None),
            listener,
            scanning: AtomicBool::new(false),
            output: Mutex::new(OutputSettings::default()),
            replay_gain: Mutex::new(ReplayGainSettingsInfo::default()),
            status: Arc::new(PlayerStatus::default()),
            router: Mutex::new(None),
            rt: Some(rt),
            upload_control: Mutex::new(UploadControl::default()),
            offline,
        });
        core.refresh_pins();
        Ok(core)
    }

    /// Registers the listener for core events, replacing any previous one. Non-blocking.
    pub fn set_listener(&self, listener: Arc<dyn EventListener>) {
        self.listener.set(listener);
    }

    /// Removes the registered event listener.
    pub fn clear_listener(&self) {
        self.listener.clear();
    }

    /// Stops playback and releases the player and its event thread. Idempotent. The core stays
    /// usable: the next play call creates a new player.
    pub fn shutdown(&self) {
        self.stop_player();
    }

    /// Whether a bucket or local backend is set.
    pub fn is_configured(&self) -> bool {
        self.backend_snapshot().is_some()
    }

    /// Validates and stores bucket settings. Invalid settings change nothing. Playback restarts
    /// on the new bucket, so the current queue is dropped. Does not touch the network.
    pub fn configure_s3(&self, settings: S3Settings) -> CoreResult<()> {
        let config = settings.to_storage_config();
        let storage = Storage::new(&config)?;
        let cache_id = cache_id_for(&config);
        self.set_backend(Some(Backend::Bucket {
            storage,
            prefix: config.prefix,
            cache_id,
        }));
        Ok(())
    }

    /// Checks the configured backend: for a bucket, one ListObjectsV2 call of at most one key
    /// under the prefix (fails with `AccessDenied` on bad credentials); for a local folder, that
    /// it still exists. Returns whether anything exists there. Blocking.
    pub fn check_backend(&self) -> CoreResult<bool> {
        match self.backend_snapshot() {
            None => Err(CoreError::NotConfigured),
            Some(Backend::Local { root }) => Ok(std::fs::read_dir(&root)?.next().is_some()),
            Some(Backend::Bucket {
                storage, prefix, ..
            }) => Ok(self.runtime()?.block_on(storage.probe(&prefix))?),
        }
    }

    /// Uses a local folder as the backend. The folder must exist. The current queue is dropped.
    pub fn configure_local(&self, dir: String) -> CoreResult<()> {
        let root = PathBuf::from(&dir);
        if !root.is_dir() {
            return Err(CoreError::InvalidConfig {
                detail: format!("not a directory: {dir}"),
            });
        }
        self.set_backend(Some(Backend::Local { root }));
        Ok(())
    }

    /// Catalogue totals. Blocking.
    pub fn stats(&self) -> CoreResult<LibraryStatsInfo> {
        Ok(self.library.stats()?.into())
    }

    /// One page of albums in `sort` order. Blocking.
    pub fn albums(&self, sort: AlbumSort, limit: u32, offset: u32) -> CoreResult<Vec<AlbumInfo>> {
        let albums = self.library.albums(sort.into(), limit, offset)?;
        Ok(Converter::new(&self.library)?.albums(albums))
    }

    /// One album by id. Blocking.
    pub fn album(&self, id: i64) -> CoreResult<AlbumInfo> {
        let album = self.library.album(id)?;
        Ok(Converter::new(&self.library)?.album(album))
    }

    /// The tracks of an album in album order. Blocking.
    pub fn album_tracks(&self, album_id: i64) -> CoreResult<Vec<TrackInfo>> {
        let tracks = self.library.album_tracks(album_id)?;
        Ok(Converter::new(&self.library)?.tracks(tracks))
    }

    /// One page of artists, ordered by name. Blocking.
    pub fn artists(&self, limit: u32, offset: u32) -> CoreResult<Vec<ArtistInfo>> {
        Ok(self
            .library
            .artists(limit, offset)?
            .into_iter()
            .map(ArtistInfo::from)
            .collect())
    }

    /// One artist by id. Blocking.
    pub fn artist(&self, id: i64) -> CoreResult<ArtistInfo> {
        Ok(self.library.artist(id)?.into())
    }

    /// The albums of an artist. Blocking.
    pub fn artist_albums(&self, artist_id: i64) -> CoreResult<Vec<AlbumInfo>> {
        let albums = self.library.artist_albums(artist_id)?;
        Ok(Converter::new(&self.library)?.albums(albums))
    }

    /// One track by id. Blocking.
    pub fn track(&self, id: i64) -> CoreResult<TrackInfo> {
        let track = self.library.track(id)?;
        Ok(Converter::new(&self.library)?.track(track))
    }

    /// Searches tracks, albums and artists, at most `limit` of each. Blocking.
    pub fn search(&self, query: String, limit: u32) -> CoreResult<SearchResult> {
        let results = self.library.search(&query, limit)?;
        Ok(Converter::new(&self.library)?.search(results))
    }

    /// Searches like `search`, keeping only tracks of one audio quality. Albums and artists are
    /// kept only when one of their matching tracks passes the filter. With an empty query and a
    /// filter other than `Any`, it lists the tracks of that quality, with no albums or artists.
    /// Blocking.
    pub fn search_filtered(
        &self,
        query: String,
        limit: u32,
        filter: QualityFilterInfo,
    ) -> CoreResult<SearchResult> {
        let results = self.library.search_filtered(&query, limit, filter.into())?;
        Ok(Converter::new(&self.library)?.search(results))
    }

    /// Up to `limit` tracks of a built-in smart playlist. Blocking.
    pub fn smart_playlist(
        &self,
        kind: SmartPlaylistKind,
        limit: u32,
    ) -> CoreResult<Vec<TrackInfo>> {
        let tracks = self.library.smart_playlist(kind.into(), limit)?;
        Ok(Converter::new(&self.library)?.tracks(tracks))
    }

    /// Listening totals over `range` (ending now), with the `top` most played tracks, albums and
    /// artists. Blocking.
    pub fn listening_stats(
        &self,
        range: StatsRangeInfo,
        top: u32,
    ) -> CoreResult<ListeningStatsInfo> {
        let stats = self.library.listening_stats(range.into(), top)?;
        Ok(Converter::new(&self.library)?.listening_stats(stats))
    }

    /// The folder view of the catalogue at `path`, relative to the bucket prefix (or the local
    /// folder). `""` is the root and `"Artist A/Album One/"` a folder; the trailing `/` is
    /// optional. Lists the child folders and the tracks directly in the folder. Fails with
    /// `NotConfigured` without a backend, and with `Library` for an absolute path or one with
    /// `..`. Blocking.
    pub fn catalogue_folder(&self, path: String) -> CoreResult<CatalogueFolderInfo> {
        let prefix = self.catalogue_prefix()?;
        let folder = self.library.catalogue_folder(&prefix, &path)?;
        Ok(Converter::new(&self.library)?.catalogue_folder(folder))
    }

    /// The lyrics of a track: a `.lrc` sidecar or the embedded tag, synced when the text has
    /// timestamps. `None` if the track is missing or has no lyrics. Blocking.
    pub fn track_lyrics(&self, track_id: i64) -> CoreResult<Option<LyricsInfo>> {
        Ok(self.library.track_lyrics(track_id)?.map(LyricsInfo::from))
    }

    /// Scans the configured backend into the catalogue. Progress and the result arrive as
    /// `ScanProgress`, `ScanFinished` or `ScanFailed` events. Blocking; takes minutes on a large
    /// bucket. Fails with `Busy` when another scan is running.
    pub fn scan_library(&self) -> CoreResult<ScanSummary> {
        let (source, prefix) = self.scan_source()?;
        // Everything that can fail runs before the flag is set, so no early return can leave it
        // stuck (the `ScanGuard` below covers the rest).
        let rt = self.runtime()?;
        if self
            .scanning
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(CoreError::Busy {
                detail: "a scan is already running".to_owned(),
            });
        }
        let listener = self.listener.clone();
        let mut last_emit: Option<Instant> = None;
        let report = {
            let _scanning = ScanGuard(&self.scanning);
            rt.block_on(scan(
                &self.library,
                source,
                &prefix,
                SCAN_CONCURRENCY,
                move |progress: &ScanProgress| {
                    let finished = progress.done == progress.total;
                    let throttled = last_emit.is_some_and(|t| t.elapsed() < PROGRESS_INTERVAL);
                    if throttled && !finished {
                        return;
                    }
                    last_emit = Some(Instant::now());
                    listener.emit(CoreEvent::ScanProgress {
                        progress: ScanProgressInfo::from(progress.clone()),
                    });
                },
            ))
        };
        self.refresh_pins();
        match report {
            Ok(report) => {
                let summary = ScanSummary::from(report);
                self.listener.emit(CoreEvent::ScanFinished {
                    summary: summary.clone(),
                });
                Ok(summary)
            }
            Err(e) => {
                let err = CoreError::from(e);
                self.listener.emit(CoreEvent::ScanFailed {
                    message: err.to_string(),
                });
                Err(err)
            }
        }
    }

    /// All playlists, ordered by name. Blocking.
    pub fn playlists(&self) -> CoreResult<Vec<PlaylistInfo>> {
        let conv = Converter::new(&self.library)?;
        Ok(self
            .library
            .playlists()?
            .into_iter()
            .map(|p| conv.playlist(p))
            .collect())
    }

    /// Creates an empty playlist. Blocking.
    pub fn create_playlist(&self, name: String) -> CoreResult<PlaylistInfo> {
        let playlist = self.library.create_playlist(&name)?;
        Ok(Converter::new(&self.library)?.playlist(playlist))
    }

    /// Renames a playlist. Blocking.
    pub fn rename_playlist(&self, id: i64, name: String) -> CoreResult<()> {
        Ok(self.library.rename_playlist(id, &name)?)
    }

    /// Deletes a playlist. Blocking.
    pub fn delete_playlist(&self, id: i64) -> CoreResult<()> {
        self.library.delete_playlist(id)?;
        self.refresh_pins();
        Ok(())
    }

    /// The slots of a playlist in order. Blocking.
    pub fn playlist_entries(&self, id: i64) -> CoreResult<Vec<PlaylistEntryInfo>> {
        let conv = Converter::new(&self.library)?;
        Ok(self
            .library
            .playlist_entries(id)?
            .into_iter()
            .map(|e| conv.entry(e))
            .collect())
    }

    /// Appends tracks to a playlist, in the given order. Unknown track ids are skipped; if none
    /// is known, nothing is added and `NotFound` is returned. Blocking.
    pub fn add_to_playlist(&self, id: i64, track_ids: Vec<i64>) -> CoreResult<()> {
        let keys: Vec<String> = self
            .library
            .tracks_by_ids(&track_ids)?
            .into_iter()
            .map(|t| t.key)
            .collect();
        if keys.is_empty() {
            return Err(no_tracks());
        }
        self.library.add_to_playlist(id, &keys)?;
        self.refresh_pins();
        Ok(())
    }

    /// Removes the slot at `position`. Blocking.
    pub fn remove_from_playlist(&self, id: i64, position: u32) -> CoreResult<()> {
        self.library.remove_from_playlist(id, position)?;
        self.refresh_pins();
        Ok(())
    }

    /// Moves the slot at `from` to `to`. Blocking.
    pub fn move_in_playlist(&self, id: i64, from: u32, to: u32) -> CoreResult<()> {
        self.library.move_in_playlist(id, from, to)?;
        self.refresh_pins();
        Ok(())
    }

    /// Marks or unmarks a track as favorite. `NotFound` for an unknown track id. Blocking.
    pub fn set_favorite(&self, track_id: i64, favorite: bool) -> CoreResult<()> {
        let track = self.library.track(track_id)?;
        Ok(self.library.set_favorite(&track.key, favorite)?)
    }

    /// The favorite tracks. Blocking.
    pub fn favorites(&self) -> CoreResult<Vec<TrackInfo>> {
        let tracks = self.library.favorites()?;
        Ok(Converter::new(&self.library)?.tracks(tracks))
    }

    /// The most recent plays, newest first, one entry per track. Blocking.
    pub fn recently_played(&self, limit: u32) -> CoreResult<Vec<TrackInfo>> {
        let tracks = self.library.recently_played(limit)?;
        Ok(Converter::new(&self.library)?.tracks(tracks))
    }

    /// Albums of the most recent plays, newest first. Blocking.
    pub fn recent_albums(&self, limit: u32) -> CoreResult<Vec<AlbumInfo>> {
        let albums = self.library.recent_albums(limit)?;
        Ok(Converter::new(&self.library)?.albums(albums))
    }

    /// Deletes the listening history. Blocking.
    pub fn clear_history(&self) -> CoreResult<()> {
        Ok(self.library.clear_history()?)
    }

    /// Merges playlists and favorites with the bucket's copies of them. Bucket backend only.
    /// Blocking.
    pub fn sync_user_data(&self) -> CoreResult<SyncSummary> {
        let Some(Backend::Bucket {
            storage, prefix, ..
        }) = self.backend_snapshot()
        else {
            return Err(CoreError::NotConfigured);
        };
        let store = BucketStore::new(storage, &prefix);
        let report = self
            .runtime()?
            .block_on(run_user_data_sync(&self.library, &store))?;
        self.refresh_pins();
        Ok(report.into())
    }

    /// Adds a local audio file to the upload queue and returns its job. Blocking.
    ///
    /// `fd` is an open file descriptor for the file's content. The core takes ownership of it and
    /// closes it when this call returns, on every path, so the caller must not close it again.
    /// The core reads the tags, hashes the whole file and chooses the object key (the bucket
    /// prefix, the artist, the album, then the track name). Nothing is uploaded yet: run the job
    /// with `upload_run`. A file whose content is already in the bucket or the queue becomes a
    /// `Duplicate` job. Fails with `NotConfigured` without a bucket, and with `InvalidConfig` for
    /// a name that is not a supported audio file, an empty file, or a file that is not audio.
    pub fn upload_prepare(
        &self,
        fd: i32,
        source_uri: String,
        display_name: String,
    ) -> CoreResult<UploadJobInfo> {
        let mut file = take_fd(fd)?;
        let (storage, prefix) = self.upload_bucket()?;
        let rt = self.runtime()?;
        let mut head = |key: &str| -> CoreResult<Option<ObjectMeta>> {
            Ok(rt.block_on(storage.head_object_meta(key))?)
        };
        prepare_job(
            &self.library,
            &prefix,
            &mut file,
            &source_uri,
            &display_name,
            &mut head,
        )
    }

    /// Uploads a queued or failed job, or resumes it from the parts already stored. Blocking; can
    /// take minutes. Returns the job as it stands afterwards: `Done`, `Duplicate` and `Cancelled`
    /// jobs return at once. A storage or read error leaves the job `Failed` (not an error here),
    /// so calling this again resumes it. Progress arrives as `UploadProgress` events and the end
    /// as `UploadFinished`.
    ///
    /// `fd` is an open descriptor for the same file content as at `upload_prepare`; ownership
    /// passes to the core, as there. Fails with `Busy` while another run or cancel holds the
    /// job, `NotFound` for an unknown job, `NotConfigured` without a bucket. A source whose size
    /// changed since preparation fails the job with "source changed".
    pub fn upload_run(&self, job_id: i64, fd: i32) -> CoreResult<UploadJobInfo> {
        let file: File = take_fd(fd)?;
        let _claim =
            UploadClaim::acquire(&self.upload_control, job_id).ok_or_else(|| CoreError::Busy {
                detail: format!("upload job {job_id} is already running"),
            })?;
        let (storage, _prefix) = self.upload_bucket()?;
        let env = RunEnv {
            library: &self.library,
            rt: self.runtime()?,
            storage: &storage,
            listener: &self.listener,
            control: &self.upload_control,
        };
        run_job(&env, job_id, &file).map(UploadJobInfo::from)
    }

    /// Cancels an upload job. A running job stops at its next part boundary: its partial upload
    /// is aborted and the job becomes `Cancelled` (this call returns before that). A job that is
    /// not running is cancelled now, and its partial upload is aborted when a bucket is set (best
    /// effort). Final jobs are left unchanged. Blocking for the abort. Fails with `NotFound` for
    /// an unknown job.
    pub fn upload_cancel(&self, job_id: i64) -> CoreResult<()> {
        match request_cancel(&self.upload_control, job_id) {
            CancelRoute::Requested => Ok(()),
            CancelRoute::Claimed(_claim) => {
                // Without a bucket the partial upload cannot be aborted here; `cancel_job`
                // logs that and still marks the job cancelled.
                let storage = match self.backend_snapshot() {
                    Some(Backend::Bucket { storage, .. }) => Some(storage),
                    _ => None,
                };
                cancel_job(
                    &self.library,
                    &self.listener,
                    self.runtime()?,
                    storage.as_ref(),
                    job_id,
                )
            }
        }
    }

    /// All upload jobs, newest first. Blocking.
    pub fn uploads(&self) -> CoreResult<Vec<UploadJobInfo>> {
        Ok(uploads::list(&self.library)?
            .into_iter()
            .map(UploadJobInfo::from)
            .collect())
    }

    /// Deletes the upload jobs that are done, duplicate or cancelled, and returns how many were
    /// deleted. Queued, uploading and failed jobs are kept (a failed job is retried with
    /// `upload_run` or removed with `upload_cancel`, which aborts its partial upload). Blocking.
    pub fn clear_finished_uploads(&self) -> CoreResult<u32> {
        Ok(uploads::delete_finished(&self.library)?)
    }

    /// Replaces the queue with `track_ids` and starts at `start_index` (clamped to the last
    /// index). Unknown ids are skipped. Blocking.
    pub fn play_tracks(&self, track_ids: Vec<i64>, start_index: u32) -> CoreResult<()> {
        self.require_backend()?;
        let refs = self.track_refs(&track_ids)?;
        self.start_queue(refs, index(start_index))
    }

    /// Replaces the queue with the tracks of an album and starts at `start_index`. Blocking.
    pub fn play_album(&self, album_id: i64, start_index: u32) -> CoreResult<()> {
        self.require_backend()?;
        let refs: Vec<TrackRef> = self
            .library
            .album_tracks(album_id)?
            .iter()
            .map(track_ref)
            .collect();
        self.start_queue(refs, index(start_index))
    }

    /// Replaces the queue with the catalogued entries of a playlist. `start_index` is a playlist
    /// position: playback starts at the first catalogued entry at or after it (clamped to the
    /// last one). Blocking.
    pub fn play_playlist(&self, playlist_id: i64, start_index: u32) -> CoreResult<()> {
        self.require_backend()?;
        let mut refs = Vec::new();
        let mut start = None;
        for entry in self.library.playlist_entries(playlist_id)? {
            let Some(track) = entry.track else {
                continue;
            };
            if start.is_none() && entry.position >= start_index {
                start = Some(refs.len());
            }
            refs.push(track_ref(&track));
        }
        self.start_queue(refs, start.unwrap_or(usize::MAX))
    }

    /// Adds tracks to the end of the queue. Blocking.
    pub fn append(&self, track_ids: Vec<i64>) -> CoreResult<()> {
        self.enqueue(&track_ids, |player, refs| player.append(refs))
    }

    /// Inserts tracks right after the current queue entry. Blocking.
    pub fn play_next(&self, track_ids: Vec<i64>) -> CoreResult<()> {
        self.enqueue(&track_ids, |player, refs| player.play_next(refs))
    }

    /// Starts or resumes playback. No effect before the first play call. Non-blocking.
    pub fn play(&self) {
        self.with_player(|p| p.play());
    }

    /// Pauses playback, keeping the position. No effect before the first play call.
    pub fn pause(&self) {
        self.with_player(|p| p.pause());
    }

    /// Stops playback and clears the current track. No effect before the first play call.
    pub fn stop(&self) {
        self.with_player(|p| p.stop());
    }

    /// Seeks the current track to `position_ms`.
    pub fn seek(&self, position_ms: u64) {
        self.with_player(|p| p.seek(Duration::from_millis(position_ms)));
    }

    /// Skips to the next queue entry.
    pub fn next(&self) {
        self.with_player(|p| p.next());
    }

    /// Goes to the previous entry, or restarts the current track when it is more than 3 s in.
    pub fn previous(&self) {
        self.with_player(|p| p.previous());
    }

    /// Starts the queue entry `entry_id`.
    pub fn jump_to(&self, entry_id: u64) {
        self.with_player(|p| p.jump_to(entry_id));
    }

    /// Removes the queue entry `entry_id`.
    pub fn remove_from_queue(&self, entry_id: u64) {
        self.with_player(|p| p.remove(entry_id));
    }

    /// Moves the queue entry `entry_id` to position `to`.
    pub fn move_in_queue(&self, entry_id: u64, to: u32) {
        self.with_player(|p| p.move_entry(entry_id, index(to)));
    }

    /// Turns shuffle on or off.
    pub fn set_shuffle(&self, on: bool) {
        self.with_player(|p| p.set_shuffle(on));
    }

    /// Sets the repeat mode.
    pub fn set_repeat(&self, mode: RepeatMode) {
        let mode: EngineRepeatMode = mode.into();
        self.with_player(|p| p.set_repeat(mode));
    }

    /// Sets the output volume, `0.0` to `1.0`.
    pub fn set_volume(&self, volume: f32) {
        self.with_player(|p| p.set_volume(volume));
    }

    /// Sets the output format and exclusive mode. Stops playback and clears the queue when the
    /// settings change; the next play uses them.
    pub fn set_output_settings(&self, settings: OutputSettings) {
        let changed = {
            let mut current = self.output.lock().unwrap_or_else(PoisonError::into_inner);
            let changed = *current != settings;
            *current = settings;
            changed
        };
        log::info!("output settings: {settings:?}");
        if changed {
            self.stop_player();
        }
    }

    /// The output settings the next player is built with.
    pub fn output_settings(&self) -> OutputSettings {
        *self.output.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the ReplayGain mode and preamp. The preamp is clamped to -15 to 15 dB and the clamped
    /// value is stored. The settings apply from the next track, and are kept when the player is
    /// rebuilt. Gain applies only on the float output path; bit-perfect output is never scaled.
    /// Non-blocking.
    pub fn set_replay_gain(&self, settings: ReplayGainSettingsInfo) {
        let settings = ReplayGainSettingsInfo {
            preamp_db: clamp_preamp(settings.preamp_db),
            ..settings
        };
        log::info!("replay gain: {settings:?}");
        // Held across the store and the send, so a player built meanwhile sees one of the two
        // values, never a mix.
        let guard = self.lock_player();
        *self
            .replay_gain
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = settings;
        if let Some(slot) = guard.as_ref() {
            slot.player.set_replay_gain(settings.into());
        }
    }

    /// The ReplayGain settings in effect, with the preamp as clamped. Non-blocking.
    pub fn replay_gain_settings(&self) -> ReplayGainSettingsInfo {
        *self
            .replay_gain
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the sleep timer, replacing any other. `Minutes` pauses playback after that many
    /// minutes (1 to 1440); `EndOfTrack` pauses when the current track ends; `Off` clears it. The
    /// timer is kept when the player is rebuilt, and is set on the next player if none runs now.
    /// Fails with `InvalidConfig` for a minute count of 0 or above 1440. Non-blocking.
    pub fn set_sleep_timer(&self, request: SleepTimerRequest) -> CoreResult<()> {
        let timer = sleep_timer_for(request, Instant::now())?;
        log::info!("sleep timer: {request:?}");
        let guard = self.lock_player();
        self.status.request_sleep_timer(timer);
        if let Some(slot) = guard.as_ref() {
            slot.player.set_sleep_timer(timer);
        }
        Ok(())
    }

    /// The sleep timer: the time left on a timed sleep and whether an end-of-track sleep is set.
    /// The countdown is computed here; `CoreEvent::SleepTimerChanged` reports when a timer is set,
    /// cleared or fires. Non-blocking.
    pub fn sleep_timer(&self) -> SleepTimerInfo {
        self.status.sleep_timer_info(Instant::now())
    }

    /// The output stream as the device actually opened it. `None` when there is no player, no
    /// open stream, or when not running on Android.
    pub fn output_info(&self) -> Option<OutputInfo> {
        self.aaudio_output_info()
    }

    /// Sets the router that chooses the output route for each stream the engine opens (USB DAC,
    /// bit-perfect format). It applies to the next open, including one on the current player.
    /// Non-blocking, but a router call may block briefly on the audio side.
    pub fn set_output_router(&self, router: Arc<dyn OutputRouter>) {
        let planner: Arc<dyn RoutePlanner> = Arc::new(RouterPlanner::new(router));
        *self.router.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&planner));
        self.apply_planner(planner);
    }

    /// Removes the output router. The next open uses the default route. Non-blocking.
    pub fn clear_output_router(&self) {
        *self.router.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.apply_planner(Arc::new(DefaultPlanner));
    }

    /// What the engine last opened or tried to open: mode, plan, stream and error. `None` before
    /// the first open, when there is no player, or when not running on Android.
    pub fn signal_path(&self) -> Option<SignalPathInfo> {
        let mut path = self.aaudio_signal_path()?;
        path.replay_gain_db = self.status.replay_gain_db();
        Some(path)
    }

    /// Sets the offline cache settings and applies them now: the byte limit evicts down to it
    /// and the expiry applies at once. The app stores the settings and sets them again at startup.
    /// Blocking: it may scan the cache directory.
    pub fn set_cache_settings(&self, settings: CacheSettings) {
        log::info!("cache settings: {settings:?}");
        self.offline.apply_settings(settings);
    }

    /// The offline cache settings in effect. Non-blocking.
    pub fn cache_settings(&self) -> CacheSettings {
        self.offline.settings()
    }

    /// The offline cache: bytes used and bytes kept by pins, the settings, and the number of
    /// complete and partly downloaded tracks. Zero usage without a bucket. Blocking.
    pub fn cache_usage(&self) -> CacheUsageInfo {
        let settings = self.offline.settings();
        let usage = self
            .offline
            .cache()
            .map(|cache| cache.usage())
            .unwrap_or_default();
        CacheUsageInfo {
            used_bytes: usage.total_bytes,
            pinned_bytes: usage.pinned_bytes,
            limit_bytes: settings.limit_bytes,
            ttl_days: settings.ttl_days.filter(|days| *days > 0),
            complete_tracks: usage.complete_files,
            partial_tracks: usage.partial_files,
        }
    }

    /// Deletes the cached tracks that are not pinned and returns the bytes freed. Pinned tracks
    /// are kept. Blocking.
    pub fn clear_cache(&self) -> CoreResult<u64> {
        match self.offline.cache() {
            Some(cache) => Ok(cache.clear_unpinned()?),
            None => Ok(0),
        }
    }

    /// Reports the connection: `connected` when the device is online, `metered` when that
    /// connection is mobile data. Until the first call the core assumes a metered connection, so
    /// nothing downloads. Pinned tracks download on an unmetered connection, or on a metered one
    /// when `download_on_metered` is set. Non-blocking.
    pub fn set_network_state(&self, connected: bool, metered: bool) {
        self.offline.set_network(connected, metered);
    }

    /// Pins or unpins an album. A pinned album's tracks stay in the offline cache and download
    /// while the network allows. Unpinning aborts the track in progress. NotFound for an unknown
    /// album. Blocking.
    pub fn set_album_pinned(&self, album_id: i64, pinned: bool) -> CoreResult<()> {
        if pinned {
            pins::pin(&self.library, PinKind::Album, album_id)?;
        } else {
            pins::unpin(&self.library, PinKind::Album, album_id)?;
            self.offline.cancel_downloads();
        }
        self.refresh_pins();
        Ok(())
    }

    /// Pins or unpins a playlist, like `set_album_pinned`. NotFound for an unknown or deleted
    /// playlist when pinning. Blocking.
    pub fn set_playlist_pinned(&self, playlist_id: i64, pinned: bool) -> CoreResult<()> {
        if pinned {
            pins::pin(&self.library, PinKind::Playlist, playlist_id)?;
        } else {
            pins::unpin(&self.library, PinKind::Playlist, playlist_id)?;
            self.offline.cancel_downloads();
        }
        self.refresh_pins();
        Ok(())
    }

    /// Whether the album is pinned. Blocking.
    pub fn is_album_pinned(&self, album_id: i64) -> CoreResult<bool> {
        Ok(pins::is_pinned(&self.library, PinKind::Album, album_id)?)
    }

    /// Whether the playlist is pinned. Blocking.
    pub fn is_playlist_pinned(&self, playlist_id: i64) -> CoreResult<bool> {
        Ok(pins::is_pinned(
            &self.library,
            PinKind::Playlist,
            playlist_id,
        )?)
    }

    /// How much of an album is on this device: its pin, its track and byte totals, and the
    /// tracks completely in the offline cache. NotFound for an unknown album. Blocking.
    pub fn download_status_album(&self, album_id: i64) -> CoreResult<DownloadStatus> {
        self.library.album(album_id)?;
        self.download_status(PinKind::Album, album_id)
    }

    /// How much of a playlist is on this device, like `download_status_album`. NotFound for an
    /// unknown or deleted playlist. Blocking.
    pub fn download_status_playlist(&self, playlist_id: i64) -> CoreResult<DownloadStatus> {
        self.library.playlist(playlist_id)?;
        self.download_status(PinKind::Playlist, playlist_id)
    }
}

/// The offline cache identity and client of a bucket backend, `None` for other backends.
fn bucket_identity(backend: Option<&Backend>) -> Option<(String, Storage)> {
    match backend {
        Some(Backend::Bucket {
            storage, cache_id, ..
        }) => Some((cache_id.clone(), storage.clone())),
        _ => None,
    }
}

impl SonorusCore {
    /// The backend, cloned so that no lock is held while it is used.
    fn backend_snapshot(&self) -> Option<Backend> {
        self.backend
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The bucket handle and key prefix uploads write to. `NotConfigured` for any other backend.
    fn upload_bucket(&self) -> CoreResult<(Storage, String)> {
        match self.backend_snapshot() {
            Some(Backend::Bucket {
                storage, prefix, ..
            }) => Ok((storage, prefix)),
            _ => Err(CoreError::NotConfigured),
        }
    }

    /// The key prefix that catalogue track keys start with: the bucket prefix, or empty for a
    /// local folder (the same prefix a scan lists). `NotConfigured` with no backend.
    fn catalogue_prefix(&self) -> CoreResult<String> {
        match self.backend_snapshot() {
            Some(Backend::Bucket { prefix, .. }) => Ok(prefix),
            Some(Backend::Local { .. }) => Ok(String::new()),
            None => Err(CoreError::NotConfigured),
        }
    }

    /// Fails with `NotConfigured` when no backend is set.
    fn require_backend(&self) -> CoreResult<()> {
        match self.backend_snapshot() {
            Some(_) => Ok(()),
            None => Err(CoreError::NotConfigured),
        }
    }

    /// Replaces the backend, then drops the player so the next play uses the new one. The
    /// offline cache follows the bucket: a different bucket (or none) deletes the old bucket's
    /// cache directory; the same bucket keeps it.
    fn set_backend(&self, backend: Option<Backend>) {
        let bucket = bucket_identity(backend.as_ref());
        *self.backend.write().unwrap_or_else(PoisonError::into_inner) = backend;
        self.offline.set_bucket(bucket);
        self.refresh_pins();
        self.stop_player();
    }

    /// Re-reads the pins after a change to the library or to the pins. Failures are logged, not
    /// returned: the change that caused them has already been made.
    fn refresh_pins(&self) {
        if let Err(e) = self.offline.refresh_pins() {
            log::warn!("could not refresh the offline pins: {e}");
        }
    }

    /// How much of an album or playlist is in the offline cache. `pinned` and the totals come
    /// from the library; the cached counts from the cache, zero without a bucket.
    fn download_status(&self, kind: PinKind, ref_id: i64) -> CoreResult<DownloadStatus> {
        let pinned = pins::is_pinned(&self.library, kind, ref_id)?;
        let tracks = pins::tracks_of(&self.library, kind, ref_id)?;
        let cache = self.offline.cache();
        let mut status = DownloadStatus {
            pinned,
            total_tracks: u32::try_from(tracks.len()).unwrap_or(u32::MAX),
            cached_tracks: 0,
            total_bytes: 0,
            cached_bytes: 0,
        };
        for (_, key, size) in &tracks {
            status.total_bytes = status.total_bytes.saturating_add(*size);
            if cache.as_ref().is_some_and(|c| c.is_complete(key, *size)) {
                status.cached_tracks = status.cached_tracks.saturating_add(1);
                status.cached_bytes = status.cached_bytes.saturating_add(*size);
            }
        }
        Ok(status)
    }

    /// The AAudio stream state of the current player (Android only).
    #[cfg(target_os = "android")]
    fn aaudio_output_info(&self) -> Option<OutputInfo> {
        let guard = self.lock_player();
        let slot = guard.as_ref()?;
        let info = slot.aaudio.stream_info()?;
        Some(output_info_from(info, slot.aaudio.xruns().unwrap_or(0)))
    }

    /// Other platforms have no AAudio stream.
    #[cfg(not(target_os = "android"))]
    fn aaudio_output_info(&self) -> Option<OutputInfo> {
        None
    }

    /// The outcome of the current player's last open attempt (Android only).
    #[cfg(target_os = "android")]
    fn aaudio_signal_path(&self) -> Option<SignalPathInfo> {
        let guard = self.lock_player();
        let slot = guard.as_ref()?;
        slot.aaudio.signal_path().map(SignalPathInfo::from)
    }

    /// Other platforms have no AAudio stream.
    #[cfg(not(target_os = "android"))]
    fn aaudio_signal_path(&self) -> Option<SignalPathInfo> {
        None
    }

    /// Installs `planner` on the current player's AAudio sink, so its next open uses it (Android
    /// only; other platforms have no sink to configure).
    #[cfg(target_os = "android")]
    fn apply_planner(&self, planner: Arc<dyn RoutePlanner>) {
        if let Some(slot) = self.lock_player().as_ref() {
            slot.aaudio.set_planner(planner);
        }
    }

    /// Other platforms have no AAudio sink.
    #[cfg(not(target_os = "android"))]
    fn apply_planner(&self, _planner: Arc<dyn RoutePlanner>) {}

    /// The planner new sinks are built with: the stored router, else the default planner.
    fn current_planner(&self) -> Arc<dyn RoutePlanner> {
        self.router
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| Arc::new(DefaultPlanner))
    }

    fn runtime(&self) -> CoreResult<&Runtime> {
        self.rt.as_ref().ok_or_else(|| CoreError::Internal {
            detail: "the core is shut down".to_owned(),
        })
    }

    fn lock_player(&self) -> MutexGuard<'_, Option<PlayerSlot>> {
        self.player.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the player out of its slot and drops it outside the lock. Idempotent.
    fn stop_player(&self) {
        let old = self.lock_player().take();
        drop(old);
    }

    /// Runs `f` on the player if there is one. Does nothing otherwise, so transport calls never
    /// create a player.
    fn with_player(&self, f: impl FnOnce(&Player)) {
        let guard = self.lock_player();
        if let Some(slot) = guard.as_ref() {
            f(&slot.player);
        }
    }

    /// Runs `f` on the player, creating the player first. Fails with `NotConfigured` when no
    /// backend is set.
    fn with_new_player(&self, f: impl FnOnce(&Player)) -> CoreResult<()> {
        let mut guard = self.lock_player();
        if guard.is_none() {
            *guard = Some(self.build_player()?);
        }
        if let Some(slot) = guard.as_ref() {
            f(&slot.player);
        }
        Ok(())
    }

    fn build_player(&self) -> CoreResult<PlayerSlot> {
        let backend = self.backend_snapshot().ok_or(CoreError::NotConfigured)?;
        let opener: Arc<dyn TrackOpener> = match backend {
            Backend::Bucket { storage, .. } => {
                let handle = self.runtime()?.handle().clone();
                match self.offline.cache() {
                    Some(cache) => Arc::new(CachingOpener::new(
                        cache,
                        storage,
                        handle,
                        SourceConfig::default(),
                    )),
                    None => Arc::new(StorageOpener::new(storage, handle, SourceConfig::default())),
                }
            }
            Backend::Local { root } => Arc::new(FileOpener::new(root)),
        };
        let settings = self.output_settings();
        let config = PlayerConfig {
            output_kind: settings.sample_format.into(),
            ..PlayerConfig::default()
        };
        #[cfg_attr(not(target_os = "android"), allow(unused_variables))]
        let (sink, handle) = make_sink(settings, self.current_planner())?;
        let (player, events) = Player::new(sink, opener, config);
        let pump = spawn_pump(
            events,
            Arc::clone(&self.library),
            self.listener.clone(),
            Arc::clone(&self.status),
        )
        .map_err(|e| CoreError::Internal {
            detail: format!("failed to start the event thread: {e}"),
        })?;
        // The gain of the player being replaced is no longer the current one. Its event thread
        // was joined when it was dropped, so nothing can set the gain again after this.
        self.status.set_replay_gain_db(None);
        // Settings stored while no player existed, or before this rebuild, are applied now. The
        // event thread is already running, so the sleep timer's confirmation event is seen.
        let replay_gain = *self
            .replay_gain
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        player.set_replay_gain(replay_gain.into());
        let timer = self.status.sleep_timer_for_new_player(Instant::now());
        if timer != SleepTimer::Off {
            player.set_sleep_timer(timer);
        }
        Ok(PlayerSlot {
            player,
            _pump: PumpHandle(Some(pump)),
            #[cfg(target_os = "android")]
            aaudio: handle,
        })
    }

    /// Source and listing prefix of a scan.
    fn scan_source(&self) -> CoreResult<(Arc<dyn MetaSource>, String)> {
        let handle = self.runtime()?.handle().clone();
        match self.backend_snapshot() {
            Some(Backend::Bucket {
                storage, prefix, ..
            }) => {
                let source: Arc<dyn MetaSource> = Arc::new(BucketSource::new(storage, handle));
                Ok((source, prefix))
            }
            Some(Backend::Local { root }) => {
                let source: Arc<dyn MetaSource> = Arc::new(DirSource::new(root));
                Ok((source, String::new()))
            }
            None => Err(CoreError::NotConfigured),
        }
    }

    /// Resolves track ids to queue entries, in the given order. Unknown ids are skipped.
    fn track_refs(&self, track_ids: &[i64]) -> CoreResult<Vec<TrackRef>> {
        Ok(self
            .library
            .tracks_by_ids(track_ids)?
            .iter()
            .map(track_ref)
            .collect())
    }

    /// Replaces the queue. `start` is clamped to the last entry.
    fn start_queue(&self, refs: Vec<TrackRef>, start: usize) -> CoreResult<()> {
        if refs.is_empty() {
            return Err(no_tracks());
        }
        let start = start.min(refs.len() - 1);
        self.with_new_player(|p| p.set_queue(refs, start, true))
    }

    /// Adds resolved tracks with `add`. Fails with `NotConfigured` first, then `NotFound` when
    /// no id resolves.
    fn enqueue(
        &self,
        track_ids: &[i64],
        add: impl FnOnce(&Player, Vec<TrackRef>),
    ) -> CoreResult<()> {
        self.require_backend()?;
        let refs = self.track_refs(track_ids)?;
        if refs.is_empty() {
            return Err(no_tracks());
        }
        self.with_new_player(|p| add(p, refs))
    }
}

impl Drop for SonorusCore {
    fn drop(&mut self) {
        // The player first, outside the lock: its event thread may still be running a listener.
        self.stop_player();
        // Stops the download worker and the purge. The runtime shutdown below ends their tasks.
        self.offline.stop();
        if let Some(rt) = self.rt.take() {
            rt.shutdown_timeout(RUNTIME_SHUTDOWN);
        }
    }
}

/// Builds the backend from the config: the local folder wins, then the bucket, then none.
fn backend_from_config(config: &CoreConfig) -> CoreResult<Option<Backend>> {
    if let Some(root) = config.local_music_dir.as_deref().filter(|d| !d.is_empty()) {
        return Ok(Some(Backend::Local {
            root: PathBuf::from(root),
        }));
    }
    match &config.s3 {
        Some(settings) => {
            let storage_config = settings.to_storage_config();
            let storage = Storage::new(&storage_config)?;
            let cache_id = cache_id_for(&storage_config);
            Ok(Some(Backend::Bucket {
                storage,
                prefix: storage_config.prefix,
                cache_id,
            }))
        }
        None => Ok(None),
    }
}

/// The handle type that reads the output state: the AAudio handle on Android, nothing elsewhere.
#[cfg(target_os = "android")]
type SinkHandle = AAudioHandle;
#[cfg(not(target_os = "android"))]
type SinkHandle = ();

/// Output sink for the player, opened with `settings` and `planner`, and the handle that reads its
/// state. Android opens the AAudio sink; other platforms use a `NullSink`, which ignores the
/// planner.
fn make_sink(
    settings: OutputSettings,
    planner: Arc<dyn RoutePlanner>,
) -> CoreResult<(Box<dyn OutputSink>, SinkHandle)> {
    #[cfg(target_os = "android")]
    {
        let sink = AAudioSink::with_planner(settings.aaudio_config(), planner).map_err(|e| {
            CoreError::Internal {
                detail: format!("audio output: {e}"),
            }
        })?;
        let handle = sink.handle();
        Ok((Box::new(sink), handle))
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (settings, planner);
        Ok((Box::new(NullSink::new()), ()))
    }
}

/// The queue entry for a library track.
fn track_ref(track: &Track) -> TrackRef {
    TrackRef {
        id: track.id.to_string(),
        key: track.key.clone(),
        size: track.size,
        gain: replay_gain_of(track),
    }
}

/// The ReplayGain tags of a library track: `Some` when any of its four values is known.
fn replay_gain_of(track: &Track) -> Option<ReplayGain> {
    let any = track.track_gain_db.is_some()
        || track.track_peak.is_some()
        || track.album_gain_db.is_some()
        || track.album_peak.is_some();
    any.then_some(ReplayGain {
        track_gain_db: track.track_gain_db,
        track_peak: track.track_peak,
        album_gain_db: track.album_gain_db,
        album_peak: track.album_peak,
    })
}

/// The preamp in dB, clamped to `-PREAMP_LIMIT_DB..=PREAMP_LIMIT_DB`. A NaN preamp is 0 dB.
fn clamp_preamp(db: f32) -> f32 {
    if db.is_nan() {
        0.0
    } else {
        db.clamp(-PREAMP_LIMIT_DB, PREAMP_LIMIT_DB)
    }
}

/// The engine timer for a request, with a timed sleep ending `minutes` after `now`. Rejects 0
/// minutes and more than 24 h.
fn sleep_timer_for(request: SleepTimerRequest, now: Instant) -> CoreResult<SleepTimer> {
    Ok(match request {
        SleepTimerRequest::Off => SleepTimer::Off,
        SleepTimerRequest::EndOfTrack => SleepTimer::EndOfTrack,
        SleepTimerRequest::Minutes { minutes } => {
            if minutes == 0 {
                return Err(CoreError::InvalidConfig {
                    detail: "a sleep timer needs at least 1 minute".to_owned(),
                });
            }
            if minutes > MAX_SLEEP_MINUTES {
                return Err(CoreError::InvalidConfig {
                    detail: format!("a sleep timer can last at most {MAX_SLEEP_MINUTES} minutes"),
                });
            }
            SleepTimer::At(now + Duration::from_secs(u64::from(minutes) * 60))
        }
    })
}

/// A list index from a UI value. Values beyond `usize` (never on 64-bit) saturate.
fn index(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn no_tracks() -> CoreError {
    CoreError::NotFound {
        detail: "no catalogued tracks".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ReplayGainModeInfo;

    #[test]
    fn index_saturates_on_overflow_only() {
        assert_eq!(index(0), 0);
        assert_eq!(index(u32::MAX), u32::MAX as usize);
    }

    fn track_with_gain(
        track_gain_db: Option<f32>,
        track_peak: Option<f32>,
        album_gain_db: Option<f32>,
        album_peak: Option<f32>,
    ) -> Track {
        Track {
            id: 7,
            key: "music/a.flac".to_owned(),
            size: 1_000,
            etag: None,
            title: "t".to_owned(),
            artist: "a".to_owned(),
            album_artist: "a".to_owned(),
            album: "al".to_owned(),
            album_id: 1,
            artist_id: 1,
            track_no: None,
            disc_no: None,
            year: None,
            genre: None,
            duration_ms: None,
            sample_rate: None,
            bits_per_sample: None,
            channels: None,
            codec: None,
            cover_id: None,
            has_lyrics: false,
            added_at: 0,
            track_gain_db,
            track_peak,
            album_gain_db,
            album_peak,
        }
    }

    #[test]
    fn track_ref_carries_gain_when_any_tag_is_known() {
        let plain = track_ref(&track_with_gain(None, None, None, None));
        assert_eq!(plain.gain, None);
        assert_eq!((plain.id.as_str(), plain.size), ("7", 1_000));

        let track = track_ref(&track_with_gain(Some(-6.5), Some(0.9), None, None));
        assert_eq!(
            track.gain,
            Some(ReplayGain {
                track_gain_db: Some(-6.5),
                track_peak: Some(0.9),
                album_gain_db: None,
                album_peak: None,
            })
        );

        let album_only = track_ref(&track_with_gain(None, None, Some(-3.0), None));
        assert_eq!(
            album_only.gain,
            Some(ReplayGain {
                track_gain_db: None,
                track_peak: None,
                album_gain_db: Some(-3.0),
                album_peak: None,
            })
        );
    }

    #[test]
    fn preamp_is_clamped_to_fifteen_db() {
        assert_eq!(clamp_preamp(20.0), 15.0);
        assert_eq!(clamp_preamp(-40.0), -15.0);
        assert_eq!(clamp_preamp(-3.5), -3.5);
        assert_eq!(clamp_preamp(15.0), 15.0);
        assert_eq!(clamp_preamp(f32::NAN), 0.0);
        assert_eq!(clamp_preamp(f32::INFINITY), 15.0);
    }

    #[test]
    fn sleep_timer_request_validation() {
        let now = Instant::now();
        assert_eq!(
            sleep_timer_for(SleepTimerRequest::Off, now).expect("off"),
            SleepTimer::Off
        );
        assert_eq!(
            sleep_timer_for(SleepTimerRequest::EndOfTrack, now).expect("end"),
            SleepTimer::EndOfTrack
        );
        assert_eq!(
            sleep_timer_for(SleepTimerRequest::Minutes { minutes: 1 }, now).expect("1"),
            SleepTimer::At(now + Duration::from_secs(60))
        );
        assert_eq!(
            sleep_timer_for(SleepTimerRequest::Minutes { minutes: 1440 }, now).expect("max"),
            SleepTimer::At(now + Duration::from_secs(24 * 3600))
        );
        assert!(matches!(
            sleep_timer_for(SleepTimerRequest::Minutes { minutes: 0 }, now),
            Err(CoreError::InvalidConfig { .. })
        ));
        assert!(matches!(
            sleep_timer_for(SleepTimerRequest::Minutes { minutes: 1441 }, now),
            Err(CoreError::InvalidConfig { .. })
        ));
        assert!(matches!(
            sleep_timer_for(SleepTimerRequest::Minutes { minutes: u32::MAX }, now),
            Err(CoreError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn set_sleep_timer_rejects_invalid_and_keeps_previous() {
        let dir = std::env::temp_dir().join(format!("sonorus-core-sleep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let core = SonorusCore::new(CoreConfig {
            data_dir: dir.to_string_lossy().into_owned(),
            s3: None,
            local_music_dir: None,
        })
        .expect("open core");
        core.set_sleep_timer(SleepTimerRequest::Minutes { minutes: 5 })
            .expect("5 minutes");
        assert!(matches!(
            core.set_sleep_timer(SleepTimerRequest::Minutes { minutes: 0 }),
            Err(CoreError::InvalidConfig { .. })
        ));
        assert!(matches!(
            core.set_sleep_timer(SleepTimerRequest::Minutes { minutes: 1441 }),
            Err(CoreError::InvalidConfig { .. })
        ));
        let info = core.sleep_timer();
        let left = info.remaining_ms.expect("timer still set");
        assert!(left > 299_000 && left <= 300_000, "{left}");
        drop(core);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_gain_settings_are_clamped_and_stored() {
        let dir = std::env::temp_dir().join(format!("sonorus-core-rg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let core = SonorusCore::new(CoreConfig {
            data_dir: dir.to_string_lossy().into_owned(),
            s3: None,
            local_music_dir: None,
        })
        .expect("open core");
        assert_eq!(
            core.replay_gain_settings(),
            ReplayGainSettingsInfo::default()
        );
        core.set_replay_gain(ReplayGainSettingsInfo {
            mode: ReplayGainModeInfo::Album,
            preamp_db: 40.0,
        });
        assert_eq!(
            core.replay_gain_settings(),
            ReplayGainSettingsInfo {
                mode: ReplayGainModeInfo::Album,
                preamp_db: 15.0,
            }
        );
        core.set_replay_gain(ReplayGainSettingsInfo {
            mode: ReplayGainModeInfo::Track,
            preamp_db: -99.0,
        });
        assert_eq!(core.replay_gain_settings().preamp_db, -15.0);
        drop(core);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn backend_prefers_local_then_bucket() {
        let base = CoreConfig {
            data_dir: "/unused".to_owned(),
            s3: None,
            local_music_dir: None,
        };
        assert!(backend_from_config(&base).expect("none").is_none());

        let empty_local = CoreConfig {
            local_music_dir: Some(String::new()),
            ..base.clone()
        };
        assert!(backend_from_config(&empty_local).expect("empty").is_none());

        let local = CoreConfig {
            local_music_dir: Some("/music".to_owned()),
            ..base
        };
        assert!(matches!(
            backend_from_config(&local).expect("local"),
            Some(Backend::Local { .. })
        ));
    }
}
