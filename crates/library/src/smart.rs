//! Smart playlists: track lists computed on each request from the catalogue and the play
//! history. Nothing is stored. Play-history rows whose track is no longer catalogued are ignored.

use rusqlite::{Params, params};

use crate::db::{Library, now_ms};
use crate::error::Result;
use crate::model::Track;
use crate::quality::{QualityFilter, quality_condition};
use crate::query::{TRACK_SELECT, row_to_track, track_order_by_artist};

/// Milliseconds in one day, for the play-history windows.
const DAY_MS: i64 = 86_400_000;

/// A list the library computes from its catalogue and play history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartPlaylist {
    /// Tracks ranked by play count, then total played time, then most recent play. Only plays
    /// in the last `days` days count; `None` counts all plays. Tracks with no counted play are
    /// left out.
    MostPlayed {
        /// Length of the window in days, or `None` for all time.
        days: Option<u32>,
    },
    /// Tracks by the time they were added to the catalogue, newest first.
    RecentlyAdded,
    /// Lossless tracks above 16 bits per sample or 48 kHz, in artist album order.
    HiRes,
    /// Catalogued tracks with no play in the history, in artist album order.
    NeverPlayed,
    /// Favorite tracks, in the order of the favorites list.
    Favorites,
}

impl Library {
    /// The first `limit` tracks of a smart playlist. Blocking SQL; `Favorites` reuses
    /// `favorites()` and is cut to `limit` in Rust.
    pub fn smart_playlist(&self, kind: SmartPlaylist, limit: u32) -> Result<Vec<Track>> {
        let limit_i = i64::from(limit);
        match kind {
            SmartPlaylist::Favorites => {
                let mut tracks = self.favorites()?;
                tracks.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
                Ok(tracks)
            }
            SmartPlaylist::MostPlayed { days } => {
                let since = days.map_or(i64::MIN, |d| {
                    now_ms().saturating_sub(i64::from(d).saturating_mul(DAY_MS))
                });
                self.query_tracks(
                    &format!(
                        "WITH plays AS (
                            SELECT track_key, COUNT(*) AS plays, SUM(played_ms) AS ms,
                                   MAX(played_at) AS at, MAX(id) AS hid
                            FROM history WHERE played_at >= ?1 GROUP BY track_key
                         )
                         {TRACK_SELECT}
                         JOIN plays p ON p.track_key = t.key
                         ORDER BY p.plays DESC, p.ms DESC, p.at DESC, p.hid DESC, t.key
                         LIMIT ?2"
                    ),
                    params![since, limit_i],
                )
            }
            SmartPlaylist::RecentlyAdded => self.query_tracks(
                &format!("{TRACK_SELECT} ORDER BY t.added_at DESC, t.id DESC LIMIT ?1"),
                params![limit_i],
            ),
            SmartPlaylist::HiRes => self.query_tracks(
                &format!(
                    "{TRACK_SELECT} WHERE {} ORDER BY {} LIMIT ?1",
                    quality_condition(QualityFilter::HiRes),
                    track_order_by_artist()
                ),
                params![limit_i],
            ),
            SmartPlaylist::NeverPlayed => self.query_tracks(
                &format!(
                    "{TRACK_SELECT}
                     WHERE NOT EXISTS (SELECT 1 FROM history h WHERE h.track_key = t.key)
                     ORDER BY {} LIMIT ?1",
                    track_order_by_artist()
                ),
                params![limit_i],
            ),
        }
    }

    /// Runs a track query and collects the rows.
    fn query_tracks(&self, sql: &str, params: impl Params) -> Result<Vec<Track>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params, row_to_track)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{TempDir, meta, stamp};
    use crate::model::TrackMeta;

    fn open(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).unwrap()
    }

    fn keys(tracks: &[Track]) -> Vec<&str> {
        tracks.iter().map(|t| t.key.as_str()).collect()
    }

    /// Catalogues `key` by `artist` on `album` as track `track_no`, with the given codec and
    /// stream properties.
    fn add(
        lib: &Library,
        key: &str,
        artist: &str,
        album: &str,
        track_no: u32,
        props: (Option<&str>, Option<u8>, Option<u32>),
    ) {
        let m = TrackMeta {
            track_no: Some(track_no),
            codec: props.0.map(str::to_owned),
            bits_per_sample: props.1,
            sample_rate: props.2,
            ..meta(key, artist, album)
        };
        lib.upsert_track(&stamp(key, 1), &m, None).unwrap();
    }

    /// Inserts a play of `key` at `played_at` (Unix ms) with `played_ms`, bypassing the clock.
    fn play_at(lib: &Library, key: &str, played_at: i64, played_ms: i64) {
        let conn = lib.lock();
        conn.execute(
            "INSERT INTO history (track_key, played_at, played_ms) VALUES (?1, ?2, ?3)",
            params![key, played_at, played_ms],
        )
        .unwrap();
    }

    const PLAIN: (Option<&str>, Option<u8>, Option<u32>) = (Some("FLAC"), Some(16), Some(44_100));

    #[test]
    fn most_played_ranks_by_count_then_time_then_recency() {
        let dir = TempDir::new();
        let lib = open(&dir);
        for k in ["a.flac", "b.flac", "c.flac", "d.flac"] {
            add(&lib, k, "Artist", "Album", 1, PLAIN);
        }
        let now = now_ms();
        // a: 2 plays, 300 ms total. b: 2 plays, 500 ms total. c: 1 play, long. d: never played.
        play_at(&lib, "a.flac", now - 1_000, 100);
        play_at(&lib, "a.flac", now - 900, 200);
        play_at(&lib, "b.flac", now - 800, 250);
        play_at(&lib, "b.flac", now - 700, 250);
        play_at(&lib, "c.flac", now - 600, 9_999);

        let all = SmartPlaylist::MostPlayed { days: None };
        assert_eq!(
            keys(&lib.smart_playlist(all, 10).unwrap()),
            vec!["b.flac", "a.flac", "c.flac"],
            "count, then played_ms; d.flac (no plays) is absent"
        );
        assert_eq!(keys(&lib.smart_playlist(all, 1).unwrap()), vec!["b.flac"]);

        // Equal count and equal time: the more recent play wins.
        let tie_dir = TempDir::new();
        let tie = open(&tie_dir);
        add(&tie, "x.flac", "Artist", "Album", 1, PLAIN);
        add(&tie, "y.flac", "Artist", "Album", 2, PLAIN);
        play_at(&tie, "x.flac", now - 500, 100);
        play_at(&tie, "y.flac", now - 100, 100);
        assert_eq!(
            keys(&tie.smart_playlist(all, 10).unwrap()),
            vec!["y.flac", "x.flac"]
        );
    }

    #[test]
    fn most_played_window_counts_only_recent_plays() {
        let dir = TempDir::new();
        let lib = open(&dir);
        add(&lib, "old.flac", "Artist", "Album", 1, PLAIN);
        add(&lib, "new.flac", "Artist", "Album", 2, PLAIN);
        let now = now_ms();
        // old has three plays 40 days ago; new has one yesterday.
        for i in 0..3 {
            play_at(&lib, "old.flac", now - 40 * DAY_MS - i, 100);
        }
        play_at(&lib, "new.flac", now - DAY_MS, 100);

        let thirty = SmartPlaylist::MostPlayed { days: Some(30) };
        assert_eq!(
            keys(&lib.smart_playlist(thirty, 10).unwrap()),
            vec!["new.flac"]
        );
        let all = SmartPlaylist::MostPlayed { days: None };
        assert_eq!(
            keys(&lib.smart_playlist(all, 10).unwrap()),
            vec!["old.flac", "new.flac"]
        );
    }

    #[test]
    fn never_played_excludes_any_played_track() {
        let dir = TempDir::new();
        let lib = open(&dir);
        add(&lib, "A/Two/02 - b.flac", "Artist", "Two", 2, PLAIN);
        add(&lib, "A/One/01 - a.flac", "Artist", "One", 1, PLAIN);
        add(&lib, "A/One/02 - c.flac", "Artist", "One", 2, PLAIN);
        add(&lib, "B/Zero/01 - z.flac", "Another", "Zero", 1, PLAIN);
        play_at(&lib, "A/One/02 - c.flac", now_ms(), 1);
        // A history row for a track that is not catalogued does not affect the list.
        play_at(&lib, "gone.flac", now_ms(), 1);

        let never = lib.smart_playlist(SmartPlaylist::NeverPlayed, 10).unwrap();
        assert_eq!(
            keys(&never),
            vec![
                "B/Zero/01 - z.flac",
                "A/One/01 - a.flac",
                "A/Two/02 - b.flac"
            ],
            "ordered by album artist (Another before Artist), album, disc, track"
        );
        assert_eq!(
            lib.smart_playlist(SmartPlaylist::NeverPlayed, 2)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn recently_added_is_newest_first() {
        let dir = TempDir::new();
        let lib = open(&dir);
        add(&lib, "a.flac", "Artist", "Album", 1, PLAIN);
        add(&lib, "b.flac", "Artist", "Album", 2, PLAIN);
        {
            let conn = lib.lock();
            conn.execute("UPDATE tracks SET added_at = 100 WHERE key = 'a.flac'", [])
                .unwrap();
            conn.execute("UPDATE tracks SET added_at = 200 WHERE key = 'b.flac'", [])
                .unwrap();
        }
        assert_eq!(
            keys(
                &lib.smart_playlist(SmartPlaylist::RecentlyAdded, 10)
                    .unwrap()
            ),
            vec!["b.flac", "a.flac"]
        );
    }

    #[test]
    fn hi_res_list_uses_the_shared_classifier_and_album_order() {
        let dir = TempDir::new();
        let lib = open(&dir);
        add(
            &lib,
            "A/One/01 - hi24.flac",
            "Artist",
            "One",
            1,
            (Some("FLAC"), Some(24), Some(44_100)),
        );
        add(&lib, "A/One/02 - cd.flac", "Artist", "One", 2, PLAIN);
        add(
            &lib,
            "A/One/03 - lossy.mp3",
            "Artist",
            "One",
            3,
            (Some("MPEG"), None, Some(96_000)),
        );
        add(
            &lib,
            "B/Two/01 - hi96.wav",
            "Artist",
            "Two",
            1,
            (Some("WAV"), Some(16), Some(96_000)),
        );
        let hi = lib.smart_playlist(SmartPlaylist::HiRes, 10).unwrap();
        assert_eq!(
            keys(&hi),
            vec!["A/One/01 - hi24.flac", "B/Two/01 - hi96.wav"]
        );
        assert_eq!(
            lib.smart_playlist(SmartPlaylist::HiRes, 1).unwrap().len(),
            1
        );
    }

    #[test]
    fn favorites_follow_the_favorites_list() {
        let dir = TempDir::new();
        let lib = open(&dir);
        add(&lib, "a.flac", "Artist", "Album", 1, PLAIN);
        add(&lib, "b.flac", "Artist", "Album", 2, PLAIN);
        add(&lib, "c.flac", "Artist", "Album", 3, PLAIN);
        lib.set_favorite("a.flac", true).unwrap();
        lib.set_favorite("b.flac", true).unwrap();
        lib.set_favorite("c.flac", true).unwrap();
        let expected: Vec<String> = lib
            .favorites()
            .unwrap()
            .into_iter()
            .map(|t| t.key)
            .collect();
        let got = lib.smart_playlist(SmartPlaylist::Favorites, 10).unwrap();
        assert_eq!(
            keys(&got),
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_eq!(
            keys(&got),
            vec!["c.flac", "b.flac", "a.flac"],
            "newest favorite first"
        );
        assert_eq!(
            lib.smart_playlist(SmartPlaylist::Favorites, 2)
                .unwrap()
                .len(),
            2
        );
    }
}
