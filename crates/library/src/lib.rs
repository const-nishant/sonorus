//! Sonorus music library: SQLite catalogue, bucket scan, tags and cover art, search.

pub mod db;
pub mod error;
pub mod model;
pub mod query;
pub mod remote;
pub mod scan;
pub mod tags;

pub use db::Library;
pub use error::{LibraryError, Result};
pub use model::{
    Album, AlbumSort, Artist, CoverImage, LibraryStats, ObjectStamp, PruneStats, SearchResults,
    Track, TrackMeta,
};
pub use remote::RemoteReader;
pub use scan::{
    BoxFuture, BucketSource, DirSource, MetaSource, ScanFailure, ScanProgress, ScanReport, scan,
};
pub use tags::{folder_cover_key, read_meta};
