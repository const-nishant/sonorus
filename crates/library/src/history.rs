//! Play history: one row per play, trimmed to the newest `HISTORY_LIMIT` rows. History is
//! device-local and is never synced to the bucket; each device keeps its own list.

use rusqlite::params;

use crate::db::{Library, now_ms, to_i64};
use crate::error::Result;
use crate::model::{Album, Track};
use crate::query::{ALBUM_SELECT, TRACK_SELECT, row_to_album, row_to_track};

/// Maximum number of play rows kept; older plays are deleted when a new one is recorded.
pub const HISTORY_LIMIT: u32 = 5000;

/// CTE of each catalogued-or-not track key with its latest play time and row id.
const LAST_PLAYS: &str = "WITH last AS (
        SELECT track_key, MAX(played_at) AS at, MAX(id) AS hid
        FROM history GROUP BY track_key
    )";

impl Library {
    /// Records a play of `key` at the current time, then trims history to `HISTORY_LIMIT`.
    /// `played_ms` is the played duration in milliseconds.
    pub fn record_play(&self, key: &str, played_ms: u64) -> Result<()> {
        self.record_play_limited(key, played_ms, HISTORY_LIMIT)
    }

    /// `record_play` with an explicit row limit, so trimming can be tested with a small value.
    fn record_play_limited(&self, key: &str, played_ms: u64, limit: u32) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO history (track_key, played_at, played_ms) VALUES (?1, ?2, ?3)",
            params![key, now_ms(), to_i64(played_ms)],
        )?;
        tx.execute(
            "DELETE FROM history WHERE id NOT IN (
                 SELECT id FROM history ORDER BY played_at DESC, id DESC LIMIT ?1
             )",
            params![i64::from(limit)],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The most recently played catalogued tracks, one entry per track, newest play first.
    /// Keys that are no longer in the catalogue are skipped.
    pub fn recently_played(&self, limit: u32) -> Result<Vec<Track>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{LAST_PLAYS}
             {TRACK_SELECT}
             JOIN last l ON l.track_key = t.key
             ORDER BY l.at DESC, l.hid DESC
             LIMIT ?1"
        ))?;
        let rows = stmt.query_map([i64::from(limit)], row_to_track)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// The most recently played albums (those with at least one catalogued played track), one
    /// entry per album, ordered by the latest play of any of its tracks, newest first.
    pub fn recent_albums(&self, limit: u32) -> Result<Vec<Album>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{LAST_PLAYS},
             album_last AS (
                 SELECT t.album_id AS album_id, MAX(l.at) AS at, MAX(l.hid) AS hid
                 FROM last l JOIN tracks t ON t.key = l.track_key
                 GROUP BY t.album_id
             )
             {ALBUM_SELECT}
             JOIN album_last x ON x.album_id = al.id
             ORDER BY x.at DESC, x.hid DESC
             LIMIT ?1"
        ))?;
        let rows = stmt.query_map([i64::from(limit)], row_to_album)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Deletes all play history.
    pub fn clear_history(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM history", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{TempDir, meta, stamp};

    fn open(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).unwrap()
    }

    /// Catalogues `key` under `album`, by `Artist`.
    fn catalogue(lib: &Library, key: &str, album: &str) {
        lib.upsert_track(&stamp(key, 1), &meta(key, "Artist", album), None)
            .unwrap();
    }

    fn keys(tracks: &[Track]) -> Vec<&str> {
        tracks.iter().map(|t| t.key.as_str()).collect()
    }

    fn history_count(lib: &Library) -> i64 {
        let conn = lib.lock();
        conn.query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn recently_played_orders_by_latest_play_without_repeats() {
        let dir = TempDir::new();
        let lib = open(&dir);
        catalogue(&lib, "a.flac", "Album");
        catalogue(&lib, "b.flac", "Album");
        lib.record_play("a.flac", 1000).unwrap();
        lib.record_play("b.flac", 1000).unwrap();
        lib.record_play("a.flac", 1000).unwrap();
        let recent = lib.recently_played(10).unwrap();
        assert_eq!(keys(&recent), vec!["a.flac", "b.flac"]);
    }

    #[test]
    fn recently_played_skips_unknown_keys_and_honours_limit() {
        let dir = TempDir::new();
        let lib = open(&dir);
        catalogue(&lib, "a.flac", "Album");
        catalogue(&lib, "b.flac", "Album");
        catalogue(&lib, "c.flac", "Album");
        lib.record_play("a.flac", 1).unwrap();
        lib.record_play("gone.flac", 1).unwrap();
        lib.record_play("b.flac", 1).unwrap();
        lib.record_play("c.flac", 1).unwrap();
        assert_eq!(
            keys(&lib.recently_played(10).unwrap()),
            vec!["c.flac", "b.flac", "a.flac"]
        );
        assert_eq!(
            keys(&lib.recently_played(2).unwrap()),
            vec!["c.flac", "b.flac"]
        );
        assert!(lib.recently_played(0).unwrap().is_empty());
    }

    #[test]
    fn recent_albums_are_distinct_and_ordered_by_latest_play() {
        let dir = TempDir::new();
        let lib = open(&dir);
        catalogue(&lib, "Artist/One/01 - a.flac", "One");
        catalogue(&lib, "Artist/One/02 - b.flac", "One");
        catalogue(&lib, "Artist/Two/01 - c.flac", "Two");
        lib.record_play("Artist/One/01 - a.flac", 1).unwrap();
        lib.record_play("Artist/Two/01 - c.flac", 1).unwrap();
        lib.record_play("Artist/One/02 - b.flac", 1).unwrap();
        lib.record_play("unknown.flac", 1).unwrap();

        let titles: Vec<String> = lib
            .recent_albums(10)
            .unwrap()
            .into_iter()
            .map(|a| a.title)
            .collect();
        assert_eq!(titles, vec!["One", "Two"]);

        lib.record_play("Artist/Two/01 - c.flac", 1).unwrap();
        let titles: Vec<String> = lib
            .recent_albums(10)
            .unwrap()
            .into_iter()
            .map(|a| a.title)
            .collect();
        assert_eq!(titles, vec!["Two", "One"]);
        assert_eq!(lib.recent_albums(1).unwrap().len(), 1);
    }

    #[test]
    fn trimming_keeps_only_newest_limit_rows() {
        let dir = TempDir::new();
        let lib = open(&dir);
        catalogue(&lib, "a.flac", "Album");
        // Seed HISTORY_LIMIT + 5 old rows (played_at 1..=HISTORY_LIMIT+5) in one statement,
        // then one real record_play at the current time, which must trim back to the limit.
        let seeded = i64::from(HISTORY_LIMIT) + 5;
        {
            let conn = lib.lock();
            conn.execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
                 INSERT INTO history (track_key, played_at, played_ms)
                 SELECT 'a.flac', i, 1 FROM n",
                params![seeded],
            )
            .unwrap();
        }
        lib.record_play("a.flac", 1).unwrap();
        assert_eq!(history_count(&lib), i64::from(HISTORY_LIMIT));
        // The six oldest seeded rows (played_at 1..=6) were the ones removed.
        let conn = lib.lock();
        let oldest: i64 = conn
            .query_row("SELECT MIN(played_at) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(oldest, 7);
    }

    #[test]
    fn trimming_drops_oldest_rows_first() {
        let dir = TempDir::new();
        let lib = open(&dir);
        catalogue(&lib, "old.flac", "Album");
        catalogue(&lib, "new.flac", "Album");
        lib.record_play("old.flac", 1).unwrap();
        lib.record_play("new.flac", 1).unwrap();
        lib.record_play("new.flac", 1).unwrap();
        lib.record_play_limited("new.flac", 1, 2).unwrap();
        assert_eq!(history_count(&lib), 2);
        assert_eq!(keys(&lib.recently_played(10).unwrap()), vec!["new.flac"]);
    }

    #[test]
    fn clear_history_empties() {
        let dir = TempDir::new();
        let lib = open(&dir);
        catalogue(&lib, "a.flac", "Album");
        lib.record_play("a.flac", 1).unwrap();
        lib.clear_history().unwrap();
        assert_eq!(history_count(&lib), 0);
        assert!(lib.recently_played(10).unwrap().is_empty());
    }
}
