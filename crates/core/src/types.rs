//! Records and enums exported to Kotlin, plus the conversions from the library, engine and
//! storage types. Exported types never use `usize`, sets, paths or `Duration`: times are
//! milliseconds (`u64`), paths are `String`s, and ids are the library's `i64` ids.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use sonorus_engine::aaudio::{
    AAudioConfig, PathMode, Performance, RoutePlan, RouteRequest, Sharing, SignalPath,
    StreamInfo as AAudioStreamInfo,
};
use sonorus_engine::decoder::StreamInfo;
use sonorus_engine::gain::{ReplayGainMode, ReplayGainSettings};
use sonorus_engine::output::SampleKind;
use sonorus_engine::player::{PlayerEvent, PlayerState};
use sonorus_engine::queue::RepeatMode as EngineRepeatMode;
use sonorus_library::uploads::{UploadRow, UploadState as LibraryUploadState};
use sonorus_library::{
    Album, AlbumSort as LibraryAlbumSort, Artist, Library, LibraryStats, LyricLine, LyricsSource,
    Playlist, PlaylistEntry, ScanProgress, ScanReport, SearchResults, SyncReport, Track,
    TrackLyrics,
};
use sonorus_storage::{StorageConfig, normalize_prefix, region_from_endpoint};

use crate::error::CoreResult;

/// Bucket connection settings, as entered by the user and stored in the Android Keystore.
///
/// `Debug` is implemented manually: the secret is printed as `"<redacted>"` and the access key
/// id is cut to its first four characters, so the value can be logged safely.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct S3Settings {
    /// Endpoint URL, `https://...` or `http://...`.
    pub endpoint: String,
    /// Signing region. Empty means "derive from the endpoint, else `auto`".
    pub region: String,
    /// Bucket name.
    pub bucket: String,
    /// Access key id.
    pub access_key_id: String,
    /// Secret access key. Never logged or shown.
    pub secret_access_key: String,
    /// Use path-style addressing (`endpoint/bucket/key`).
    pub path_style: bool,
    /// Key prefix the music lives under. Empty means the whole bucket.
    pub prefix: String,
}

impl S3Settings {
    /// Builds the storage client configuration. Strings are trimmed, the prefix is normalized
    /// (`"/music/"` becomes `"music/"`), and an empty region is derived from the endpoint or
    /// falls back to `auto`.
    pub fn to_storage_config(&self) -> StorageConfig {
        let endpoint = self.endpoint.trim().to_string();
        let region = self.region.trim();
        let region = if region.is_empty() {
            region_from_endpoint(&endpoint).unwrap_or_else(|| "auto".to_string())
        } else {
            region.to_string()
        };
        StorageConfig {
            endpoint,
            region,
            bucket: self.bucket.trim().to_string(),
            access_key_id: self.access_key_id.trim().to_string(),
            secret_access_key: self.secret_access_key.trim().to_string(),
            path_style: self.path_style,
            prefix: normalize_prefix(&self.prefix),
        }
    }
}

impl fmt::Debug for S3Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key_hint = if self.access_key_id.chars().count() >= 8 {
            let head: String = self.access_key_id.chars().take(4).collect();
            format!("{head}…")
        } else {
            "…".to_string()
        };
        f.debug_struct("S3Settings")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &key_hint)
            .field("secret_access_key", &"<redacted>")
            .field("path_style", &self.path_style)
            .field("prefix", &self.prefix)
            .finish()
    }
}

/// Everything the core needs at construction time.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CoreConfig {
    /// Directory for the catalogue database, cover files and local state. Must exist and be
    /// writable.
    pub data_dir: String,
    /// Bucket settings. `None` means no bucket backend.
    pub s3: Option<S3Settings>,
    /// Development and testing backend: scan a local folder and play files from it. When set,
    /// it takes precedence over `s3`.
    pub local_music_dir: Option<String>,
}

/// Playback state as shown in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PlaybackState {
    /// Nothing loaded.
    Idle,
    /// Opening the current track.
    Loading,
    /// Waiting for audio data.
    Buffering,
    /// Audio is playing.
    Playing,
    /// Paused with a track loaded.
    Paused,
}

/// Repeat behaviour at the end of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RepeatMode {
    /// Stop at the end of the queue.
    Off,
    /// Start over when the queue ends.
    All,
    /// Repeat the current track.
    One,
}

/// Sort order for album listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AlbumSort {
    /// By album artist, then title.
    Artist,
    /// By album title.
    Title,
    /// By release year.
    Year,
    /// Most recently added first.
    RecentlyAdded,
}

/// A catalogued track with its album and artist resolved, as shown in the UI.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TrackInfo {
    /// Library track id.
    pub id: i64,
    /// Object key in the bucket (or relative path for the local backend).
    pub key: String,
    /// Track title.
    pub title: String,
    /// Track artist.
    pub artist: String,
    /// Album artist.
    pub album_artist: String,
    /// Album title.
    pub album: String,
    /// Library album id.
    pub album_id: i64,
    /// Library artist id.
    pub artist_id: i64,
    /// Track number on its disc.
    pub track_no: Option<u32>,
    /// Disc number.
    pub disc_no: Option<u32>,
    /// Release year.
    pub year: Option<i32>,
    /// Genre tag.
    pub genre: Option<String>,
    /// Duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// Sample rate in Hz, when known.
    pub sample_rate: Option<u32>,
    /// Bits per sample, when known.
    pub bits_per_sample: Option<u8>,
    /// Channel count, when known.
    pub channels: Option<u8>,
    /// Codec name, when known.
    pub codec: Option<String>,
    /// File size in bytes.
    pub size: u64,
    /// Absolute path of the cover image file, when there is one.
    pub cover_path: Option<String>,
    /// Whether the track has lyrics, embedded or from a `.lrc` sidecar.
    pub has_lyrics: bool,
    /// Whether the track is in the favorites.
    pub is_favorite: bool,
}

/// One timed line of synced lyrics.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LyricLineInfo {
    /// Start time from the beginning of the track, in milliseconds.
    pub time_ms: u64,
    /// Line text. Empty for an instrumental gap.
    pub text: String,
}

/// Where the lyrics of a track came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LyricsSourceInfo {
    /// A `.lrc` file next to the audio file in the library.
    Sidecar,
    /// The lyrics tag of the audio file.
    Embedded,
}

/// The lyrics of a track.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LyricsInfo {
    /// Timed lines in time order, when the lyrics are synced.
    pub synced: Option<Vec<LyricLineInfo>>,
    /// Readable text: the lines joined with newlines when synced, otherwise the plain text.
    pub plain: String,
    /// Where the lyrics came from.
    pub source: LyricsSourceInfo,
}

/// An album as shown in listings.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AlbumInfo {
    /// Library album id.
    pub id: i64,
    /// Album title.
    pub title: String,
    /// Album artist name.
    pub artist: String,
    /// Library artist id.
    pub artist_id: i64,
    /// Release year.
    pub year: Option<i32>,
    /// Number of catalogued tracks.
    pub track_count: u32,
    /// Sum of track durations in milliseconds.
    pub duration_ms: u64,
    /// Absolute path of the cover image file, when there is one.
    pub cover_path: Option<String>,
    /// When the album was first catalogued (Unix seconds).
    pub added_at: i64,
}

/// An artist as shown in listings.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ArtistInfo {
    /// Library artist id.
    pub id: i64,
    /// Artist name.
    pub name: String,
    /// Number of albums.
    pub album_count: u32,
    /// Number of tracks.
    pub track_count: u32,
}

/// A user playlist.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PlaylistInfo {
    /// Library playlist id.
    pub id: i64,
    /// Playlist name.
    pub name: String,
    /// Number of entries, including ones whose track is not catalogued.
    pub track_count: u32,
    /// Sum of durations of the catalogued entries, in milliseconds.
    pub duration_ms: u64,
    /// Absolute path of the cover of the first catalogued entry, when there is one.
    pub cover_path: Option<String>,
    /// Creation time (Unix milliseconds).
    pub created_at: i64,
    /// Last change time (Unix milliseconds).
    pub updated_at: i64,
}

/// One slot of a playlist.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PlaylistEntryInfo {
    /// Zero-based position in the playlist.
    pub position: u32,
    /// Object key of the track in this slot.
    pub track_key: String,
    /// The track, or `None` when the key is not (or no longer) catalogued.
    pub track: Option<TrackInfo>,
}

/// Search results grouped by kind.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SearchResult {
    /// Matching tracks.
    pub tracks: Vec<TrackInfo>,
    /// Matching albums.
    pub albums: Vec<AlbumInfo>,
    /// Matching artists.
    pub artists: Vec<ArtistInfo>,
}

/// Library totals for the about or settings screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct LibraryStatsInfo {
    /// Number of catalogued tracks.
    pub tracks: u64,
    /// Number of albums.
    pub albums: u64,
    /// Number of artists.
    pub artists: u64,
    /// Sum of track sizes in bytes.
    pub total_bytes: u64,
    /// Sum of track durations in milliseconds.
    pub total_duration_ms: u64,
}

/// Description of the decoded stream of the track being played.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct StreamInfoRecord {
    /// Codec name, such as `FLAC` or `PCM`.
    pub codec: String,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
    /// Bits per sample, when the container states it.
    pub bits_per_sample: Option<u32>,
    /// Duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// Human-readable summary, such as `FLAC 24/96`.
    pub label: String,
}

/// One entry of the play queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct QueueItem {
    /// Queue entry id. Unique within the queue, so the same track can appear twice.
    pub entry_id: u64,
    /// Library track id.
    pub track_id: i64,
}

/// Counters of a bucket or folder scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct ScanProgressInfo {
    /// Tracks to add or update in this scan.
    pub total: u32,
    /// Tracks finished so far.
    pub done: u32,
    /// Tracks newly catalogued.
    pub added: u32,
    /// Existing tracks re-read because they changed.
    pub updated: u32,
    /// Catalogue tracks whose object disappeared.
    pub removed: u32,
    /// Tracks that could not be read or stored.
    pub failed: u32,
}

/// One object that could not be catalogued.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ScanFailureInfo {
    /// Object key of the failed track.
    pub key: String,
    /// Why it failed.
    pub error: String,
}

/// Outcome of a completed scan.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ScanSummary {
    /// Final counters.
    pub progress: ScanProgressInfo,
    /// Per-object failures, in completion order.
    pub failures: Vec<ScanFailureInfo>,
    /// Albums removed by the final prune.
    pub pruned_albums: u64,
    /// Artists removed by the final prune.
    pub pruned_artists: u64,
    /// Cover files removed by the final prune.
    pub pruned_covers: u64,
}

/// Outcome of a user-data sync (playlists and favorites).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct SyncSummary {
    /// Local playlists created or overwritten from the remote document.
    pub playlists_applied: u32,
    /// Local favorites created or overwritten from the remote document.
    pub favorites_applied: u32,
    /// Whether the playlists document was written to the bucket.
    pub playlists_pushed: bool,
    /// Whether the favorites document was written to the bucket.
    pub favorites_pushed: bool,
}

/// Where an upload job stands. `Done`, `Duplicate` and `Cancelled` are final; `Failed` jobs can be
/// resumed by running them again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum UploadState {
    /// Added, not started.
    Queued,
    /// A multipart upload is in progress.
    Uploading,
    /// The object is stored in the bucket.
    Done,
    /// The same content is already in the bucket or in the queue; nothing was uploaded.
    Duplicate,
    /// A storage or read error stopped the upload. Run the job again to resume it.
    Failed,
    /// The user cancelled; any partial upload was aborted.
    Cancelled,
}

/// One upload job, as shown in the upload queue.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct UploadJobInfo {
    /// Job id, stable across restarts.
    pub id: i64,
    /// File name as the user picked it.
    pub display_name: String,
    /// Object key in the bucket (for a duplicate, the key of the existing object).
    pub key: String,
    /// Size of the source file in bytes.
    pub size: u64,
    /// Bytes of the source stored in the bucket so far.
    pub bytes_done: u64,
    /// Current state.
    pub state: UploadState,
    /// Short description of the last failure, when the job failed.
    pub error: Option<String>,
    /// Where the source was read from (the app's content URI), so the app can re-open it to run
    /// or resume the job.
    pub source_uri: String,
}

/// Offline cache settings. They apply live, see `SonorusCore::set_cache_settings`. The app stores
/// them and sets them again at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct CacheSettings {
    /// Byte limit of the cache. Pinned tracks are kept even above it. `0` disables caching of
    /// tracks that are not pinned.
    pub limit_bytes: u64,
    /// Unpinned tracks not played or downloaded for this many days are deleted. `None`, or `0`,
    /// means they never expire.
    pub ttl_days: Option<u32>,
    /// Download pinned tracks on a metered (mobile data) connection too.
    pub download_on_metered: bool,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            limit_bytes: DEFAULT_CACHE_LIMIT_BYTES,
            ttl_days: Some(DEFAULT_CACHE_TTL_DAYS),
            download_on_metered: false,
        }
    }
}

/// Default offline cache limit: 4 GiB.
pub const DEFAULT_CACHE_LIMIT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Default expiry of unpinned cached tracks, in days.
pub const DEFAULT_CACHE_TTL_DAYS: u32 = 30;

/// What the offline cache holds right now, see `SonorusCore::cache_usage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct CacheUsageInfo {
    /// Bytes used by complete and partially downloaded tracks.
    pub used_bytes: u64,
    /// Bytes used by tracks covered by a pin.
    pub pinned_bytes: u64,
    /// The configured byte limit.
    pub limit_bytes: u64,
    /// The configured expiry of unpinned tracks in days; `None` when they never expire.
    pub ttl_days: Option<u32>,
    /// Complete tracks in the cache.
    pub complete_tracks: u32,
    /// Tracks that are partly downloaded.
    pub partial_tracks: u32,
}

/// How much of an album or playlist is on this device. Totals count the catalogued tracks it
/// covers, whether or not it is pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct DownloadStatus {
    /// Whether the album or playlist is pinned.
    pub pinned: bool,
    /// Catalogued tracks it covers.
    pub total_tracks: u32,
    /// Of those, tracks completely in the cache.
    pub cached_tracks: u32,
    /// Bytes of all the covered tracks.
    pub total_bytes: u64,
    /// Bytes of the tracks completely in the cache.
    pub cached_bytes: u64,
}

/// Which ReplayGain tag value to apply, see `SonorusCore::set_replay_gain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, uniffi::Enum)]
pub enum ReplayGainModeInfo {
    /// No gain is applied.
    #[default]
    Off,
    /// Track gain, falling back to album gain.
    Track,
    /// Album gain, falling back to track gain.
    Album,
}

/// ReplayGain settings. They apply from the next track, see `SonorusCore::set_replay_gain`.
#[derive(Debug, Clone, Copy, PartialEq, Default, uniffi::Record)]
pub struct ReplayGainSettingsInfo {
    /// Which tag values to use.
    pub mode: ReplayGainModeInfo,
    /// Offset added to the tag gain, in dB. The core clamps it to -15 to 15 dB.
    pub preamp_db: f32,
}

/// A sleep timer request, see `SonorusCore::set_sleep_timer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SleepTimerRequest {
    /// Clears the sleep timer.
    Off,
    /// Pauses playback after `minutes`, counted from the request. Must be 1 to 1440 (24 h).
    Minutes {
        /// Minutes until playback pauses.
        minutes: u32,
    },
    /// Pauses playback when the current track ends.
    EndOfTrack,
}

/// The sleep timer as the core knows it, see `SonorusCore::sleep_timer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct SleepTimerInfo {
    /// Milliseconds left on a timed sleep, `None` when no timed sleep is set.
    pub remaining_ms: Option<u64>,
    /// Whether an end-of-track sleep is set.
    pub end_of_track: bool,
}

/// Events pushed to the foreign `EventListener` by the core's event pump.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum CoreEvent {
    /// The playback state changed.
    StateChanged {
        /// The new state.
        state: PlaybackState,
    },
    /// The current queue entry changed. `track` is `None` if the track is not catalogued.
    TrackChanged {
        /// Queue entry that is now current.
        entry_id: u64,
        /// The track, when it resolves in the catalogue.
        track: Option<TrackInfo>,
    },
    /// A track's stream was opened and is ready to play.
    TrackLoaded {
        /// Library track id.
        track_id: i64,
        /// Format of the stream.
        stream: StreamInfoRecord,
    },
    /// Playback position update, emitted a few times per second.
    Position {
        /// Library track id.
        track_id: i64,
        /// Position in milliseconds.
        position_ms: u64,
        /// Track duration in milliseconds, when known.
        duration_ms: Option<u64>,
    },
    /// The track played to its end.
    TrackEnded {
        /// Library track id.
        track_id: i64,
    },
    /// The queue changed, or the current entry changed.
    QueueChanged {
        /// Queue in play order.
        items: Vec<QueueItem>,
        /// Current entry id, if any.
        current: Option<u64>,
        /// Shuffle on or off.
        shuffle: bool,
        /// Repeat mode.
        repeat: RepeatMode,
    },
    /// A transient failure stopped playback; the core is retrying.
    Recovering {
        /// Library track id.
        track_id: i64,
        /// Retry attempt number, starting at 1.
        attempt: u32,
        /// Position the retry resumes at, in milliseconds.
        position_ms: u64,
        /// Delay before the retry, in milliseconds.
        retry_in_ms: u64,
    },
    /// Playback failed.
    PlaybackError {
        /// Library track id, when the failure is tied to a track.
        track_id: Option<i64>,
        /// What went wrong.
        message: String,
    },
    /// Progress of a running scan.
    ScanProgress {
        /// Current counters.
        progress: ScanProgressInfo,
    },
    /// A scan completed.
    ScanFinished {
        /// Final summary.
        summary: ScanSummary,
    },
    /// A scan failed before completing.
    ScanFailed {
        /// What went wrong.
        message: String,
    },
    /// An upload job stored more bytes. Sent after each part, so at most once per part.
    UploadProgress {
        /// Upload job id.
        job_id: i64,
        /// Source bytes stored in the bucket so far.
        bytes_done: u64,
        /// Source file size in bytes.
        total: u64,
    },
    /// An upload run ended: the job is done, failed (resumable), or cancelled.
    UploadFinished {
        /// Upload job id.
        job_id: i64,
        /// The state the run ended in.
        state: UploadState,
    },
    /// A pinned track finished downloading into the offline cache.
    DownloadProgress {
        /// Library track id of the track that finished.
        track_id: i64,
        /// Pinned tracks complete in the cache, including this one.
        done_tracks: u32,
        /// Pinned tracks in total.
        total_tracks: u32,
    },
    /// The download worker has nothing more it may do right now. `pending_tracks` above zero means
    /// tracks still wait: for a Wi-Fi connection, or because a download failed (retried later).
    DownloadsIdle {
        /// Pinned tracks not yet complete in the cache.
        pending_tracks: u32,
    },
    /// The sleep timer was set, cleared or fired. `remaining_ms` is the time left on a timed sleep
    /// (`None` when none is set); `end_of_track` is true while an end-of-track sleep is set.
    /// Poll `SonorusCore::sleep_timer` for the countdown: this event is not sent every second.
    SleepTimerChanged {
        /// Milliseconds left on a timed sleep, `None` when no timed sleep is set.
        remaining_ms: Option<u64>,
        /// Whether an end-of-track sleep is set.
        end_of_track: bool,
    },
}

/// Sample format the output stream is opened with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, uniffi::Enum)]
pub enum OutputSampleFormat {
    /// 32-bit float, volume applied.
    #[default]
    Float,
    /// Left-justified 32-bit integers, bit-perfect.
    Int32,
    /// 24-bit packed integers, bit-perfect (for 24-bit DACs).
    Int24Packed,
    /// 16-bit integers, bit-perfect (for 16-bit DAC modes; lossless for 16-bit sources only).
    Int16,
}

/// User output settings. They apply to the next player, see `SonorusCore::set_output_settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, uniffi::Record)]
pub struct OutputSettings {
    /// Sample format the output is opened with.
    pub sample_format: OutputSampleFormat,
    /// Request exclusive device access (lowest latency, may fall back to shared).
    pub exclusive: bool,
}

/// The output stream as the device actually opened it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OutputInfo {
    /// Output backend name, `"aaudio"`.
    pub backend: String,
    /// Negotiated sample rate in Hz.
    pub sample_rate: u32,
    /// Negotiated channel count.
    pub channels: u32,
    /// Negotiated sample format.
    pub sample_format: OutputSampleFormat,
    /// Whether the stream actually runs in exclusive sharing mode.
    pub exclusive: bool,
    /// Whether the stream actually runs in low-latency performance mode.
    pub low_latency: bool,
    /// Device the stream is attached to.
    pub device_id: i32,
    /// Frames per hardware burst.
    pub frames_per_burst: u32,
    /// Buffer size in frames.
    pub buffer_frames: u32,
    /// Underruns (xruns) reported by the stream so far.
    pub xruns: i32,
}

/// How the audio reaches the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum OutputPathMode {
    /// Integer samples reach the device unchanged (bit-perfect).
    BitPerfect,
    /// Exclusive device access, but not bit-perfect.
    Exclusive,
    /// Shared Android mixer, which may resample and apply volume.
    Mixer,
}

/// The format the engine is about to open, passed to `OutputRouter::plan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct RouteRequestInfo {
    /// Sample rate of the source in Hz (the rate the stream is opened at).
    pub sample_rate: u32,
    /// Channel count of the source.
    pub channels: u32,
    /// Bit depth of the decoded source, when known.
    pub source_bits: Option<u32>,
    /// Sample format the player asks for.
    pub requested: OutputSampleFormat,
    /// Whether the user asked for exclusive device access.
    pub exclusive: bool,
}

/// How to open the stream, returned by `OutputRouter::plan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct RoutePlanInfo {
    /// Device to open, or `None` for the default device.
    pub device_id: Option<i32>,
    /// Sample format to open the stream with.
    pub sample_format: OutputSampleFormat,
    /// Whether to request exclusive sharing (the performance mode follows from it).
    pub exclusive: bool,
    /// Whether the route is meant to be bit-perfect.
    pub bit_perfect: bool,
    /// Open the stream but do not restart it automatically after a disconnect reopen; an explicit
    /// play still starts it. Set by a router while the output device has just been removed and
    /// the app is pausing.
    pub hold: bool,
}

/// What the engine opened, passed to `OutputRouter::opened` and returned by `SonorusCore::signal_path`.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SignalPathInfo {
    /// The path the audio takes (`Mixer` when the open failed).
    pub mode: OutputPathMode,
    /// The plan the open was attempted with.
    pub plan: RoutePlanInfo,
    /// The negotiated stream, `None` when the open failed.
    pub stream: Option<OutputInfo>,
    /// The open error message, `None` on success.
    pub error: Option<String>,
    /// Sample rate of the source in Hz.
    pub source_rate: u32,
    /// Bit depth of the source, when known.
    pub source_bits: Option<u32>,
    /// The ReplayGain gain in dB applied to the current track, from the last `GainApplied` event.
    /// `None` when no gain is applied (ReplayGain off, untagged, integer output, or unity).
    /// Filled in by `SonorusCore::signal_path`, not by the engine.
    pub replay_gain_db: Option<f32>,
}

impl OutputSettings {
    /// The AAudio configuration for these settings. Performance is `PowerSaving`; the engine
    /// forces low latency for exclusive streams. The device is the default one.
    pub fn aaudio_config(&self) -> AAudioConfig {
        AAudioConfig {
            sharing: if self.exclusive {
                Sharing::Exclusive
            } else {
                Sharing::Shared
            },
            performance: Performance::PowerSaving,
            device_id: None,
        }
    }
}

impl From<OutputSampleFormat> for SampleKind {
    fn from(f: OutputSampleFormat) -> Self {
        match f {
            OutputSampleFormat::Float => SampleKind::F32,
            OutputSampleFormat::Int32 => SampleKind::I32,
            OutputSampleFormat::Int24Packed => SampleKind::I24Packed,
            OutputSampleFormat::Int16 => SampleKind::I16,
        }
    }
}

impl From<SampleKind> for OutputSampleFormat {
    fn from(k: SampleKind) -> Self {
        match k {
            SampleKind::F32 => OutputSampleFormat::Float,
            SampleKind::I32 => OutputSampleFormat::Int32,
            SampleKind::I24Packed => OutputSampleFormat::Int24Packed,
            SampleKind::I16 => OutputSampleFormat::Int16,
        }
    }
}

/// The exported view of a negotiated AAudio stream. `xruns` comes from the stream, not from
/// `info`.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn output_info_from(info: AAudioStreamInfo, xruns: i32) -> OutputInfo {
    OutputInfo {
        backend: "aaudio".to_owned(),
        sample_rate: info.sample_rate,
        channels: u32::from(info.channels),
        sample_format: info.sample.into(),
        exclusive: info.sharing == Sharing::Exclusive,
        low_latency: info.performance == Performance::LowLatency,
        device_id: info.device_id,
        frames_per_burst: info.frames_per_burst,
        buffer_frames: info.buffer_frames,
        xruns,
    }
}

impl From<PathMode> for OutputPathMode {
    fn from(m: PathMode) -> Self {
        match m {
            PathMode::BitPerfect => OutputPathMode::BitPerfect,
            PathMode::Exclusive => OutputPathMode::Exclusive,
            PathMode::Mixer => OutputPathMode::Mixer,
        }
    }
}

impl From<OutputPathMode> for PathMode {
    fn from(m: OutputPathMode) -> Self {
        match m {
            OutputPathMode::BitPerfect => PathMode::BitPerfect,
            OutputPathMode::Exclusive => PathMode::Exclusive,
            OutputPathMode::Mixer => PathMode::Mixer,
        }
    }
}

impl From<RouteRequest> for RouteRequestInfo {
    /// Exclusive is taken from the user's sharing mode; the other fields map one to one.
    fn from(r: RouteRequest) -> Self {
        RouteRequestInfo {
            sample_rate: r.sample_rate,
            channels: u32::from(r.channels),
            source_bits: r.source_bits,
            requested: r.requested.into(),
            exclusive: r.config.sharing == Sharing::Exclusive,
        }
    }
}

impl From<RoutePlanInfo> for RoutePlan {
    /// Exclusive selects `Sharing::Exclusive` with `Performance::LowLatency` (the MMAP path needs
    /// it); otherwise the stream is shared and power saving.
    fn from(p: RoutePlanInfo) -> Self {
        RoutePlan {
            device_id: p.device_id,
            sample: p.sample_format.into(),
            sharing: if p.exclusive {
                Sharing::Exclusive
            } else {
                Sharing::Shared
            },
            performance: if p.exclusive {
                Performance::LowLatency
            } else {
                Performance::PowerSaving
            },
            bit_perfect: p.bit_perfect,
            hold: p.hold,
        }
    }
}

impl From<RoutePlan> for RoutePlanInfo {
    /// Only the sharing mode is exported; the performance mode is implied by it.
    fn from(p: RoutePlan) -> Self {
        RoutePlanInfo {
            device_id: p.device_id,
            sample_format: p.sample.into(),
            exclusive: p.sharing == Sharing::Exclusive,
            bit_perfect: p.bit_perfect,
            hold: p.hold,
        }
    }
}

impl From<SignalPath> for SignalPathInfo {
    /// The stream's xruns are not part of the path, so the exported stream has `xruns` 0.
    fn from(p: SignalPath) -> Self {
        SignalPathInfo {
            mode: p.mode.into(),
            plan: p.plan.into(),
            stream: p.stream.map(|s| output_info_from(s, 0)),
            error: p.error,
            source_rate: p.source_rate,
            source_bits: p.source_bits,
            replay_gain_db: None,
        }
    }
}

impl From<ReplayGainModeInfo> for ReplayGainMode {
    fn from(m: ReplayGainModeInfo) -> Self {
        match m {
            ReplayGainModeInfo::Off => ReplayGainMode::Off,
            ReplayGainModeInfo::Track => ReplayGainMode::Track,
            ReplayGainModeInfo::Album => ReplayGainMode::Album,
        }
    }
}

impl From<ReplayGainMode> for ReplayGainModeInfo {
    fn from(m: ReplayGainMode) -> Self {
        match m {
            ReplayGainMode::Off => ReplayGainModeInfo::Off,
            ReplayGainMode::Track => ReplayGainModeInfo::Track,
            ReplayGainMode::Album => ReplayGainModeInfo::Album,
        }
    }
}

impl From<ReplayGainSettingsInfo> for ReplayGainSettings {
    fn from(s: ReplayGainSettingsInfo) -> Self {
        ReplayGainSettings {
            mode: s.mode.into(),
            preamp_db: s.preamp_db,
        }
    }
}

impl From<ReplayGainSettings> for ReplayGainSettingsInfo {
    fn from(s: ReplayGainSettings) -> Self {
        ReplayGainSettingsInfo {
            mode: s.mode.into(),
            preamp_db: s.preamp_db,
        }
    }
}

impl From<PlayerState> for PlaybackState {
    fn from(s: PlayerState) -> Self {
        match s {
            PlayerState::Idle => PlaybackState::Idle,
            PlayerState::Loading => PlaybackState::Loading,
            PlayerState::Buffering => PlaybackState::Buffering,
            PlayerState::Playing => PlaybackState::Playing,
            PlayerState::Paused => PlaybackState::Paused,
        }
    }
}

impl From<EngineRepeatMode> for RepeatMode {
    fn from(m: EngineRepeatMode) -> Self {
        match m {
            EngineRepeatMode::Off => RepeatMode::Off,
            EngineRepeatMode::All => RepeatMode::All,
            EngineRepeatMode::One => RepeatMode::One,
        }
    }
}

impl From<RepeatMode> for EngineRepeatMode {
    fn from(m: RepeatMode) -> Self {
        match m {
            RepeatMode::Off => EngineRepeatMode::Off,
            RepeatMode::All => EngineRepeatMode::All,
            RepeatMode::One => EngineRepeatMode::One,
        }
    }
}

impl From<AlbumSort> for LibraryAlbumSort {
    fn from(s: AlbumSort) -> Self {
        match s {
            AlbumSort::Artist => LibraryAlbumSort::Artist,
            AlbumSort::Title => LibraryAlbumSort::Title,
            AlbumSort::Year => LibraryAlbumSort::Year,
            AlbumSort::RecentlyAdded => LibraryAlbumSort::RecentlyAdded,
        }
    }
}

impl From<&StreamInfo> for StreamInfoRecord {
    fn from(info: &StreamInfo) -> Self {
        StreamInfoRecord {
            codec: info.codec.clone(),
            sample_rate: info.sample_rate,
            channels: info.channels,
            bits_per_sample: info.bits_per_sample,
            duration_ms: info.duration.map(duration_ms),
            label: info.label(),
        }
    }
}

impl From<ScanProgress> for ScanProgressInfo {
    fn from(p: ScanProgress) -> Self {
        ScanProgressInfo {
            total: p.total,
            done: p.done,
            added: p.added,
            updated: p.updated,
            removed: p.removed,
            failed: p.failed,
        }
    }
}

impl From<ScanReport> for ScanSummary {
    fn from(r: ScanReport) -> Self {
        ScanSummary {
            progress: r.progress.into(),
            failures: r
                .failures
                .into_iter()
                .map(|f| ScanFailureInfo {
                    key: f.key,
                    error: f.error,
                })
                .collect(),
            pruned_albums: r.prune.albums,
            pruned_artists: r.prune.artists,
            pruned_covers: r.prune.covers,
        }
    }
}

impl From<SyncReport> for SyncSummary {
    fn from(r: SyncReport) -> Self {
        SyncSummary {
            playlists_applied: r.playlists_applied,
            favorites_applied: r.favorites_applied,
            playlists_pushed: r.playlists_pushed,
            favorites_pushed: r.favorites_pushed,
        }
    }
}

impl From<LibraryStats> for LibraryStatsInfo {
    fn from(s: LibraryStats) -> Self {
        LibraryStatsInfo {
            tracks: s.tracks,
            albums: s.albums,
            artists: s.artists,
            total_bytes: s.total_bytes,
            total_duration_ms: s.total_duration_ms,
        }
    }
}

impl From<LibraryUploadState> for UploadState {
    fn from(state: LibraryUploadState) -> Self {
        match state {
            LibraryUploadState::Queued => UploadState::Queued,
            LibraryUploadState::Uploading => UploadState::Uploading,
            LibraryUploadState::Done => UploadState::Done,
            LibraryUploadState::Duplicate => UploadState::Duplicate,
            LibraryUploadState::Failed => UploadState::Failed,
            LibraryUploadState::Cancelled => UploadState::Cancelled,
        }
    }
}

impl From<UploadRow> for UploadJobInfo {
    fn from(row: UploadRow) -> Self {
        UploadJobInfo {
            id: row.id,
            display_name: row.display_name,
            key: row.key,
            size: row.size,
            bytes_done: row.bytes_done,
            state: row.state.into(),
            error: row.error,
            source_uri: row.source_uri,
        }
    }
}

impl From<LyricLine> for LyricLineInfo {
    fn from(l: LyricLine) -> Self {
        LyricLineInfo {
            time_ms: l.time_ms,
            text: l.text,
        }
    }
}

impl From<LyricsSource> for LyricsSourceInfo {
    fn from(s: LyricsSource) -> Self {
        match s {
            LyricsSource::Sidecar => LyricsSourceInfo::Sidecar,
            LyricsSource::Embedded => LyricsSourceInfo::Embedded,
        }
    }
}

impl From<TrackLyrics> for LyricsInfo {
    fn from(t: TrackLyrics) -> Self {
        LyricsInfo {
            synced: t
                .synced
                .map(|lines| lines.into_iter().map(LyricLineInfo::from).collect()),
            plain: t.plain.unwrap_or_default(),
            source: t.source.into(),
        }
    }
}

impl From<Artist> for ArtistInfo {
    fn from(a: Artist) -> Self {
        ArtistInfo {
            id: a.id,
            name: a.name,
            album_count: a.album_count,
            track_count: a.track_count,
        }
    }
}

/// Saturating conversion of a duration to whole milliseconds.
pub(crate) fn duration_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Parses a library track id from the engine's string form (`TrackRef.id`).
pub(crate) fn parse_track_id(s: &str) -> Option<i64> {
    s.parse::<i64>().ok()
}

/// Turns library rows into UI records. Covers are resolved to file paths and favorites are
/// looked up once, when the converter is created; cover lookups are cached per converter.
pub(crate) struct Converter<'a> {
    lib: &'a Library,
    favorites: HashSet<String>,
    covers: RefCell<HashMap<i64, Option<String>>>,
}

impl<'a> Converter<'a> {
    /// Creates a converter over `lib`, loading the current favorite keys.
    pub(crate) fn new(lib: &'a Library) -> CoreResult<Self> {
        Ok(Self {
            lib,
            favorites: lib.favorite_keys()?,
            covers: RefCell::new(HashMap::new()),
        })
    }

    /// Absolute path of the cover with this id, or `None` for no cover. A lookup error is
    /// logged and treated as no cover.
    pub(crate) fn cover(&self, cover_id: Option<i64>) -> Option<String> {
        let id = cover_id?;
        if let Some(hit) = self.covers.borrow().get(&id) {
            return hit.clone();
        }
        let path = match self.lib.cover_path(id) {
            Ok(path) => path.map(|p| p.to_string_lossy().into_owned()),
            Err(e) => {
                log::warn!("cover {id} lookup failed: {e}");
                None
            }
        };
        self.covers.borrow_mut().insert(id, path.clone());
        path
    }

    /// Converts one track.
    pub(crate) fn track(&self, t: Track) -> TrackInfo {
        TrackInfo {
            is_favorite: self.favorites.contains(&t.key),
            cover_path: self.cover(t.cover_id),
            id: t.id,
            key: t.key,
            title: t.title,
            artist: t.artist,
            album_artist: t.album_artist,
            album: t.album,
            album_id: t.album_id,
            artist_id: t.artist_id,
            track_no: t.track_no,
            disc_no: t.disc_no,
            year: t.year,
            genre: t.genre,
            duration_ms: t.duration_ms,
            sample_rate: t.sample_rate,
            bits_per_sample: t.bits_per_sample,
            channels: t.channels,
            codec: t.codec,
            size: t.size,
            has_lyrics: t.has_lyrics,
        }
    }

    /// Converts a list of tracks, keeping order.
    pub(crate) fn tracks(&self, tracks: Vec<Track>) -> Vec<TrackInfo> {
        tracks.into_iter().map(|t| self.track(t)).collect()
    }

    /// Converts one album.
    pub(crate) fn album(&self, a: Album) -> AlbumInfo {
        AlbumInfo {
            cover_path: self.cover(a.cover_id),
            id: a.id,
            title: a.title,
            artist: a.artist,
            artist_id: a.artist_id,
            year: a.year,
            track_count: a.track_count,
            duration_ms: a.duration_ms,
            added_at: a.added_at,
        }
    }

    /// Converts a list of albums, keeping order.
    pub(crate) fn albums(&self, albums: Vec<Album>) -> Vec<AlbumInfo> {
        albums.into_iter().map(|a| self.album(a)).collect()
    }

    /// Converts one playlist.
    pub(crate) fn playlist(&self, p: Playlist) -> PlaylistInfo {
        PlaylistInfo {
            cover_path: self.cover(p.cover_id),
            id: p.id,
            name: p.name,
            track_count: p.track_count,
            duration_ms: p.duration_ms,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }

    /// Converts one playlist slot.
    pub(crate) fn entry(&self, e: PlaylistEntry) -> PlaylistEntryInfo {
        PlaylistEntryInfo {
            position: e.position,
            track_key: e.track_key,
            track: e.track.map(|t| self.track(t)),
        }
    }

    /// Converts search results.
    pub(crate) fn search(&self, r: SearchResults) -> SearchResult {
        SearchResult {
            tracks: self.tracks(r.tracks),
            albums: self.albums(r.albums),
            artists: r.artists.into_iter().map(ArtistInfo::from).collect(),
        }
    }
}

/// Resolves a track id to its UI record. `None` when the id is unknown.
fn resolve_track(lib: &Library, conv: Option<&Converter<'_>>, id: i64) -> Option<TrackInfo> {
    let track = lib.track(id).ok()?;
    match conv {
        Some(c) => Some(c.track(track)),
        None => Converter::new(lib).ok().map(|c| c.track(track)),
    }
}

/// Maps one engine event to the core event sent to the UI. Returns `None` for events that
/// cannot be represented (an unparsable track id in an event that needs one), and for
/// `GainApplied`, which is not sent as an event: the core keeps it as status and reports it in
/// `SignalPathInfo::replay_gain_db`.
///
/// `conv` is reused for track lookups when given; otherwise a temporary converter is built.
pub(crate) fn player_event(
    event: PlayerEvent,
    conv: Option<&Converter<'_>>,
    lib: &Library,
) -> Option<CoreEvent> {
    Some(match event {
        PlayerEvent::GainApplied { .. } => return None,
        PlayerEvent::SleepTimerChanged {
            remaining,
            end_of_track,
        } => CoreEvent::SleepTimerChanged {
            remaining_ms: remaining.map(duration_ms),
            end_of_track,
        },
        PlayerEvent::StateChanged(state) => CoreEvent::StateChanged {
            state: state.into(),
        },
        PlayerEvent::TrackLoaded { track_id, info } => CoreEvent::TrackLoaded {
            track_id: parse_track_id(&track_id)?,
            stream: StreamInfoRecord::from(&info),
        },
        PlayerEvent::Position {
            track_id,
            position,
            duration,
        } => CoreEvent::Position {
            track_id: parse_track_id(&track_id)?,
            position_ms: duration_ms(position),
            duration_ms: duration.map(duration_ms),
        },
        PlayerEvent::TrackEnded { track_id } => CoreEvent::TrackEnded {
            track_id: parse_track_id(&track_id)?,
        },
        PlayerEvent::Error { track_id, message } => CoreEvent::PlaybackError {
            track_id: track_id.as_deref().and_then(parse_track_id),
            message,
        },
        PlayerEvent::Recovering {
            track_id,
            attempt,
            position,
            retry_in,
        } => CoreEvent::Recovering {
            track_id: parse_track_id(&track_id)?,
            attempt,
            position_ms: duration_ms(position),
            retry_in_ms: duration_ms(retry_in),
        },
        PlayerEvent::TrackChanged { entry_id, track_id } => CoreEvent::TrackChanged {
            entry_id,
            track: resolve_track(lib, conv, parse_track_id(&track_id)?),
        },
        PlayerEvent::QueueChanged {
            entries,
            current,
            shuffle,
            repeat,
        } => CoreEvent::QueueChanged {
            items: entries
                .iter()
                .filter_map(|e| {
                    parse_track_id(&e.track.id).map(|track_id| QueueItem {
                        entry_id: e.id,
                        track_id,
                    })
                })
                .collect(),
            current,
            shuffle,
            repeat: repeat.into(),
        },
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use sonorus_engine::opener::TrackRef;
    use sonorus_engine::queue::QueueEntry;
    use sonorus_library::{ObjectStamp, TrackMeta};

    use super::*;

    const SECRET: &str = "wJalrXUtnFEMI-SECRET-VALUE-9z";
    const KEY_ID: &str = "AKIAEXAMPLEKEYID1234";

    /// Removes a temporary directory when dropped, even if the test panics.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir()
                .join(format!("sonorus-core-{tag}-{}-{nanos}", std::process::id()));
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

    fn settings(endpoint: &str, region: &str, prefix: &str) -> S3Settings {
        S3Settings {
            endpoint: endpoint.to_string(),
            region: region.to_string(),
            bucket: " music ".to_string(),
            access_key_id: KEY_ID.to_string(),
            secret_access_key: SECRET.to_string(),
            path_style: true,
            prefix: prefix.to_string(),
        }
    }

    #[test]
    fn s3_debug_redacts_secret_and_truncates_key_id() {
        let text = format!("{:?}", settings("https://s3.example.com", "auto", ""));
        assert!(!text.contains(SECRET), "secret leaked: {text}");
        assert!(!text.contains("EXAMPLEKEYID"), "key id leaked: {text}");
        assert!(text.contains("AKIA…"), "missing key hint: {text}");
        assert!(
            text.contains("<redacted>"),
            "missing redaction marker: {text}"
        );

        let mut short = settings("https://s3.example.com", "auto", "");
        short.access_key_id = "AKIA".to_string();
        let text = format!("{short:?}");
        assert!(text.contains("access_key_id: \"…\""), "{text}");
        assert!(!text.contains("AKIA"), "{text}");
    }

    #[test]
    fn core_config_debug_does_not_leak_secret() {
        let config = CoreConfig {
            data_dir: "/data".to_string(),
            s3: Some(settings("https://s3.example.com", "auto", "")),
            local_music_dir: None,
        };
        assert!(!format!("{config:?}").contains(SECRET));
    }

    #[test]
    fn to_storage_config_trims_and_normalizes_prefix() {
        let mut s = settings("  https://s3.us-west-004.backblazeb2.com/ ", "", "/music/");
        s.access_key_id = " AKIAEXAMPLEKEYID1234 ".to_string();
        s.secret_access_key = " SECRET ".to_string();
        let cfg = s.to_storage_config();
        assert_eq!(cfg.endpoint, "https://s3.us-west-004.backblazeb2.com/");
        assert_eq!(cfg.region, "us-west-004");
        assert_eq!(cfg.bucket, "music");
        assert_eq!(cfg.access_key_id, "AKIAEXAMPLEKEYID1234");
        assert_eq!(cfg.secret_access_key, "SECRET");
        assert_eq!(cfg.prefix, "music/");
        assert!(cfg.path_style);
    }

    #[test]
    fn to_storage_config_region_fallback_and_explicit() {
        let cfg = settings("https://example.r2.cloudflarestorage.com", "", "").to_storage_config();
        assert_eq!(cfg.region, "auto");
        assert_eq!(cfg.prefix, "");

        let cfg = settings("https://s3.example.com", " eu-1 ", "").to_storage_config();
        assert_eq!(cfg.region, "eu-1");
    }

    #[test]
    fn repeat_mode_round_trips() {
        for mode in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
            let engine: EngineRepeatMode = mode.into();
            assert_eq!(RepeatMode::from(engine), mode);
        }
        assert_eq!(
            EngineRepeatMode::from(RepeatMode::All),
            EngineRepeatMode::All
        );
    }

    #[test]
    fn playback_state_and_album_sort_map() {
        assert_eq!(
            PlaybackState::from(PlayerState::Paused),
            PlaybackState::Paused
        );
        assert_eq!(
            LibraryAlbumSort::from(AlbumSort::RecentlyAdded),
            LibraryAlbumSort::RecentlyAdded
        );
    }

    #[test]
    fn output_sample_format_round_trips() {
        for (format, kind) in [
            (OutputSampleFormat::Float, SampleKind::F32),
            (OutputSampleFormat::Int32, SampleKind::I32),
            (OutputSampleFormat::Int24Packed, SampleKind::I24Packed),
            (OutputSampleFormat::Int16, SampleKind::I16),
        ] {
            assert_eq!(SampleKind::from(format), kind);
            assert_eq!(OutputSampleFormat::from(kind), format);
        }
        assert_eq!(OutputSampleFormat::default(), OutputSampleFormat::Float);
    }

    #[test]
    fn output_settings_default_is_shared_float() {
        let d = OutputSettings::default();
        assert_eq!(d.sample_format, OutputSampleFormat::Float);
        assert!(!d.exclusive);
    }

    #[test]
    fn output_settings_aaudio_config_maps_sharing() {
        let shared = OutputSettings {
            sample_format: OutputSampleFormat::Int32,
            exclusive: false,
        }
        .aaudio_config();
        assert_eq!(shared, AAudioConfig::default());
        assert_eq!(shared.sharing, Sharing::Shared);
        assert_eq!(shared.performance, Performance::PowerSaving);
        assert_eq!(shared.device_id, None);

        let exclusive = OutputSettings {
            sample_format: OutputSampleFormat::Float,
            exclusive: true,
        }
        .aaudio_config();
        assert_eq!(exclusive.sharing, Sharing::Exclusive);
        assert_eq!(exclusive.performance, Performance::PowerSaving);
        assert_eq!(exclusive.device_id, None);
    }

    #[test]
    fn output_info_from_maps_every_field() {
        let info = AAudioStreamInfo {
            sample_rate: 96_000,
            channels: 2,
            sample: SampleKind::I24Packed,
            sharing: Sharing::Exclusive,
            performance: Performance::LowLatency,
            device_id: 7,
            frames_per_burst: 192,
            buffer_frames: 768,
        };
        assert_eq!(
            output_info_from(info, 3),
            OutputInfo {
                backend: "aaudio".to_owned(),
                sample_rate: 96_000,
                channels: 2,
                sample_format: OutputSampleFormat::Int24Packed,
                exclusive: true,
                low_latency: true,
                device_id: 7,
                frames_per_burst: 192,
                buffer_frames: 768,
                xruns: 3,
            }
        );
    }

    #[test]
    fn output_info_from_reports_shared_and_power_saving() {
        let info = AAudioStreamInfo {
            sample_rate: 48_000,
            channels: 2,
            sample: SampleKind::F32,
            sharing: Sharing::Shared,
            performance: Performance::PowerSaving,
            device_id: 0,
            frames_per_burst: 240,
            buffer_frames: 960,
        };
        let out = output_info_from(info, 0);
        assert!(!out.exclusive);
        assert!(!out.low_latency);
        assert_eq!(out.sample_format, OutputSampleFormat::Float);
        assert_eq!(out.xruns, 0);
    }

    #[test]
    fn path_mode_maps_both_ways() {
        for (mode, out) in [
            (PathMode::BitPerfect, OutputPathMode::BitPerfect),
            (PathMode::Exclusive, OutputPathMode::Exclusive),
            (PathMode::Mixer, OutputPathMode::Mixer),
        ] {
            assert_eq!(OutputPathMode::from(mode), out);
            assert_eq!(PathMode::from(out), mode);
        }
    }

    #[test]
    fn route_request_maps_fields_and_exclusive_from_sharing() {
        let request = |sharing| RouteRequest {
            sample_rate: 96_000,
            channels: 2,
            source_bits: Some(24),
            requested: SampleKind::I24Packed,
            config: AAudioConfig {
                sharing,
                performance: Performance::None,
                device_id: Some(5),
            },
        };
        assert_eq!(
            RouteRequestInfo::from(request(Sharing::Exclusive)),
            RouteRequestInfo {
                sample_rate: 96_000,
                channels: 2,
                source_bits: Some(24),
                requested: OutputSampleFormat::Int24Packed,
                exclusive: true,
            }
        );
        assert!(!RouteRequestInfo::from(request(Sharing::Shared)).exclusive);
    }

    #[test]
    fn route_plan_info_to_plan_sets_sharing_and_performance() {
        let info = |exclusive| RoutePlanInfo {
            device_id: Some(9),
            sample_format: OutputSampleFormat::Int16,
            exclusive,
            bit_perfect: true,
            hold: true,
        };
        assert_eq!(
            RoutePlan::from(info(true)),
            RoutePlan {
                device_id: Some(9),
                sample: SampleKind::I16,
                sharing: Sharing::Exclusive,
                performance: Performance::LowLatency,
                bit_perfect: true,
                hold: true,
            }
        );
        assert_eq!(
            RoutePlan::from(info(false)),
            RoutePlan {
                device_id: Some(9),
                sample: SampleKind::I16,
                sharing: Sharing::Shared,
                performance: Performance::PowerSaving,
                bit_perfect: true,
                hold: true,
            }
        );
    }

    #[test]
    fn route_plan_to_info_keeps_sharing_as_exclusive_flag() {
        let plan = |sharing, performance| RoutePlan {
            device_id: None,
            sample: SampleKind::I32,
            sharing,
            performance,
            bit_perfect: false,
            hold: false,
        };
        let exclusive = RoutePlanInfo::from(plan(Sharing::Exclusive, Performance::LowLatency));
        assert_eq!(
            exclusive,
            RoutePlanInfo {
                device_id: None,
                sample_format: OutputSampleFormat::Int32,
                exclusive: true,
                bit_perfect: false,
                hold: false,
            }
        );
        assert!(!RoutePlanInfo::from(plan(Sharing::Shared, Performance::None)).exclusive);
    }

    #[test]
    fn route_plan_to_info_exports_hold() {
        let held = RoutePlan {
            device_id: Some(4),
            sample: SampleKind::I32,
            sharing: Sharing::Shared,
            performance: Performance::PowerSaving,
            bit_perfect: false,
            hold: true,
        };
        assert!(RoutePlanInfo::from(held).hold);
    }

    #[test]
    fn route_plan_info_round_trips_through_plan() {
        for exclusive in [true, false] {
            for hold in [true, false] {
                let info = RoutePlanInfo {
                    device_id: Some(2),
                    sample_format: OutputSampleFormat::Int24Packed,
                    exclusive,
                    bit_perfect: true,
                    hold,
                };
                assert_eq!(RoutePlanInfo::from(RoutePlan::from(info)), info);
            }
        }
    }

    #[test]
    fn signal_path_info_exports_stream_with_zero_xruns() {
        let path = SignalPath {
            mode: PathMode::BitPerfect,
            plan: RoutePlan {
                device_id: Some(7),
                sample: SampleKind::I24Packed,
                sharing: Sharing::Exclusive,
                performance: Performance::LowLatency,
                bit_perfect: true,
                hold: false,
            },
            stream: Some(AAudioStreamInfo {
                sample_rate: 96_000,
                channels: 2,
                sample: SampleKind::I24Packed,
                sharing: Sharing::Exclusive,
                performance: Performance::LowLatency,
                device_id: 7,
                frames_per_burst: 192,
                buffer_frames: 768,
            }),
            error: None,
            source_rate: 96_000,
            source_bits: Some(24),
        };
        assert_eq!(
            SignalPathInfo::from(path),
            SignalPathInfo {
                mode: OutputPathMode::BitPerfect,
                plan: RoutePlanInfo {
                    device_id: Some(7),
                    sample_format: OutputSampleFormat::Int24Packed,
                    exclusive: true,
                    bit_perfect: true,
                    hold: false,
                },
                stream: Some(OutputInfo {
                    backend: "aaudio".to_owned(),
                    sample_rate: 96_000,
                    channels: 2,
                    sample_format: OutputSampleFormat::Int24Packed,
                    exclusive: true,
                    low_latency: true,
                    device_id: 7,
                    frames_per_burst: 192,
                    buffer_frames: 768,
                    xruns: 0,
                }),
                error: None,
                source_rate: 96_000,
                source_bits: Some(24),
                replay_gain_db: None,
            }
        );
    }

    #[test]
    fn signal_path_info_keeps_error_when_open_failed() {
        let path = SignalPath {
            mode: PathMode::Mixer,
            plan: RoutePlan {
                device_id: None,
                sample: SampleKind::F32,
                sharing: Sharing::Shared,
                performance: Performance::None,
                bit_perfect: false,
                hold: false,
            },
            stream: None,
            error: Some("device gone".to_owned()),
            source_rate: 44_100,
            source_bits: None,
        };
        let info = SignalPathInfo::from(path);
        assert_eq!(info.mode, OutputPathMode::Mixer);
        assert_eq!(info.stream, None);
        assert_eq!(info.error.as_deref(), Some("device gone"));
        assert_eq!(info.source_bits, None);
    }

    #[test]
    fn stream_info_record_from_engine() {
        let info = StreamInfo {
            codec: "FLAC".to_string(),
            sample_rate: 96_000,
            channels: 2,
            bits_per_sample: Some(24),
            duration: Some(Duration::from_millis(2_500)),
            total_frames: None,
        };
        let rec = StreamInfoRecord::from(&info);
        assert_eq!(rec.duration_ms, Some(2_500));
        assert_eq!(rec.label, "FLAC 24/96");
        assert_eq!(rec.channels, 2);
    }

    #[test]
    fn duration_ms_saturates() {
        assert_eq!(duration_ms(Duration::from_millis(1_234)), 1_234);
        assert_eq!(duration_ms(Duration::MAX), u64::MAX);
    }

    #[test]
    fn parse_track_id_rejects_non_numbers() {
        assert_eq!(parse_track_id("42"), Some(42));
        assert_eq!(parse_track_id("abc"), None);
        assert_eq!(parse_track_id(""), None);
    }

    #[test]
    fn scan_report_converts() {
        let report = ScanReport {
            progress: ScanProgress {
                total: 3,
                done: 3,
                added: 2,
                updated: 0,
                removed: 1,
                failed: 1,
            },
            failures: vec![sonorus_library::ScanFailure {
                key: "a.flac".to_string(),
                error: "bad tags".to_string(),
            }],
            prune: sonorus_library::PruneStats {
                albums: 1,
                artists: 2,
                covers: 3,
            },
        };
        let summary = ScanSummary::from(report);
        assert_eq!(summary.progress.failed, 1);
        assert_eq!(summary.failures[0].error, "bad tags");
        assert_eq!(
            (
                summary.pruned_albums,
                summary.pruned_artists,
                summary.pruned_covers
            ),
            (1, 2, 3)
        );
    }

    /// An in-memory library with two tracks; the first is a favorite and has a cover.
    struct Fixture {
        _dir: TempDir,
        lib: Library,
        fav_id: i64,
        plain_id: i64,
    }

    fn fixture(tag: &str) -> Fixture {
        let dir = TempDir::new(tag);
        let lib = Library::open_in_memory(&dir.path().join("covers")).expect("open library");
        let cover_id = lib
            .store_cover(b"\xff\xd8\xff fake jpeg", "image/jpeg")
            .expect("store cover");
        let meta = TrackMeta {
            title: Some("Song".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_artist: Some("Artist".to_string()),
            duration_ms: Some(180_000),
            ..TrackMeta::default()
        };
        let stamp = |key: &str| ObjectStamp {
            key: key.to_string(),
            size: 100,
            etag: None,
            last_modified: None,
        };
        let fav_id = lib
            .upsert_track(&stamp("Artist/Album/01 - Song.flac"), &meta, Some(cover_id))
            .expect("upsert fav");
        let plain_id = lib
            .upsert_track(&stamp("Artist/Album/02 - Other.flac"), &meta, None)
            .expect("upsert plain");
        lib.set_favorite("Artist/Album/01 - Song.flac", true)
            .expect("set favorite");
        Fixture {
            _dir: dir,
            lib,
            fav_id,
            plain_id,
        }
    }

    #[test]
    fn converter_sets_favorite_and_cover() {
        let f = fixture("conv");
        let conv = Converter::new(&f.lib).expect("converter");

        let fav = conv.track(f.lib.track(f.fav_id).expect("track"));
        assert!(fav.is_favorite);
        let cover = fav.cover_path.expect("cover path");
        assert!(cover.ends_with(".jpg"), "{cover}");
        assert!(Path::new(&cover).is_file());

        let plain = conv.track(f.lib.track(f.plain_id).expect("track"));
        assert!(!plain.is_favorite);
        assert_eq!(plain.cover_path, None);
    }

    #[test]
    fn converter_album_cover_resolves() {
        let f = fixture("album");
        let conv = Converter::new(&f.lib).expect("converter");
        let track = f.lib.track(f.fav_id).expect("track");
        let album = f.lib.album(track.album_id).expect("album");
        let info = conv.album(album);
        assert!(info.cover_path.is_some());
        assert_eq!(info.track_count, 2);
    }

    #[test]
    fn player_event_maps_positions_and_drops_bad_ids() {
        let f = fixture("events");
        let pos = player_event(
            PlayerEvent::Position {
                track_id: f.fav_id.to_string(),
                position: Duration::from_millis(1_500),
                duration: Some(Duration::from_secs(3)),
            },
            None,
            &f.lib,
        );
        assert_eq!(
            pos,
            Some(CoreEvent::Position {
                track_id: f.fav_id,
                position_ms: 1_500,
                duration_ms: Some(3_000),
            })
        );

        let bad = player_event(
            PlayerEvent::Position {
                track_id: "not-a-number".to_string(),
                position: Duration::ZERO,
                duration: None,
            },
            None,
            &f.lib,
        );
        assert_eq!(bad, None);

        let ended = player_event(
            PlayerEvent::TrackEnded {
                track_id: "x".to_string(),
            },
            None,
            &f.lib,
        );
        assert_eq!(ended, None);

        let err = player_event(
            PlayerEvent::Error {
                track_id: Some("x".to_string()),
                message: "boom".to_string(),
            },
            None,
            &f.lib,
        );
        assert_eq!(
            err,
            Some(CoreEvent::PlaybackError {
                track_id: None,
                message: "boom".to_string(),
            })
        );
    }

    #[test]
    fn player_event_track_changed_resolves_track() {
        let f = fixture("changed");
        let conv = Converter::new(&f.lib).expect("converter");
        let ev = |id: &str| PlayerEvent::TrackChanged {
            entry_id: 7,
            track_id: id.to_string(),
        };

        match player_event(ev(&f.fav_id.to_string()), Some(&conv), &f.lib) {
            Some(CoreEvent::TrackChanged {
                entry_id: 7,
                track: Some(t),
            }) => {
                assert!(t.is_favorite);
                assert_eq!(t.id, f.fav_id);
            }
            other => panic!("unexpected: {other:?}"),
        }
        match player_event(ev(&f.plain_id.to_string()), None, &f.lib) {
            Some(CoreEvent::TrackChanged { track: Some(t), .. }) => assert!(!t.is_favorite),
            other => panic!("unexpected: {other:?}"),
        }
        match player_event(ev("999999"), None, &f.lib) {
            Some(CoreEvent::TrackChanged { track: None, .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(player_event(ev("zz"), Some(&conv), &f.lib), None);
    }

    #[test]
    fn player_event_queue_skips_unparsable_ids() {
        let f = fixture("queue");
        let entry = |id: u64, track_id: &str| QueueEntry {
            id,
            track: TrackRef {
                id: track_id.to_string(),
                key: String::new(),
                size: 0,
                gain: None,
            },
        };
        let ev = PlayerEvent::QueueChanged {
            entries: vec![entry(1, "3"), entry(2, "x"), entry(4, "5")],
            current: Some(4),
            shuffle: true,
            repeat: EngineRepeatMode::All,
        };
        assert_eq!(
            player_event(ev, None, &f.lib),
            Some(CoreEvent::QueueChanged {
                items: vec![
                    QueueItem {
                        entry_id: 1,
                        track_id: 3
                    },
                    QueueItem {
                        entry_id: 4,
                        track_id: 5
                    },
                ],
                current: Some(4),
                shuffle: true,
                repeat: RepeatMode::All,
            })
        );
    }

    #[test]
    fn player_event_maps_sleep_timer_changed() {
        let f = fixture("sleep-event");
        let ev = |remaining: Option<Duration>, end_of_track: bool| {
            player_event(
                PlayerEvent::SleepTimerChanged {
                    remaining,
                    end_of_track,
                },
                None,
                &f.lib,
            )
        };
        assert_eq!(
            ev(Some(Duration::from_millis(90_000)), false),
            Some(CoreEvent::SleepTimerChanged {
                remaining_ms: Some(90_000),
                end_of_track: false,
            })
        );
        assert_eq!(
            ev(None, true),
            Some(CoreEvent::SleepTimerChanged {
                remaining_ms: None,
                end_of_track: true,
            })
        );
        assert_eq!(
            ev(None, false),
            Some(CoreEvent::SleepTimerChanged {
                remaining_ms: None,
                end_of_track: false,
            })
        );
    }

    #[test]
    fn player_event_gain_applied_is_status_not_an_event() {
        let f = fixture("gain-event");
        assert_eq!(
            player_event(PlayerEvent::GainApplied { db: Some(-3.0) }, None, &f.lib),
            None
        );
        assert_eq!(
            player_event(PlayerEvent::GainApplied { db: None }, None, &f.lib),
            None
        );
    }

    #[test]
    fn replay_gain_settings_convert_both_ways() {
        let info = ReplayGainSettingsInfo {
            mode: ReplayGainModeInfo::Album,
            preamp_db: -2.5,
        };
        let engine: ReplayGainSettings = info.into();
        assert_eq!(engine.mode, ReplayGainMode::Album);
        assert_eq!(engine.preamp_db, -2.5);
        assert_eq!(ReplayGainSettingsInfo::from(engine), info);
        assert_eq!(
            ReplayGainModeInfo::from(ReplayGainMode::Track),
            ReplayGainModeInfo::Track
        );
        assert_eq!(
            ReplayGainMode::from(ReplayGainModeInfo::Off),
            ReplayGainMode::Off
        );
    }
}
