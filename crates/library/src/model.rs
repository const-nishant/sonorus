//! Plain data types shared by the library crate.

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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
}

/// A catalogued track with its album and artist names resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
