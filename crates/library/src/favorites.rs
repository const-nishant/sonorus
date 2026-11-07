//! Favorites: per-track favorite flags keyed by object key, stored with a last-writer-wins
//! `updated_at` (Unix ms) so they can be merged with other devices through the sync documents
//! in `sync.rs`. Un-favoriting keeps the row with `favorite = false` so the removal itself
//! propagates.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, params};

use crate::db::{Library, next_stamp};
use crate::error::Result;
use crate::model::{FavoriteRecord, Track};
use crate::query::{TRACK_SELECT, row_to_track};
use crate::sync::favorite_key;

impl Library {
    /// Marks the track `key` as favorite or not. Writing the value the row already holds is a
    /// no-op and does not bump `updated_at`; any real change is stamped with `next_stamp`.
    pub fn set_favorite(&self, key: &str, favorite: bool) -> Result<()> {
        let conn = self.lock();
        let existing: Option<(i64, i64)> = conn
            .query_row(
                "SELECT favorite, updated_at FROM favorites WHERE track_key = ?1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let prev_stamp = match existing {
            Some((current, _)) if (current != 0) == favorite => return Ok(()),
            Some((_, updated_at)) => updated_at,
            None => 0,
        };
        conn.execute(
            "INSERT INTO favorites (track_key, favorite, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(track_key) DO UPDATE SET
                 favorite = excluded.favorite,
                 updated_at = excluded.updated_at",
            params![key, i64::from(favorite), next_stamp(prev_stamp)],
        )?;
        Ok(())
    }

    /// Whether the track `key` is currently a favorite (unknown keys are not).
    pub fn is_favorite(&self, key: &str) -> Result<bool> {
        let conn = self.lock();
        let found: Option<i64> = conn
            .query_row(
                "SELECT favorite FROM favorites WHERE track_key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found.is_some_and(|v| v != 0))
    }

    /// Object keys of all favorites, including ones not (yet) in the catalogue.
    pub fn favorite_keys(&self) -> Result<HashSet<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT track_key FROM favorites WHERE favorite = 1")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let out = rows.collect::<rusqlite::Result<HashSet<_>>>()?;
        Ok(out)
    }

    /// Favorited tracks that are in the catalogue, most recently favorited first.
    pub fn favorites(&self) -> Result<Vec<Track>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{TRACK_SELECT}
             JOIN favorites f ON f.track_key = t.key
             WHERE f.favorite = 1
             ORDER BY f.updated_at DESC, f.rowid DESC"
        ))?;
        let rows = stmt.query_map([], row_to_track)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Every favorites row, including `favorite = false` tombstones, sorted by `track_key`.
    pub fn favorite_records(&self) -> Result<Vec<FavoriteRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT track_key, favorite, updated_at FROM favorites ORDER BY track_key")?;
        let rows = stmt.query_map([], |r| {
            let favorite: i64 = r.get(1)?;
            Ok(FavoriteRecord {
                track_key: r.get(0)?,
                favorite: favorite != 0,
                updated_at: r.get(2)?,
            })
        })?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Merges remote favorite records in one transaction. A record for an unknown key is
    /// inserted; for a known key it overwrites `favorite` and `updated_at` (verbatim, never
    /// re-stamped) only when it wins under the [`crate::sync::merge_favorites`] rule (greater
    /// `updated_at`, then `favorite = false` on a tie). Returns
    /// the number of rows inserted or changed.
    pub fn apply_favorite_records(&self, records: &[FavoriteRecord]) -> Result<u32> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let mut changed: u32 = 0;
        for record in records {
            let local: Option<FavoriteRecord> = tx
                .query_row(
                    "SELECT favorite, updated_at FROM favorites WHERE track_key = ?1",
                    params![record.track_key],
                    |r| {
                        Ok(FavoriteRecord {
                            track_key: record.track_key.clone(),
                            favorite: r.get::<_, i64>(0)? != 0,
                            updated_at: r.get(1)?,
                        })
                    },
                )
                .optional()?;
            // Same winner rule as `merge_favorites`, so equal timestamps still converge.
            if local.is_some_and(|l| favorite_key(record) <= favorite_key(&l)) {
                continue;
            }
            tx.execute(
                "INSERT INTO favorites (track_key, favorite, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(track_key) DO UPDATE SET
                     favorite = excluded.favorite,
                     updated_at = excluded.updated_at",
                params![
                    record.track_key,
                    i64::from(record.favorite),
                    record.updated_at
                ],
            )?;
            changed = changed.saturating_add(1);
        }
        tx.commit()?;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{TempDir, meta, stamp};

    fn open(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).unwrap()
    }

    fn catalogue(lib: &Library, key: &str, title: &str) {
        lib.upsert_track(&stamp(key, 1), &meta(title, "Artist", "Album"), None)
            .unwrap();
    }

    fn record(key: &str, favorite: bool, updated_at: i64) -> FavoriteRecord {
        FavoriteRecord {
            track_key: key.to_owned(),
            favorite,
            updated_at,
        }
    }

    #[test]
    fn set_unset_and_is_favorite() {
        let dir = TempDir::new();
        let lib = open(&dir);
        assert!(!lib.is_favorite("a.flac").unwrap());
        lib.set_favorite("a.flac", true).unwrap();
        assert!(lib.is_favorite("a.flac").unwrap());
        lib.set_favorite("a.flac", false).unwrap();
        assert!(!lib.is_favorite("a.flac").unwrap());
    }

    #[test]
    fn favorite_keys_lists_only_favorites() {
        let dir = TempDir::new();
        let lib = open(&dir);
        lib.set_favorite("a.flac", true).unwrap();
        lib.set_favorite("b.flac", true).unwrap();
        lib.set_favorite("b.flac", false).unwrap();
        lib.set_favorite("c.flac", false).unwrap();
        let keys = lib.favorite_keys().unwrap();
        assert_eq!(keys, HashSet::from(["a.flac".to_owned()]));
    }

    #[test]
    fn favorites_returns_catalogued_tracks_newest_first() {
        let dir = TempDir::new();
        let lib = open(&dir);
        catalogue(&lib, "Artist/Album/01 - one.flac", "one");
        catalogue(&lib, "Artist/Album/02 - two.flac", "two");
        catalogue(&lib, "Artist/Album/03 - three.flac", "three");
        lib.set_favorite("Artist/Album/01 - one.flac", true)
            .unwrap();
        lib.set_favorite("unknown.flac", true).unwrap();
        lib.set_favorite("Artist/Album/02 - two.flac", true)
            .unwrap();
        lib.set_favorite("Artist/Album/03 - three.flac", false)
            .unwrap();

        assert!(lib.favorite_keys().unwrap().contains("unknown.flac"));
        let keys: Vec<String> = lib
            .favorites()
            .unwrap()
            .into_iter()
            .map(|t| t.key)
            .collect();
        assert_eq!(
            keys,
            vec![
                "Artist/Album/02 - two.flac".to_owned(),
                "Artist/Album/01 - one.flac".to_owned(),
            ]
        );
    }

    #[test]
    fn setting_same_value_keeps_updated_at() {
        let dir = TempDir::new();
        let lib = open(&dir);
        lib.set_favorite("a.flac", true).unwrap();
        let first = lib.favorite_records().unwrap()[0].updated_at;
        lib.set_favorite("a.flac", true).unwrap();
        assert_eq!(lib.favorite_records().unwrap()[0].updated_at, first);
        lib.set_favorite("a.flac", false).unwrap();
        assert!(lib.favorite_records().unwrap()[0].updated_at > first);
    }

    #[test]
    fn unset_keeps_a_false_record() {
        let dir = TempDir::new();
        let lib = open(&dir);
        lib.set_favorite("a.flac", true).unwrap();
        lib.set_favorite("a.flac", false).unwrap();
        let records = lib.favorite_records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].track_key, "a.flac");
        assert!(!records[0].favorite);
    }

    #[test]
    fn apply_inserts_overwrites_and_skips_losers() {
        let dir = TempDir::new();
        let lib = open(&dir);
        lib.set_favorite("a.flac", true).unwrap();
        lib.set_favorite("b.flac", true).unwrap();
        lib.set_favorite("c.flac", true).unwrap();
        let local = lib.favorite_records().unwrap();
        let ts = |key: &str| {
            local
                .iter()
                .find(|r| r.track_key == key)
                .unwrap()
                .updated_at
        };
        let (a_ts, b_ts, c_ts) = (ts("a.flac"), ts("b.flac"), ts("c.flac"));

        let incoming = vec![
            record("new.flac", true, 5),
            record("a.flac", false, a_ts + 1_000),
            record("b.flac", false, b_ts - 1),
            record("c.flac", false, c_ts),
        ];
        // c.flac: equal timestamp, `favorite = false` wins the tie (same rule as the merge).
        assert_eq!(lib.apply_favorite_records(&incoming).unwrap(), 3);

        let after = lib.favorite_records().unwrap();
        let get = |key: &str| after.iter().find(|r| r.track_key == key).unwrap().clone();
        assert_eq!(get("new.flac"), record("new.flac", true, 5));
        assert_eq!(get("a.flac"), record("a.flac", false, a_ts + 1_000));
        assert_eq!(get("b.flac"), record("b.flac", true, b_ts));
        assert_eq!(get("c.flac"), record("c.flac", false, c_ts));
        // Equal timestamp where the incoming record loses the tie: skipped.
        assert_eq!(
            lib.apply_favorite_records(&[record("c.flac", true, c_ts)])
                .unwrap(),
            0
        );
        assert!(!lib.is_favorite("c.flac").unwrap());
    }

    #[test]
    fn records_round_trip_into_fresh_library() {
        let dir = TempDir::new();
        let source = open(&dir);
        source.set_favorite("a.flac", true).unwrap();
        source.set_favorite("b.flac", true).unwrap();
        source.set_favorite("b.flac", false).unwrap();
        let records = source.favorite_records().unwrap();

        let fresh_dir = TempDir::new();
        let fresh = open(&fresh_dir);
        assert_eq!(fresh.apply_favorite_records(&records).unwrap(), 2);
        assert_eq!(fresh.favorite_records().unwrap(), records);
        assert!(fresh.is_favorite("a.flac").unwrap());
        assert!(!fresh.is_favorite("b.flac").unwrap());
    }
}
