//! User playlists: CRUD, ordered entries, and the sync form (`PlaylistRecord`) with
//! last-writer-wins merge. Entries are stored as object keys, so they survive rescans.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::db::{Library, next_stamp, now_ms};
use crate::error::{LibraryError, Result};
use crate::model::{Playlist, PlaylistEntry, PlaylistRecord};
use crate::query::{TRACK_SELECT, row_to_track};
use crate::sync::playlist_key;

/// Name used when a playlist is created or renamed with a blank name.
const DEFAULT_NAME: &str = "New Playlist";

/// Playlist columns plus aggregates over the catalogued items. Aggregates are computed in SQL
/// per row, so listing playlists takes one query.
const PLAYLIST_SELECT: &str = "SELECT p.id, p.uuid, p.name, p.created_at, p.updated_at,
        (SELECT COUNT(*) FROM playlist_items pi WHERE pi.playlist_id = p.id),
        (SELECT COALESCE(SUM(t.duration_ms), 0) FROM playlist_items pi
            JOIN tracks t ON t.key = pi.track_key WHERE pi.playlist_id = p.id),
        (SELECT t.cover_id FROM playlist_items pi JOIN tracks t ON t.key = pi.track_key
            WHERE pi.playlist_id = p.id AND t.cover_id IS NOT NULL
            ORDER BY pi.position LIMIT 1)
    FROM playlists p";

fn row_to_playlist(row: &Row<'_>) -> rusqlite::Result<Playlist> {
    let track_count: i64 = row.get(5)?;
    let duration_ms: i64 = row.get(6)?;
    Ok(Playlist {
        id: row.get(0)?,
        uuid: row.get(1)?,
        name: row.get(2)?,
        track_count: u32::try_from(track_count).unwrap_or(u32::MAX),
        duration_ms: u64::try_from(duration_ms).unwrap_or(0),
        cover_id: row.get(7)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn not_found(id: i64) -> LibraryError {
    LibraryError::NotFound(format!("playlist {id}"))
}

/// Trims a playlist name, substituting the default for a blank one.
fn clean_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        DEFAULT_NAME.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `updated_at` of a live (existing, not deleted) playlist; NotFound otherwise.
fn live_updated_at(conn: &Connection, id: i64) -> Result<i64> {
    conn.query_row(
        "SELECT updated_at FROM playlists WHERE id = ?1 AND deleted = 0",
        params![id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| not_found(id))
}

/// Reads a live playlist with its aggregates; NotFound otherwise.
fn live_playlist(conn: &Connection, id: i64) -> Result<Playlist> {
    conn.query_row(
        &format!("{PLAYLIST_SELECT} WHERE p.id = ?1 AND p.deleted = 0"),
        params![id],
        row_to_playlist,
    )
    .optional()?
    .ok_or_else(|| not_found(id))
}

/// The track keys of a playlist in position order.
fn load_keys(conn: &Connection, id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT track_key FROM playlist_items WHERE playlist_id = ?1 ORDER BY position",
    )?;
    let keys = stmt
        .query_map(params![id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(keys)
}

/// Replaces a playlist's whole item list with `keys`, positions 0..n.
fn store_keys(conn: &Connection, id: i64, keys: &[String]) -> Result<()> {
    conn.execute(
        "DELETE FROM playlist_items WHERE playlist_id = ?1",
        params![id],
    )?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO playlist_items (playlist_id, position, track_key) VALUES (?1, ?2, ?3)",
    )?;
    for (pos, key) in keys.iter().enumerate() {
        let pos = i64::try_from(pos).unwrap_or(i64::MAX);
        stmt.execute(params![id, pos, key])?;
    }
    Ok(())
}

/// Bumps `updated_at` of a playlist last stamped `prev`.
fn touch(conn: &Connection, id: i64, prev: i64) -> Result<()> {
    conn.execute(
        "UPDATE playlists SET updated_at = ?2 WHERE id = ?1",
        params![id, next_stamp(prev)],
    )?;
    Ok(())
}

impl Library {
    /// Creates an empty playlist with a random uuid. A blank name becomes "New Playlist".
    pub fn create_playlist(&self, name: &str) -> Result<Playlist> {
        let uuid = format!("{:016x}{:016x}", fastrand::u64(..), fastrand::u64(..));
        let name = clean_name(name);
        let now = now_ms();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO playlists (uuid, name, created_at, updated_at, deleted)
             VALUES (?1, ?2, ?3, ?3, 0)",
            params![uuid, name, now],
        )?;
        let id = tx.last_insert_rowid();
        let playlist = live_playlist(&tx, id)?;
        tx.commit()?;
        Ok(playlist)
    }

    /// Renames a live playlist. A blank name becomes "New Playlist".
    pub fn rename_playlist(&self, id: i64, name: &str) -> Result<()> {
        let name = clean_name(name);
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let prev = live_updated_at(&tx, id)?;
        tx.execute(
            "UPDATE playlists SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        touch(&tx, id, prev)?;
        tx.commit()?;
        Ok(())
    }

    /// Deletes a playlist by tombstoning it (`deleted = 1`) so the deletion can sync. Its items
    /// are removed. NotFound if it is missing or already deleted.
    pub fn delete_playlist(&self, id: i64) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let prev = live_updated_at(&tx, id)?;
        store_keys(&tx, id, &[])?;
        tx.execute(
            "UPDATE playlists SET deleted = 1 WHERE id = ?1",
            params![id],
        )?;
        touch(&tx, id, prev)?;
        tx.commit()?;
        Ok(())
    }

    /// All live playlists, sorted by name (case-insensitive) then id.
    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{PLAYLIST_SELECT} WHERE p.deleted = 0 ORDER BY p.name COLLATE NOCASE, p.id"
        ))?;
        let rows = stmt
            .query_map([], row_to_playlist)?
            .collect::<rusqlite::Result<Vec<Playlist>>>()?;
        Ok(rows)
    }

    /// A live playlist by id. NotFound if it is missing or deleted.
    pub fn playlist(&self, id: i64) -> Result<Playlist> {
        let conn = self.lock();
        live_playlist(&conn, id)
    }

    /// The entries of a live playlist in position order. `track` is `None` when the key is not
    /// in the catalogue. NotFound if the playlist is missing or deleted.
    pub fn playlist_entries(&self, id: i64) -> Result<Vec<PlaylistEntry>> {
        let conn = self.lock();
        live_updated_at(&conn, id)?;
        let keys = load_keys(&conn, id)?;
        let mut stmt = conn.prepare_cached(&format!("{TRACK_SELECT} WHERE t.key = ?1"))?;
        let mut entries = Vec::with_capacity(keys.len());
        for (idx, key) in keys.into_iter().enumerate() {
            let track = stmt.query_row(params![key], row_to_track).optional()?;
            entries.push(PlaylistEntry {
                position: u32::try_from(idx).unwrap_or(u32::MAX),
                track_key: key,
                track,
            });
        }
        Ok(entries)
    }

    /// Appends track keys to a live playlist; duplicates are allowed. An empty slice changes
    /// nothing. NotFound if the playlist is missing or deleted.
    pub fn add_to_playlist(&self, id: i64, keys: &[String]) -> Result<()> {
        self.edit_items(id, |items| {
            if keys.is_empty() {
                return Ok(false);
            }
            items.extend_from_slice(keys);
            Ok(true)
        })
    }

    /// Removes the entry at `position`. NotFound if the playlist is missing or deleted, or the
    /// position is out of range.
    pub fn remove_from_playlist(&self, id: i64, position: u32) -> Result<()> {
        self.edit_items(id, |items| {
            let idx = position as usize;
            if idx >= items.len() {
                return Err(not_found(id));
            }
            items.remove(idx);
            Ok(true)
        })
    }

    /// Moves the entry at `from` so it ends up at index `to` (`Vec::remove` then `Vec::insert`).
    /// NotFound if `from` is out of range, `to` is not less than the length, or the playlist is
    /// missing or deleted. `from == to` changes nothing.
    pub fn move_in_playlist(&self, id: i64, from: u32, to: u32) -> Result<()> {
        self.edit_items(id, |items| {
            let (from, to) = (from as usize, to as usize);
            if from >= items.len() || to >= items.len() {
                return Err(not_found(id));
            }
            if from == to {
                return Ok(false);
            }
            let key = items.remove(from);
            items.insert(to, key);
            Ok(true)
        })
    }

    /// Every playlist, including tombstones, sorted by uuid. Tombstones carry no tracks.
    pub fn playlist_records(&self) -> Result<Vec<PlaylistRecord>> {
        let conn = self.lock();

        let mut by_playlist: HashMap<i64, Vec<String>> = HashMap::new();
        let mut items = conn.prepare(
            "SELECT playlist_id, track_key FROM playlist_items ORDER BY playlist_id, position",
        )?;
        let pairs = items.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for pair in pairs {
            let (playlist_id, key) = pair?;
            by_playlist.entry(playlist_id).or_default().push(key);
        }

        let mut stmt = conn.prepare(
            "SELECT id, uuid, name, created_at, updated_at, deleted FROM playlists ORDER BY uuid",
        )?;
        let records = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let deleted: bool = row.get(5)?;
                Ok((
                    id,
                    PlaylistRecord {
                        uuid: row.get(1)?,
                        name: row.get(2)?,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                        deleted,
                        tracks: Vec::new(),
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<(i64, PlaylistRecord)>>>()?;

        Ok(records
            .into_iter()
            .map(|(id, mut record)| {
                if !record.deleted {
                    record.tracks = by_playlist.remove(&id).unwrap_or_default();
                }
                record
            })
            .collect())
    }

    /// Merges incoming playlist records by uuid, last writer wins on `updated_at`. Unknown uuids
    /// are inserted (tombstones too). A known playlist is overwritten only when the incoming
    /// record wins under the same rule as [`crate::sync::merge_playlists`] (greater
    /// `updated_at`, then the deterministic tie-break); the incoming timestamp is stored
    /// verbatim. Returns the
    /// number of playlists inserted or overwritten. All changes happen in one transaction.
    pub fn apply_playlist_records(&self, records: &[PlaylistRecord]) -> Result<u32> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let mut changed = 0u32;
        for record in records {
            let items: &[String] = if record.deleted { &[] } else { &record.tracks };
            let local: Option<(i64, PlaylistRecord)> = tx
                .query_row(
                    "SELECT id, name, created_at, updated_at, deleted FROM playlists
                     WHERE uuid = ?1",
                    params![record.uuid],
                    |r| {
                        Ok((
                            r.get(0)?,
                            PlaylistRecord {
                                uuid: record.uuid.clone(),
                                name: r.get(1)?,
                                created_at: r.get(2)?,
                                updated_at: r.get(3)?,
                                deleted: r.get(4)?,
                                tracks: Vec::new(),
                            },
                        ))
                    },
                )
                .optional()?;
            let local = match local {
                Some((id, mut rec)) => {
                    if !rec.deleted && rec.updated_at == record.updated_at {
                        // Only needed for the equal-timestamp tie-break.
                        rec.tracks = load_keys(&tx, id)?;
                    }
                    Some((id, rec))
                }
                None => None,
            };
            match local {
                None => {
                    tx.execute(
                        "INSERT INTO playlists (uuid, name, created_at, updated_at, deleted)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            record.uuid,
                            record.name,
                            record.created_at,
                            record.updated_at,
                            record.deleted
                        ],
                    )?;
                    let id = tx.last_insert_rowid();
                    store_keys(&tx, id, items)?;
                    changed += 1;
                }
                // Same winner rule as `merge_playlists` (newer, then the deterministic
                // tie-break), so devices converge even when timestamps are equal.
                Some((id, local)) if playlist_key(record) > playlist_key(&local) => {
                    tx.execute(
                        "UPDATE playlists SET name = ?2, deleted = ?3, updated_at = ?4
                         WHERE id = ?1",
                        params![id, record.name, record.deleted, record.updated_at],
                    )?;
                    store_keys(&tx, id, items)?;
                    changed += 1;
                }
                Some(_) => {}
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Applies `edit` to a live playlist's item list inside one transaction. `edit` returns
    /// whether it changed the list; only then are items rewritten and `updated_at` bumped.
    fn edit_items<F>(&self, id: i64, edit: F) -> Result<()>
    where
        F: FnOnce(&mut Vec<String>) -> Result<bool>,
    {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let prev = live_updated_at(&tx, id)?;
        let mut items = load_keys(&tx, id)?;
        if edit(&mut items)? {
            store_keys(&tx, id, &items)?;
            touch(&tx, id, prev)?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{TempDir, meta, stamp};
    use crate::model::TrackMeta;

    const KEY_A: &str = "Artist/Album/01 - Alpha.flac";
    const KEY_B: &str = "Artist/Album/02 - Beta.flac";

    fn library(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).unwrap()
    }

    fn keys(lib: &Library, id: i64) -> Vec<String> {
        lib.playlist_entries(id)
            .unwrap()
            .into_iter()
            .map(|e| e.track_key)
            .collect()
    }

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn record(
        uuid: &str,
        name: &str,
        updated_at: i64,
        deleted: bool,
        tracks: &[&str],
    ) -> PlaylistRecord {
        PlaylistRecord {
            uuid: uuid.to_owned(),
            name: name.to_owned(),
            created_at: 1_000,
            updated_at,
            deleted,
            tracks: strs(tracks),
        }
    }

    fn is_not_found<T: std::fmt::Debug>(r: Result<T>) -> bool {
        matches!(r, Err(LibraryError::NotFound(_)))
    }

    #[test]
    fn create_generates_uuid_and_default_name() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let a = lib.create_playlist("  Road Trip  ").unwrap();
        assert_eq!(a.name, "Road Trip");
        assert_eq!(a.uuid.len(), 32);
        assert!(a.uuid.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a.created_at, a.updated_at);
        assert_eq!(a.track_count, 0);
        assert_eq!(a.duration_ms, 0);
        assert_eq!(a.cover_id, None);

        let blank = lib.create_playlist("  ").unwrap();
        assert_eq!(blank.name, "New Playlist");
        let other = lib.create_playlist("").unwrap();
        assert_ne!(blank.uuid, other.uuid);
        assert_eq!(lib.playlist(blank.id).unwrap(), blank);
    }

    #[test]
    fn rename_trims_and_defaults() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let p = lib.create_playlist("Old").unwrap();
        lib.rename_playlist(p.id, "  New name ").unwrap();
        assert_eq!(lib.playlist(p.id).unwrap().name, "New name");
        lib.rename_playlist(p.id, "   ").unwrap();
        assert_eq!(lib.playlist(p.id).unwrap().name, "New Playlist");
        assert!(is_not_found(lib.rename_playlist(999, "x")));
    }

    #[test]
    fn list_is_sorted_by_name_nocase_then_id() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let b = lib.create_playlist("beta").unwrap();
        let a = lib.create_playlist("Alpha").unwrap();
        let s1 = lib.create_playlist("same").unwrap();
        let s2 = lib.create_playlist("SAME").unwrap();
        let ids: Vec<i64> = lib.playlists().unwrap().iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![a.id, b.id, s1.id, s2.id]);
    }

    #[test]
    fn add_remove_move_produce_exact_order() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let p = lib.create_playlist("P").unwrap();

        lib.add_to_playlist(p.id, &strs(&["k1", "k2", "k3"]))
            .unwrap();
        lib.add_to_playlist(p.id, &strs(&["k2"])).unwrap();
        assert_eq!(keys(&lib, p.id), strs(&["k1", "k2", "k3", "k2"]));
        lib.add_to_playlist(p.id, &[]).unwrap();
        assert_eq!(keys(&lib, p.id).len(), 4);

        lib.remove_from_playlist(p.id, 1).unwrap();
        assert_eq!(keys(&lib, p.id), strs(&["k1", "k3", "k2"]));
        assert!(is_not_found(lib.remove_from_playlist(p.id, 3)));
        assert!(is_not_found(lib.remove_from_playlist(p.id, u32::MAX)));

        // Move first to last: remove(0) -> [k3, k2], insert(2, k1) -> [k3, k2, k1].
        lib.move_in_playlist(p.id, 0, 2).unwrap();
        assert_eq!(keys(&lib, p.id), strs(&["k3", "k2", "k1"]));
        // Move last to first.
        lib.move_in_playlist(p.id, 2, 0).unwrap();
        assert_eq!(keys(&lib, p.id), strs(&["k1", "k3", "k2"]));
        assert!(is_not_found(lib.move_in_playlist(p.id, 3, 0)));
        assert!(is_not_found(lib.move_in_playlist(p.id, 0, 3)));
        assert!(is_not_found(lib.move_in_playlist(p.id, 0, 99)));
        assert_eq!(keys(&lib, p.id), strs(&["k1", "k3", "k2"]));
    }

    #[test]
    fn move_to_same_index_is_noop_without_bump() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let p = lib.create_playlist("P").unwrap();
        lib.add_to_playlist(p.id, &strs(&["a", "b"])).unwrap();
        let before = lib.playlist(p.id).unwrap();
        lib.move_in_playlist(p.id, 1, 1).unwrap();
        assert_eq!(lib.playlist(p.id).unwrap(), before);
        assert_eq!(keys(&lib, p.id), strs(&["a", "b"]));
    }

    #[test]
    fn empty_add_does_not_bump_updated_at() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let p = lib.create_playlist("P").unwrap();
        let before = lib.playlist(p.id).unwrap();
        lib.add_to_playlist(p.id, &[]).unwrap();
        assert_eq!(lib.playlist(p.id).unwrap(), before);
    }

    #[test]
    fn entries_resolve_catalogued_tracks() {
        let dir = TempDir::new();
        let lib = library(&dir);
        lib.upsert_track(&stamp(KEY_A, 10), &meta("Alpha", "Artist", "Album"), None)
            .unwrap();
        let p = lib.create_playlist("P").unwrap();
        lib.add_to_playlist(p.id, &strs(&[KEY_A, "missing/key.flac"]))
            .unwrap();

        let entries = lib.playlist_entries(p.id).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].position, 0);
        assert_eq!(entries[0].track_key, KEY_A);
        assert_eq!(entries[0].track.as_ref().unwrap().title, "Alpha");
        assert_eq!(entries[1].position, 1);
        assert_eq!(entries[1].track_key, "missing/key.flac");
        assert!(entries[1].track.is_none());
    }

    #[test]
    fn aggregates_track_count_duration_and_cover() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let cover1 = lib.store_cover(b"one", "image/jpeg").unwrap();
        let cover2 = lib.store_cover(b"two", "image/png").unwrap();
        lib.upsert_track(
            &stamp(KEY_A, 10),
            &TrackMeta {
                duration_ms: Some(1500),
                ..meta("Alpha", "Artist", "Album")
            },
            Some(cover1),
        )
        .unwrap();
        lib.upsert_track(
            &stamp(KEY_B, 20),
            &TrackMeta {
                duration_ms: Some(500),
                ..meta("Beta", "Artist", "Album")
            },
            None,
        )
        .unwrap();
        lib.upsert_track(
            &stamp("Artist/Other/01 - Gamma.flac", 30),
            &TrackMeta {
                duration_ms: Some(2000),
                ..meta("Gamma", "Artist", "Other")
            },
            Some(cover2),
        )
        .unwrap();

        let p = lib.create_playlist("P").unwrap();
        // Beta (no cover) comes first, then Alpha (cover1), missing, Gamma (cover2).
        lib.add_to_playlist(
            p.id,
            &strs(&[KEY_B, KEY_A, "missing.flac", "Artist/Other/01 - Gamma.flac"]),
        )
        .unwrap();
        let pl = lib.playlist(p.id).unwrap();
        assert_eq!(pl.track_count, 4);
        assert_eq!(pl.duration_ms, 1500 + 500 + 2000);
        assert_eq!(pl.cover_id, Some(cover1));

        let listed = lib.playlists().unwrap();
        assert_eq!(listed, vec![pl]);

        let empty = lib.create_playlist("Only missing").unwrap();
        lib.add_to_playlist(empty.id, &strs(&["gone.flac"]))
            .unwrap();
        let e = lib.playlist(empty.id).unwrap();
        assert_eq!(e.track_count, 1);
        assert_eq!(e.duration_ms, 0);
        assert_eq!(e.cover_id, None);
    }

    #[test]
    fn delete_tombstones_playlist() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let p = lib.create_playlist("Gone").unwrap();
        lib.add_to_playlist(p.id, &strs(&["a", "b"])).unwrap();
        lib.delete_playlist(p.id).unwrap();

        assert!(is_not_found(lib.playlist(p.id).map(|_| ())));
        assert!(is_not_found(lib.playlist_entries(p.id).map(|_| ())));
        assert!(lib.playlists().unwrap().is_empty());

        let records = lib.playlist_records().unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].deleted);
        assert!(records[0].tracks.is_empty());
        assert_eq!(records[0].name, "Gone");
        assert!(records[0].updated_at > p.updated_at);
    }

    #[test]
    fn mutating_deleted_or_missing_playlist_is_not_found() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let p = lib.create_playlist("Gone").unwrap();
        lib.delete_playlist(p.id).unwrap();

        assert!(is_not_found(lib.delete_playlist(p.id)));
        assert!(is_not_found(lib.rename_playlist(p.id, "x")));
        assert!(is_not_found(lib.add_to_playlist(p.id, &strs(&["a"]))));
        assert!(is_not_found(lib.remove_from_playlist(p.id, 0)));
        assert!(is_not_found(lib.move_in_playlist(p.id, 0, 0)));
        assert!(is_not_found(lib.delete_playlist(12345)));
        assert!(is_not_found(lib.add_to_playlist(12345, &strs(&["a"]))));
    }

    #[test]
    fn updated_at_strictly_increases() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let p = lib.create_playlist("P").unwrap();
        lib.add_to_playlist(p.id, &strs(&["a"])).unwrap();
        let t1 = lib.playlist(p.id).unwrap().updated_at;
        assert!(t1 > p.updated_at);
        lib.rename_playlist(p.id, "Q").unwrap();
        let t2 = lib.playlist(p.id).unwrap().updated_at;
        assert!(t2 > t1);
        lib.remove_from_playlist(p.id, 0).unwrap();
        assert!(lib.playlist(p.id).unwrap().updated_at > t2);
    }

    #[test]
    fn apply_inserts_unknown_playlists() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let incoming = vec![
            record("bb", "Live", 5_000, false, &["x", "y"]),
            record("aa", "Dead", 6_000, true, &["z"]),
        ];
        assert_eq!(lib.apply_playlist_records(&incoming).unwrap(), 2);

        let p = lib.playlists().unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].uuid, "bb");
        assert_eq!(p[0].updated_at, 5_000);
        assert_eq!(p[0].created_at, 1_000);
        assert_eq!(keys(&lib, p[0].id), strs(&["x", "y"]));

        let records = lib.playlist_records().unwrap();
        assert_eq!(records.len(), 2);
        assert!(records[0].deleted);
        assert!(records[0].tracks.is_empty());
    }

    #[test]
    fn apply_overwrites_only_when_incoming_wins() {
        let dir = TempDir::new();
        let lib = library(&dir);
        lib.apply_playlist_records(&[record("u1", "Base", 5_000, false, &["a"])])
            .unwrap();

        // Older: skipped.
        let older = [record("u1", "Older", 4_999, false, &["b"])];
        assert_eq!(lib.apply_playlist_records(&older).unwrap(), 0);
        // Equal timestamp, loses the tie-break ("Aaa" < "Base"): skipped.
        let equal = [record("u1", "Aaa", 5_000, false, &["c"])];
        assert_eq!(lib.apply_playlist_records(&equal).unwrap(), 0);
        let before = lib.playlist_records().unwrap();
        assert_eq!(before[0].name, "Base");
        assert_eq!(before[0].tracks, strs(&["a"]));
        // Identical record: skipped.
        let same = [record("u1", "Base", 5_000, false, &["a"])];
        assert_eq!(lib.apply_playlist_records(&same).unwrap(), 0);
        // Equal timestamp, wins the tie-break ("Base" < "Bz"): overwritten, as merge would.
        let tie_win = [record("u1", "Bz", 5_000, false, &["c"])];
        assert_eq!(lib.apply_playlist_records(&tie_win).unwrap(), 1);
        assert_eq!(lib.playlist_records().unwrap()[0].name, "Bz");

        // Newer: overwritten, with the incoming timestamp kept verbatim.
        let newer = [record("u1", "Newer", 7_777, false, &["d", "e"])];
        assert_eq!(lib.apply_playlist_records(&newer).unwrap(), 1);
        let after = lib.playlist_records().unwrap();
        assert_eq!(after[0].name, "Newer");
        assert_eq!(after[0].updated_at, 7_777);
        assert_eq!(after[0].tracks, strs(&["d", "e"]));
    }

    #[test]
    fn newer_tombstone_deletes_local_playlist() {
        let dir = TempDir::new();
        let lib = library(&dir);
        lib.apply_playlist_records(&[record("u1", "Local", 5_000, false, &["a"])])
            .unwrap();
        let id = lib.playlists().unwrap()[0].id;

        let tomb = [record("u1", "Local", 6_000, true, &[])];
        assert_eq!(lib.apply_playlist_records(&tomb).unwrap(), 1);
        assert!(is_not_found(lib.playlist(id).map(|_| ())));
        assert!(lib.playlists().unwrap().is_empty());
        assert!(lib.playlist_records().unwrap()[0].deleted);
    }

    #[test]
    fn applying_same_records_twice_changes_nothing_second_time() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let incoming = vec![
            record("u1", "One", 5_000, false, &["a", "b"]),
            record("u2", "Two", 5_500, true, &[]),
        ];
        assert_eq!(lib.apply_playlist_records(&incoming).unwrap(), 2);
        assert_eq!(lib.apply_playlist_records(&incoming).unwrap(), 0);
    }

    #[test]
    fn records_round_trip_into_fresh_library() {
        let src_dir = TempDir::new();
        let src = library(&src_dir);
        src.upsert_track(&stamp(KEY_A, 10), &meta("Alpha", "Artist", "Album"), None)
            .unwrap();
        let p1 = src.create_playlist("First").unwrap();
        src.add_to_playlist(p1.id, &strs(&[KEY_A, KEY_B, KEY_A]))
            .unwrap();
        src.move_in_playlist(p1.id, 2, 0).unwrap();
        let p2 = src.create_playlist("Second").unwrap();
        src.delete_playlist(p2.id).unwrap();
        let records = src.playlist_records().unwrap();
        assert_eq!(records.len(), 2);

        let dst_dir = TempDir::new();
        let dst = library(&dst_dir);
        assert_eq!(dst.apply_playlist_records(&records).unwrap(), 2);
        assert_eq!(dst.playlist_records().unwrap(), records);
        let dst_id = dst
            .playlists()
            .unwrap()
            .iter()
            .find(|p| p.uuid == p1.uuid)
            .unwrap()
            .id;
        assert_eq!(keys(&dst, dst_id), strs(&[KEY_A, KEY_A, KEY_B]));
    }
}
