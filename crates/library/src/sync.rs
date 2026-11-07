//! Bucket sync of user data (playlists and favorites) through a [`UserDataStore`].
//!
//! Each collection is one JSON document in the bucket (`_sonorus/playlists.json`,
//! `_sonorus/favorites.json`). A sync merges the remote document with the local records,
//! applies whatever is newer to the local database, and pushes the merged result back when
//! it differs from the remote one.
//!
//! Conflicts are resolved per record by last writer wins: the record with the greater
//! `updated_at` (Unix ms) wins. Equal timestamps are broken by a fixed rule (see
//! [`merge_playlists`] and [`merge_favorites`]) so every device picks the same record.
//! Deleted playlists are never removed from the document; they stay as tombstones
//! (`deleted = true`) so the deletion propagates to other devices.
//!
//! Listening history is device-local and is not synced.
//!
//! A remote document that fails to parse, or whose `version` is newer than [`DOC_VERSION`],
//! aborts the sync with [`LibraryError::Sync`] before anything is applied or pushed, so a
//! newer or corrupt remote is never overwritten.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sonorus_storage::{Storage, StorageError, normalize_prefix};

use crate::db::Library;
use crate::error::{LibraryError, Result};
use crate::model::{FavoriteRecord, PlaylistRecord};
use crate::scan::BoxFuture;

/// Bucket object key of the playlists document, relative to the store prefix.
pub const PLAYLISTS_FILE: &str = "_sonorus/playlists.json";
/// Bucket object key of the favorites document, relative to the store prefix.
pub const FAVORITES_FILE: &str = "_sonorus/favorites.json";
/// Format version written into documents. Documents with a greater version are refused.
pub const DOC_VERSION: u32 = 1;

/// The playlists document (`_sonorus/playlists.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistsDoc {
    pub version: u32,
    pub playlists: Vec<PlaylistRecord>,
}

/// The favorites document (`_sonorus/favorites.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoritesDoc {
    pub version: u32,
    pub favorites: Vec<FavoriteRecord>,
}

/// Byte-level access to the user-data documents. Keys are the `*_FILE` constants.
pub trait UserDataStore: Send + Sync {
    /// Returns the object's bytes, or `Ok(None)` when the object does not exist.
    fn get(&self, key: &str) -> BoxFuture<'_, Result<Option<Vec<u8>>>>;
    /// Replaces the object's bytes.
    fn put(&self, key: &str, data: Vec<u8>) -> BoxFuture<'_, Result<()>>;
}

/// Object key for `file` under `prefix`.
fn object_key(prefix: &str, file: &str) -> String {
    format!("{prefix}{file}")
}

/// [`UserDataStore`] backed by an S3-compatible bucket. Objects are stored under a
/// normalized prefix (see [`normalize_prefix`]).
pub struct BucketStore {
    storage: Storage,
    prefix: String,
}

impl BucketStore {
    /// Creates a store over `storage`; `prefix` is normalized with [`normalize_prefix`].
    pub fn new(storage: Storage, prefix: &str) -> Self {
        Self {
            storage,
            prefix: normalize_prefix(prefix),
        }
    }
}

impl UserDataStore for BucketStore {
    fn get(&self, key: &str) -> BoxFuture<'_, Result<Option<Vec<u8>>>> {
        let key = object_key(&self.prefix, key);
        Box::pin(async move {
            match self.storage.get_bytes(&key).await {
                Ok(bytes) => Ok(Some(bytes.to_vec())),
                Err(StorageError::NotFound(_)) => Ok(None),
                Err(e) => Err(LibraryError::Storage(e)),
            }
        })
    }

    fn put(&self, key: &str, data: Vec<u8>) -> BoxFuture<'_, Result<()>> {
        let key = object_key(&self.prefix, key);
        Box::pin(async move {
            self.storage
                .put_bytes(&key, Bytes::from(data), Some("application/json"))
                .await?;
            Ok(())
        })
    }
}

/// In-memory [`UserDataStore`] for tests and local experiments.
#[derive(Debug, Default)]
pub struct MemoryStore {
    objects: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Synchronous read of one object.
    pub fn get_now(&self, key: &str) -> Option<Vec<u8>> {
        self.lock().get(key).cloned()
    }

    /// Synchronous write of one object.
    pub fn put_now(&self, key: &str, data: Vec<u8>) {
        self.lock().insert(key.to_string(), data);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<u8>>> {
        self.objects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl UserDataStore for MemoryStore {
    fn get(&self, key: &str) -> BoxFuture<'_, Result<Option<Vec<u8>>>> {
        let key = key.to_string();
        Box::pin(async move { Ok(self.get_now(&key)) })
    }

    fn put(&self, key: &str, data: Vec<u8>) -> BoxFuture<'_, Result<()>> {
        let key = key.to_string();
        Box::pin(async move {
            self.put_now(&key, data);
            Ok(())
        })
    }
}

/// What a sync changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Local playlist rows created or overwritten from the remote document.
    pub playlists_applied: u32,
    /// Local favorite rows created or overwritten from the remote document.
    pub favorites_applied: u32,
    /// Whether the playlists document was written to the store.
    pub playlists_pushed: bool,
    /// Whether the favorites document was written to the store.
    pub favorites_pushed: bool,
}

/// Total order used to pick the winner of two playlist records with the same `uuid`.
/// The greatest key wins: `updated_at` first, then `deleted` (true wins), then the greater
/// `name`, then the greater `tracks`, then `created_at`. The last field only matters for
/// records that are otherwise identical, and keeps the merge symmetric.
pub(crate) fn playlist_key(r: &PlaylistRecord) -> (i64, bool, &str, &[String], i64) {
    (
        r.updated_at,
        r.deleted,
        r.name.as_str(),
        r.tracks.as_slice(),
        r.created_at,
    )
}

/// Total order used to pick the winner of two favorite records with the same `track_key`.
/// The greatest key wins: `updated_at` first, then `favorite == false` wins.
pub(crate) fn favorite_key(r: &FavoriteRecord) -> (i64, bool) {
    (r.updated_at, !r.favorite)
}

/// Merges two playlist lists by `uuid` with last writer wins (see [`playlist_key`]).
/// Pure: the result is the union of both lists, one record per uuid, sorted by uuid, and
/// `merge_playlists(a, b) == merge_playlists(b, a)`.
pub fn merge_playlists(local: &[PlaylistRecord], remote: &[PlaylistRecord]) -> Vec<PlaylistRecord> {
    let mut out: BTreeMap<&str, &PlaylistRecord> = BTreeMap::new();
    for record in local.iter().chain(remote) {
        match out.get(record.uuid.as_str()) {
            Some(current) if playlist_key(current) >= playlist_key(record) => {}
            _ => {
                out.insert(record.uuid.as_str(), record);
            }
        }
    }
    out.into_values().cloned().collect()
}

/// Merges two favorite lists by `track_key` with last writer wins (see [`favorite_key`]).
/// Pure: the result is the union of both lists, one record per key, sorted by key, and
/// `merge_favorites(a, b) == merge_favorites(b, a)`.
pub fn merge_favorites(local: &[FavoriteRecord], remote: &[FavoriteRecord]) -> Vec<FavoriteRecord> {
    let mut out: BTreeMap<&str, &FavoriteRecord> = BTreeMap::new();
    for record in local.iter().chain(remote) {
        match out.get(record.track_key.as_str()) {
            Some(current) if favorite_key(current) >= favorite_key(record) => {}
            _ => {
                out.insert(record.track_key.as_str(), record);
            }
        }
    }
    out.into_values().cloned().collect()
}

/// Rejects documents whose format is newer than this build understands.
fn check_version(file: &str, version: u32) -> Result<()> {
    if version > DOC_VERSION {
        return Err(LibraryError::Sync(format!(
            "{file}: format version {version} is newer than supported version {DOC_VERSION}"
        )));
    }
    Ok(())
}

/// Parses one remote document, mapping parse errors to [`LibraryError::Sync`].
fn parse_doc<T: DeserializeOwned>(file: &str, bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|e| LibraryError::Sync(format!("{file}: {e}")))
}

/// Serializes a document, mapping errors to [`LibraryError::Sync`].
fn encode_doc<T: Serialize>(file: &str, doc: &T) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(doc).map_err(|e| LibraryError::Sync(format!("{file}: {e}")))
}

/// Fetches and validates the remote playlists document. `None` when the object is absent.
async fn fetch_playlists(store: &dyn UserDataStore) -> Result<Option<PlaylistsDoc>> {
    let Some(bytes) = store.get(PLAYLISTS_FILE).await? else {
        return Ok(None);
    };
    let doc: PlaylistsDoc = parse_doc(PLAYLISTS_FILE, &bytes)?;
    check_version(PLAYLISTS_FILE, doc.version)?;
    Ok(Some(doc))
}

/// Fetches and validates the remote favorites document. `None` when the object is absent.
async fn fetch_favorites(store: &dyn UserDataStore) -> Result<Option<FavoritesDoc>> {
    let Some(bytes) = store.get(FAVORITES_FILE).await? else {
        return Ok(None);
    };
    let doc: FavoritesDoc = parse_doc(FAVORITES_FILE, &bytes)?;
    check_version(FAVORITES_FILE, doc.version)?;
    Ok(Some(doc))
}

/// Merges the playlists, applies what is newer locally, and pushes if the result differs
/// from the remote document. Returns `(applied, pushed)`.
async fn sync_playlists(
    library: &Library,
    store: &dyn UserDataStore,
    remote_doc: Option<PlaylistsDoc>,
) -> Result<(u32, bool)> {
    let remote: &[PlaylistRecord] = remote_doc.as_ref().map_or(&[], |d| d.playlists.as_slice());
    let local = library.playlist_records()?;
    let merged = merge_playlists(&local, remote);
    let applied = library.apply_playlist_records(&merged)?;
    let push = match &remote_doc {
        None => !merged.is_empty(),
        Some(_) => {
            let mut sorted = remote.to_vec();
            sorted.sort_by(|a, b| a.uuid.cmp(&b.uuid));
            sorted != merged
        }
    };
    if push {
        let bytes = encode_doc(
            PLAYLISTS_FILE,
            &PlaylistsDoc {
                version: DOC_VERSION,
                playlists: merged,
            },
        )?;
        store.put(PLAYLISTS_FILE, bytes).await?;
    }
    Ok((applied, push))
}

/// Favorites counterpart of [`sync_playlists`]. Returns `(applied, pushed)`.
async fn sync_favorites(
    library: &Library,
    store: &dyn UserDataStore,
    remote_doc: Option<FavoritesDoc>,
) -> Result<(u32, bool)> {
    let remote: &[FavoriteRecord] = remote_doc.as_ref().map_or(&[], |d| d.favorites.as_slice());
    let local = library.favorite_records()?;
    let merged = merge_favorites(&local, remote);
    let applied = library.apply_favorite_records(&merged)?;
    let push = match &remote_doc {
        None => !merged.is_empty(),
        Some(_) => {
            let mut sorted = remote.to_vec();
            sorted.sort_by(|a, b| a.track_key.cmp(&b.track_key));
            sorted != merged
        }
    };
    if push {
        let bytes = encode_doc(
            FAVORITES_FILE,
            &FavoritesDoc {
                version: DOC_VERSION,
                favorites: merged,
            },
        )?;
        store.put(FAVORITES_FILE, bytes).await?;
    }
    Ok((applied, push))
}

/// Syncs playlists and favorites with `store`.
///
/// Both remote documents are fetched and validated before anything is applied, so a corrupt
/// or newer remote aborts with [`LibraryError::Sync`] and changes nothing. Then, for each
/// collection, the remote and local records are merged, the newer ones are applied locally,
/// and the merged document is pushed when it differs from the remote one.
pub async fn sync_user_data(library: &Library, store: &dyn UserDataStore) -> Result<SyncReport> {
    let remote_playlists = fetch_playlists(store).await?;
    let remote_favorites = fetch_favorites(store).await?;

    let (playlists_applied, playlists_pushed) =
        sync_playlists(library, store, remote_playlists).await?;
    let (favorites_applied, favorites_pushed) =
        sync_favorites(library, store, remote_favorites).await?;

    Ok(SyncReport {
        playlists_applied,
        favorites_applied,
        playlists_pushed,
        favorites_pushed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::TempDir;
    use sonorus_storage::StorageConfig;

    fn pl(uuid: &str, name: &str, updated_at: i64) -> PlaylistRecord {
        PlaylistRecord {
            uuid: uuid.to_string(),
            name: name.to_string(),
            created_at: 1,
            updated_at,
            deleted: false,
            tracks: Vec::new(),
        }
    }

    fn fav(key: &str, favorite: bool, updated_at: i64) -> FavoriteRecord {
        FavoriteRecord {
            track_key: key.to_string(),
            favorite,
            updated_at,
        }
    }

    fn open(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).expect("open library")
    }

    #[test]
    fn merge_playlists_disjoint_is_union() {
        let a = [pl("b", "B", 1)];
        let b = [pl("a", "A", 1)];
        let merged = merge_playlists(&a, &b);
        assert_eq!(merged, vec![pl("a", "A", 1), pl("b", "B", 1)]);
    }

    #[test]
    fn merge_playlists_newer_wins_both_directions() {
        let old = pl("u", "old", 1);
        let new = pl("u", "new", 2);
        assert_eq!(
            merge_playlists(std::slice::from_ref(&old), std::slice::from_ref(&new)),
            vec![new.clone()]
        );
        assert_eq!(
            merge_playlists(std::slice::from_ref(&new), std::slice::from_ref(&old)),
            vec![new]
        );
    }

    #[test]
    fn merge_playlists_equal_timestamp_tie_break_is_symmetric() {
        let a = pl("u", "Apple", 5);
        let b = pl("u", "Banana", 5);
        let ab = merge_playlists(std::slice::from_ref(&a), std::slice::from_ref(&b));
        let ba = merge_playlists(std::slice::from_ref(&b), std::slice::from_ref(&a));
        assert_eq!(ab, ba);
        assert_eq!(ab[0].name, "Banana");

        let mut tomb = pl("u", "Apple", 5);
        tomb.deleted = true;
        let live = pl("u", "Zebra", 5);
        let ab = merge_playlists(std::slice::from_ref(&tomb), std::slice::from_ref(&live));
        let ba = merge_playlists(std::slice::from_ref(&live), std::slice::from_ref(&tomb));
        assert_eq!(ab, ba);
        assert!(ab[0].deleted);

        let mut x = pl("u", "same", 5);
        x.tracks = vec!["a".into()];
        let mut y = pl("u", "same", 5);
        y.tracks = vec!["b".into()];
        let ab = merge_playlists(std::slice::from_ref(&x), std::slice::from_ref(&y));
        let ba = merge_playlists(std::slice::from_ref(&y), std::slice::from_ref(&x));
        assert_eq!(ab, ba);
        assert_eq!(ab[0].tracks, vec!["b".to_string()]);
    }

    #[test]
    fn merge_playlists_newer_tombstone_beats_live() {
        let live = pl("u", "Name", 1);
        let mut tomb = pl("u", "Name", 2);
        tomb.deleted = true;
        let merged = merge_playlists(std::slice::from_ref(&live), std::slice::from_ref(&tomb));
        assert_eq!(merged, vec![tomb.clone()]);
        let merged = merge_playlists(std::slice::from_ref(&tomb), std::slice::from_ref(&live));
        assert_eq!(merged, vec![tomb]);
    }

    #[test]
    fn merge_playlists_output_sorted_by_uuid() {
        let local = [pl("m", "m", 1), pl("c", "c", 1)];
        let remote = [pl("x", "x", 1), pl("a", "a", 1)];
        let uuids: Vec<String> = merge_playlists(&local, &remote)
            .into_iter()
            .map(|r| r.uuid)
            .collect();
        assert_eq!(uuids, vec!["a", "c", "m", "x"]);
    }

    #[test]
    fn merge_favorites_rules() {
        let on = fav("k", true, 1);
        let off = fav("k", false, 2);
        assert_eq!(
            merge_favorites(std::slice::from_ref(&on), std::slice::from_ref(&off)),
            vec![off.clone()]
        );
        assert_eq!(
            merge_favorites(std::slice::from_ref(&off), std::slice::from_ref(&on)),
            vec![off.clone()]
        );

        let on_same = fav("k", true, 3);
        let off_same = fav("k", false, 3);
        let ab = merge_favorites(
            std::slice::from_ref(&on_same),
            std::slice::from_ref(&off_same),
        );
        let ba = merge_favorites(
            std::slice::from_ref(&off_same),
            std::slice::from_ref(&on_same),
        );
        assert_eq!(ab, ba);
        assert!(!ab[0].favorite);

        let merged = merge_favorites(&[fav("b", true, 1)], &[fav("a", true, 1)]);
        let keys: Vec<&str> = merged.iter().map(|r| r.track_key.as_str()).collect();
        assert_eq!(keys, vec!["a", "b"]);
    }

    #[test]
    fn playlists_doc_round_trip() {
        let mut tomb = pl("t", "gone", 9);
        tomb.deleted = true;
        let mut live = pl("l", "live", 7);
        live.tracks = vec!["a/b.flac".into(), "c/d.flac".into()];
        let doc = PlaylistsDoc {
            version: DOC_VERSION,
            playlists: vec![live, tomb],
        };
        let bytes = serde_json::to_vec_pretty(&doc).expect("encode");
        let back: PlaylistsDoc = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(back, doc);
    }

    #[test]
    fn favorites_doc_round_trip() {
        let doc = FavoritesDoc {
            version: DOC_VERSION,
            favorites: vec![fav("a/b.flac", true, 4), fav("c.flac", false, 5)],
        };
        let bytes = serde_json::to_vec(&doc).expect("encode");
        let back: FavoritesDoc = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(back, doc);
    }

    #[test]
    fn docs_default_missing_fields() {
        let json =
            br#"{"version":1,"playlists":[{"uuid":"u","name":"n","created_at":1,"updated_at":2}]}"#;
        let doc: PlaylistsDoc = serde_json::from_slice(json).expect("decode");
        assert_eq!(doc.playlists.len(), 1);
        assert!(!doc.playlists[0].deleted);
        assert!(doc.playlists[0].tracks.is_empty());
    }

    #[test]
    fn object_key_joins_prefix_and_file() {
        assert_eq!(object_key("", PLAYLISTS_FILE), "_sonorus/playlists.json");
        assert_eq!(
            object_key("music/", FAVORITES_FILE),
            "music/_sonorus/favorites.json"
        );
    }

    #[test]
    fn bucket_store_normalizes_prefix() {
        let config = StorageConfig::backblaze_b2("us-west-004", "music", "key", "secret");
        let storage = Storage::new(&config).expect("storage");
        let store = BucketStore::new(storage, " /music/ ");
        assert_eq!(store.prefix, "music/");
    }

    #[tokio::test]
    async fn sync_empty_remote_pushes_local() {
        let dir = TempDir::new();
        let lib = open(&dir);
        let playlist = lib.create_playlist("Mix").expect("create");
        lib.add_to_playlist(playlist.id, &["x/a.flac".to_string()])
            .expect("add");
        lib.set_favorite("x/a.flac", true).expect("fav");
        let store = MemoryStore::new();

        let report = sync_user_data(&lib, &store).await.expect("sync");
        assert_eq!(report.playlists_applied, 0);
        assert_eq!(report.favorites_applied, 0);
        assert!(report.playlists_pushed);
        assert!(report.favorites_pushed);

        let raw = store.get_now(PLAYLISTS_FILE).expect("playlists doc");
        let doc: PlaylistsDoc = serde_json::from_slice(&raw).expect("decode");
        assert_eq!(doc.version, DOC_VERSION);
        assert_eq!(doc.playlists, lib.playlist_records().expect("records"));
        let raw = store.get_now(FAVORITES_FILE).expect("favorites doc");
        let doc: FavoritesDoc = serde_json::from_slice(&raw).expect("decode");
        assert_eq!(doc.favorites, lib.favorite_records().expect("records"));
    }

    #[tokio::test]
    async fn sync_twice_second_is_noop() {
        let dir = TempDir::new();
        let lib = open(&dir);
        lib.create_playlist("Mix").expect("create");
        lib.set_favorite("x/a.flac", true).expect("fav");
        let store = MemoryStore::new();
        sync_user_data(&lib, &store).await.expect("first sync");

        let report = sync_user_data(&lib, &store).await.expect("second sync");
        assert_eq!(report.playlists_applied, 0);
        assert_eq!(report.favorites_applied, 0);
        assert!(!report.playlists_pushed);
        assert!(!report.favorites_pushed);
    }

    #[tokio::test]
    async fn sync_between_two_devices_propagates_edits_and_deletes() {
        let dir_a = TempDir::new();
        let dir_b = TempDir::new();
        let a = open(&dir_a);
        let b = open(&dir_b);
        let store = MemoryStore::new();

        let created = a.create_playlist("Mix").expect("create");
        a.set_favorite("x/a.flac", true).expect("fav");
        sync_user_data(&a, &store).await.expect("A sync");

        let report = sync_user_data(&b, &store).await.expect("B sync");
        assert!(report.playlists_applied >= 1);
        assert!(report.favorites_applied >= 1);
        let b_playlists = b.playlists().expect("playlists");
        assert_eq!(b_playlists.len(), 1);
        assert_eq!(b_playlists[0].uuid, created.uuid);
        assert!(b.is_favorite("x/a.flac").expect("is_favorite"));

        b.rename_playlist(b_playlists[0].id, "Renamed")
            .expect("rename");
        b.set_favorite("x/a.flac", false).expect("unfav");
        sync_user_data(&b, &store).await.expect("B sync 2");

        let report = sync_user_data(&a, &store).await.expect("A sync 2");
        assert!(report.playlists_applied >= 1);
        assert!(report.favorites_applied >= 1);
        let a_playlists = a.playlists().expect("playlists");
        assert_eq!(a_playlists.len(), 1);
        assert_eq!(a_playlists[0].name, "Renamed");
        assert!(!a.is_favorite("x/a.flac").expect("is_favorite"));
    }

    #[tokio::test]
    async fn sync_propagates_delete() {
        let dir_a = TempDir::new();
        let dir_b = TempDir::new();
        let a = open(&dir_a);
        let b = open(&dir_b);
        let store = MemoryStore::new();

        let created = a.create_playlist("Doomed").expect("create");
        sync_user_data(&a, &store).await.expect("A sync");
        sync_user_data(&b, &store).await.expect("B sync");
        assert_eq!(b.playlists().expect("playlists").len(), 1);

        a.delete_playlist(created.id).expect("delete");
        sync_user_data(&a, &store).await.expect("A sync 2");
        sync_user_data(&b, &store).await.expect("B sync 2");
        assert!(b.playlists().expect("playlists").is_empty());
    }

    #[tokio::test]
    async fn sync_corrupt_remote_is_error_and_untouched() {
        let dir = TempDir::new();
        let lib = open(&dir);
        lib.create_playlist("Local").expect("create");
        let store = MemoryStore::new();
        store.put_now(PLAYLISTS_FILE, b"{not json".to_vec());

        let err = sync_user_data(&lib, &store)
            .await
            .expect_err("corrupt remote must fail");
        assert!(matches!(err, LibraryError::Sync(_)), "got {err:?}");
        assert_eq!(store.get_now(PLAYLISTS_FILE).expect("bytes"), b"{not json");
        assert!(store.get_now(FAVORITES_FILE).is_none());
        let names: Vec<String> = lib
            .playlists()
            .expect("playlists")
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["Local".to_string()]);
    }

    #[tokio::test]
    async fn sync_newer_remote_version_is_error_and_not_overwritten() {
        let dir = TempDir::new();
        let lib = open(&dir);
        lib.create_playlist("Local").expect("create");
        let store = MemoryStore::new();
        let newer = br#"{"version":2,"playlists":[{"uuid":"zz","name":"Future","created_at":1,"updated_at":99999999999999,"deleted":false,"tracks":[]}]}"#;
        store.put_now(PLAYLISTS_FILE, newer.to_vec());

        let err = sync_user_data(&lib, &store)
            .await
            .expect_err("newer remote must fail");
        assert!(matches!(err, LibraryError::Sync(_)), "got {err:?}");
        assert_eq!(
            store.get_now(PLAYLISTS_FILE).expect("bytes"),
            newer.to_vec()
        );
        let names: Vec<String> = lib
            .playlists()
            .expect("playlists")
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["Local".to_string()]);
    }

    #[tokio::test]
    async fn equal_timestamps_converge_across_devices() {
        let (da, db) = (TempDir::new(), TempDir::new());
        let (a, b) = (open(&da), open(&db));
        let store = MemoryStore::new();
        // Same uuid and timestamp, different content on each device.
        a.apply_playlist_records(&[pl("u1", "Apple", 50)]).unwrap();
        b.apply_playlist_records(&[pl("u1", "Zebra", 50)]).unwrap();
        a.apply_favorite_records(&[fav("k", true, 70)]).unwrap();
        b.apply_favorite_records(&[fav("k", false, 70)]).unwrap();

        sync_user_data(&a, &store).await.unwrap();
        sync_user_data(&b, &store).await.unwrap();
        sync_user_data(&a, &store).await.unwrap();

        assert_eq!(a.playlist_records().unwrap(), b.playlist_records().unwrap());
        assert_eq!(
            a.playlists().unwrap()[0].name,
            "Zebra",
            "greater name wins the tie"
        );
        assert_eq!(a.favorite_records().unwrap(), b.favorite_records().unwrap());
        assert!(!a.is_favorite("k").unwrap(), "unfavorite wins the tie");

        // Steady state: nothing left to push.
        let r = sync_user_data(&b, &store).await.unwrap();
        assert!(!r.playlists_pushed && !r.favorites_pushed);
    }
}
