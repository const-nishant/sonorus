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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(target_os = "android")]
use sonorus_engine::aaudio::{AAudioHandle, AAudioSink};
use sonorus_engine::opener::{FileOpener, StorageOpener, TrackOpener, TrackRef};
#[cfg(not(target_os = "android"))]
use sonorus_engine::output::NullSink;
use sonorus_engine::output::OutputSink;
use sonorus_engine::player::{Player, PlayerConfig};
use sonorus_engine::queue::RepeatMode as EngineRepeatMode;
use sonorus_engine::source::SourceConfig;
use sonorus_library::sync::sync_user_data as run_user_data_sync;
use sonorus_library::{
    BucketSource, BucketStore, DirSource, Library, MetaSource, ScanProgress, Track, scan,
};
use sonorus_storage::Storage;
use tokio::runtime::Runtime;

use crate::error::{CoreError, CoreResult};
use crate::events::{EventListener, ListenerSlot, spawn_pump};
#[cfg(target_os = "android")]
use crate::types::output_info_from;
use crate::types::{
    AlbumInfo, AlbumSort, ArtistInfo, Converter, CoreConfig, CoreEvent, LibraryStatsInfo,
    OutputInfo, OutputSettings, PlaylistEntryInfo, PlaylistInfo, RepeatMode, S3Settings,
    ScanProgressInfo, ScanSummary, SearchResult, SyncSummary, TrackInfo,
};

/// Concurrent tag reads during a scan.
const SCAN_CONCURRENCY: usize = 4;
/// Minimum time between two `ScanProgress` events (the final one is never throttled).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// How long dropping the core waits for runtime tasks to finish.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);

/// Where the music comes from.
#[derive(Clone)]
enum Backend {
    /// S3-compatible bucket; `prefix` is normalized (empty, or ending with '/').
    Bucket { storage: Storage, prefix: String },
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
    /// Option only so that `Drop` can shut it down. Always `Some` while the core is alive.
    rt: Option<Runtime>,
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
        Ok(Arc::new(Self {
            library: Arc::new(library),
            backend: RwLock::new(backend),
            player: Mutex::new(None),
            listener: ListenerSlot::default(),
            scanning: AtomicBool::new(false),
            output: Mutex::new(OutputSettings::default()),
            rt: Some(rt),
        }))
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
        self.set_backend(Some(Backend::Bucket {
            storage,
            prefix: config.prefix,
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
            Some(Backend::Bucket { storage, prefix }) => {
                Ok(self.runtime()?.block_on(storage.probe(&prefix))?)
            }
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

    /// The embedded lyrics of a track, if it has any. Blocking.
    pub fn lyrics(&self, track_id: i64) -> CoreResult<Option<String>> {
        Ok(self.library.lyrics(track_id)?)
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
        Ok(self.library.delete_playlist(id)?)
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
        Ok(self.library.add_to_playlist(id, &keys)?)
    }

    /// Removes the slot at `position`. Blocking.
    pub fn remove_from_playlist(&self, id: i64, position: u32) -> CoreResult<()> {
        Ok(self.library.remove_from_playlist(id, position)?)
    }

    /// Moves the slot at `from` to `to`. Blocking.
    pub fn move_in_playlist(&self, id: i64, from: u32, to: u32) -> CoreResult<()> {
        Ok(self.library.move_in_playlist(id, from, to)?)
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
        let Some(Backend::Bucket { storage, prefix }) = self.backend_snapshot() else {
            return Err(CoreError::NotConfigured);
        };
        let store = BucketStore::new(storage, &prefix);
        let report = self
            .runtime()?
            .block_on(run_user_data_sync(&self.library, &store))?;
        Ok(report.into())
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

    /// The output stream as the device actually opened it. `None` when there is no player, no
    /// open stream, or when not running on Android.
    pub fn output_info(&self) -> Option<OutputInfo> {
        self.aaudio_output_info()
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

    /// Fails with `NotConfigured` when no backend is set.
    fn require_backend(&self) -> CoreResult<()> {
        match self.backend_snapshot() {
            Some(_) => Ok(()),
            None => Err(CoreError::NotConfigured),
        }
    }

    /// Replaces the backend, then drops the player so the next play uses the new one.
    fn set_backend(&self, backend: Option<Backend>) {
        *self.backend.write().unwrap_or_else(PoisonError::into_inner) = backend;
        self.stop_player();
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
            Backend::Bucket { storage, .. } => Arc::new(StorageOpener::new(
                storage,
                self.runtime()?.handle().clone(),
                SourceConfig::default(),
            )),
            Backend::Local { root } => Arc::new(FileOpener::new(root)),
        };
        let settings = self.output_settings();
        let config = PlayerConfig {
            output_kind: settings.sample_format.into(),
            ..PlayerConfig::default()
        };
        #[cfg_attr(not(target_os = "android"), allow(unused_variables))]
        let (sink, handle) = make_sink(settings)?;
        let (player, events) = Player::new(sink, opener, config);
        let pump =
            spawn_pump(events, Arc::clone(&self.library), self.listener.clone()).map_err(|e| {
                CoreError::Internal {
                    detail: format!("failed to start the event thread: {e}"),
                }
            })?;
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
            Some(Backend::Bucket { storage, prefix }) => {
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
            Ok(Some(Backend::Bucket {
                storage,
                prefix: storage_config.prefix,
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

/// Output sink for the player, opened with `settings`, and the handle that reads its state.
/// Android opens the AAudio sink; other platforms use a `NullSink`.
fn make_sink(settings: OutputSettings) -> CoreResult<(Box<dyn OutputSink>, SinkHandle)> {
    #[cfg(target_os = "android")]
    {
        let sink = AAudioSink::new(settings.aaudio_config()).map_err(|e| CoreError::Internal {
            detail: format!("audio output: {e}"),
        })?;
        let handle = sink.handle();
        Ok((Box::new(sink), handle))
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = settings;
        Ok((Box::new(NullSink::new()), ()))
    }
}

/// The queue entry for a library track.
fn track_ref(track: &Track) -> TrackRef {
    TrackRef {
        id: track.id.to_string(),
        key: track.key.clone(),
        size: track.size,
    }
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

    #[test]
    fn index_saturates_on_overflow_only() {
        assert_eq!(index(0), 0);
        assert_eq!(index(u32::MAX), u32::MAX as usize);
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
