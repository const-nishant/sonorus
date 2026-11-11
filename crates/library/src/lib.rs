//! Sonorus music library: SQLite catalogue, bucket scan, tags and cover art, search.

pub mod db;
pub mod error;
pub mod favorites;
pub mod history;
pub mod lrc;
pub mod model;
pub mod pins;
pub mod playlists;
pub mod query;
pub mod remote;
pub mod scan;
pub mod sync;
pub mod tags;
pub mod uploads;

pub use db::Library;
pub use error::{LibraryError, Result};
pub use lrc::{LyricLine, parse_lrc};
pub use model::{
    Album, AlbumSort, Artist, CoverImage, FavoriteRecord, LibraryStats, LyricsSource, ObjectStamp,
    Playlist, PlaylistEntry, PlaylistRecord, PruneStats, SearchResults, Track, TrackLyrics,
    TrackMeta,
};
pub use pins::{Pin, PinKind, PinnedTrack};
pub use remote::RemoteReader;
pub use scan::{
    BoxFuture, BucketSource, DirSource, MetaSource, ScanFailure, ScanProgress, ScanReport, scan,
};
pub use sync::{
    BucketStore, DOC_VERSION, FAVORITES_FILE, FavoritesDoc, MemoryStore, PLAYLISTS_FILE,
    PlaylistsDoc, SyncReport, UserDataStore, merge_favorites, merge_playlists, sync_user_data,
};
pub use tags::{folder_cover_key, read_meta};
