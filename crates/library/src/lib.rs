//! Sonorus music library: SQLite catalogue, bucket scan, tags and cover art, search.

pub mod db;
pub mod error;
pub mod favorites;
pub mod folder;
pub mod history;
pub mod lrc;
pub mod model;
pub mod pins;
pub mod playlists;
pub mod quality;
pub mod query;
pub mod remote;
pub mod scan;
pub mod smart;
pub mod stats;
pub mod sync;
pub mod tags;
pub mod uploads;

pub use db::Library;
pub use error::{LibraryError, Result};
pub use folder::{CatalogueFolder, FolderEntry};
pub use lrc::{LyricLine, parse_lrc};
pub use model::{
    Album, AlbumSort, Artist, CoverImage, FavoriteRecord, LibraryStats, LyricsSource, ObjectStamp,
    Playlist, PlaylistEntry, PlaylistRecord, PruneStats, SearchResults, Track, TrackLyrics,
    TrackMeta,
};
pub use pins::{Pin, PinKind, PinnedTrack};
pub use quality::{QualityFilter, is_lossless, matches_quality};
pub use remote::RemoteReader;
pub use scan::{
    BoxFuture, BucketSource, DirSource, MetaSource, ScanFailure, ScanProgress, ScanReport, scan,
};
pub use smart::SmartPlaylist;
pub use stats::{AlbumPlays, ArtistPlays, ListeningStats, StatsRange, TrackPlays};
pub use sync::{
    BucketStore, DOC_VERSION, FAVORITES_FILE, FavoritesDoc, MemoryStore, PLAYLISTS_FILE,
    PlaylistsDoc, SyncReport, UserDataStore, merge_favorites, merge_playlists, sync_user_data,
};
pub use tags::{folder_cover_key, read_meta};
