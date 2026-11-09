//! Plain data types shared by the library crate.

use serde::{Deserialize, Serialize};

/// Identity and change-detection data for one object in the bucket (or local folder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectStamp {
    pub key: String,
    pub size: u64,
    pub etag: Option<String>,
    pub last_modified: Option<i64>,
}

/// Embedded or folder cover art.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverImage {
    pub data: Vec<u8>,
    pub mime: String,
}

/// Metadata read from an audio file's tags and stream properties.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackMeta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub lyrics: Option<String>,
    pub codec: Option<String>,
    pub track_no: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_no: Option<u32>,
    pub disc_total: Option<u32>,
    pub year: Option<i32>,
    pub duration_ms: Option<u64>,
    pub sample_rate: Option<u32>,
    pub bits_per_sample: Option<u8>,
    pub channels: Option<u8>,
    pub bitrate_kbps: Option<u32>,
    pub cover: Option<CoverImage>,
    /// ReplayGain track gain in dB (`REPLAYGAIN_TRACK_GAIN`), e.g. `-6.54`.
    pub track_gain_db: Option<f32>,
    /// ReplayGain track peak as a linear amplitude (`REPLAYGAIN_TRACK_PEAK`), e.g. `0.988525`.
    pub track_peak: Option<f32>,
    /// ReplayGain album gain in dB (`REPLAYGAIN_ALBUM_GAIN`).
    pub album_gain_db: Option<f32>,
    /// ReplayGain album peak as a linear amplitude (`REPLAYGAIN_ALBUM_PEAK`).
    pub album_peak: Option<f32>,
}

/// A catalogued track with its album and artist names resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: i64,
    pub key: String,
    pub size: u64,
    pub etag: Option<String>,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub album_id: i64,
    pub artist_id: i64,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub duration_ms: Option<u64>,
    pub sample_rate: Option<u32>,
    pub bits_per_sample: Option<u8>,
    pub channels: Option<u8>,
    pub codec: Option<String>,
    pub cover_id: Option<i64>,
    pub has_lyrics: bool,
    pub added_at: i64,
    /// ReplayGain track gain in dB, if the file carries it.
    pub track_gain_db: Option<f32>,
    /// ReplayGain track peak (linear amplitude), if the file carries it.
    pub track_peak: Option<f32>,
    /// ReplayGain album gain in dB, if the file carries it.
    pub album_gain_db: Option<f32>,
    /// ReplayGain album peak (linear amplitude), if the file carries it.
    pub album_peak: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub artist_id: i64,
    pub year: Option<i32>,
    pub track_count: u32,
    pub duration_ms: u64,
    pub cover_id: Option<i64>,
    pub added_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artist {
    pub id: i64,
    pub name: String,
    pub album_count: u32,
    pub track_count: u32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlbumSort {
    #[default]
    Artist,
    Title,
    Year,
    RecentlyAdded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryStats {
    pub tracks: u64,
    pub albums: u64,
    pub artists: u64,
    pub total_bytes: u64,
    pub total_duration_ms: u64,
}

/// Counts of rows removed by `Library::prune`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PruneStats {
    pub albums: u64,
    pub artists: u64,
    pub covers: u64,
}

/// A user playlist (not deleted). Timestamps are Unix milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    /// Number of entries, including ones whose track is not in the catalogue.
    pub track_count: u32,
    /// Sum of durations of the entries whose track is catalogued.
    pub duration_ms: u64,
    /// Cover of the first catalogued entry.
    pub cover_id: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One playlist slot. `track` is `None` when the key is not (or no longer) catalogued.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistEntry {
    pub position: u32,
    pub track_key: String,
    pub track: Option<Track>,
}

/// Sync form of a playlist (`_sonorus/playlists.json`). Last writer wins per `uuid` by
/// `updated_at` (Unix ms). Deleted playlists stay as tombstones (`deleted = true`, no tracks).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistRecord {
    pub uuid: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub deleted: bool,
    /// Object keys in playlist order.
    #[serde(default)]
    pub tracks: Vec<String>,
}

/// Sync form of a favorite (`_sonorus/favorites.json`). Last writer wins per `track_key` by
/// `updated_at` (Unix ms); un-favoriting keeps the row with `favorite = false`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteRecord {
    pub track_key: String,
    pub favorite: bool,
    pub updated_at: i64,
}
