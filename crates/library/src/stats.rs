//! Listening statistics over a rolling window of the play history. Plays of tracks that are no
//! longer catalogued are ignored everywhere.

use rusqlite::{OptionalExtension, params};

use crate::db::{Library, now_ms};
use crate::error::Result;
use crate::model::{Album, Artist, Track};
use crate::query::{
    ALBUM_SELECT, ARTIST_SELECT, TRACK_SELECT, row_to_album, row_to_artist, row_to_track,
};

/// Milliseconds in one day.
const DAY_MS: i64 = 86_400_000;

/// The window a statistics query covers, ending now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsRange {
    /// The last 7 days.
    Week,
    /// The last 30 days.
    Month,
    /// The last 365 days.
    Year,
    /// Every play in the history.
    AllTime,
}

impl StatsRange {
    /// The earliest play time (Unix ms) that falls in the window ending at `now`.
    fn since_ms(self, now: i64) -> i64 {
        let days = match self {
            StatsRange::Week => 7,
            StatsRange::Month => 30,
            StatsRange::Year => 365,
            StatsRange::AllTime => return i64::MIN,
        };
        now.saturating_sub(days * DAY_MS)
    }
}

/// A track with its plays in the window.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackPlays {
    /// The track.
    pub track: Track,
    /// Number of plays in the window.
    pub plays: u64,
    /// Total played time in the window, in milliseconds.
    pub played_ms: u64,
}

/// An album with the plays of its tracks in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumPlays {
    /// The album.
    pub album: Album,
    /// Number of plays of its tracks in the window.
    pub plays: u64,
    /// Total played time of its tracks in the window, in milliseconds.
    pub played_ms: u64,
}

/// An artist (album artist) with the plays of their tracks in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistPlays {
    /// The artist.
    pub artist: Artist,
    /// Number of plays of their tracks in the window.
    pub plays: u64,
    /// Total played time of their tracks in the window, in milliseconds.
    pub played_ms: u64,
}

/// Listening totals for a window, with the top tracks, albums and artists.
#[derive(Debug, Clone, PartialEq)]
pub struct ListeningStats {
    /// Total played time in the window, in milliseconds.
    pub total_played_ms: u64,
    /// Number of plays in the window.
    pub plays: u64,
    /// Number of different catalogued tracks played in the window.
    pub distinct_tracks: u64,
    /// Most played tracks, by played time then plays.
    pub top_tracks: Vec<TrackPlays>,
    /// Most played albums, by played time then plays.
    pub top_albums: Vec<AlbumPlays>,
    /// Most played artists, by played time then plays.
    pub top_artists: Vec<ArtistPlays>,
}

/// Per-track play totals in the window, for catalogued tracks only. Used as the base of every
/// aggregate below.
const WINDOW_PLAYS: &str = "WITH win AS (
        SELECT track_key, COUNT(*) AS plays, SUM(played_ms) AS ms
        FROM history WHERE played_at >= ?1 GROUP BY track_key
    )";

impl Library {
    /// Listening statistics over `range` (ending now), with the `top` best tracks, albums and
    /// artists. Aggregation is done in SQL. Blocking.
    pub fn listening_stats(&self, range: StatsRange, top: u32) -> Result<ListeningStats> {
        let since = range.since_ms(now_ms());
        let top = i64::from(top);
        let conn = self.lock();

        let (plays, played_ms, distinct): (i64, i64, i64) = conn.query_row(
            &format!(
                "{WINDOW_PLAYS}
                 SELECT COALESCE(SUM(w.plays), 0), COALESCE(SUM(w.ms), 0), COUNT(*)
                 FROM win w JOIN tracks t ON t.key = w.track_key"
            ),
            params![since],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;

        let track_rows: Vec<(String, i64, i64)> = {
            let mut stmt = conn.prepare(&format!(
                "{WINDOW_PLAYS}
                 SELECT w.track_key, w.plays, w.ms
                 FROM win w JOIN tracks t ON t.key = w.track_key
                 ORDER BY w.ms DESC, w.plays DESC, w.track_key
                 LIMIT ?2"
            ))?;
            let rows = stmt.query_map(params![since, top], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut top_tracks = Vec::with_capacity(track_rows.len());
        {
            let mut stmt = conn.prepare_cached(&format!("{TRACK_SELECT} WHERE t.key = ?1"))?;
            for (key, plays, ms) in track_rows {
                if let Some(track) = stmt.query_row(params![key], row_to_track).optional()? {
                    top_tracks.push(TrackPlays {
                        track,
                        plays: to_u64(plays),
                        played_ms: to_u64(ms),
                    });
                }
            }
        }

        let album_rows = grouped(
            &conn,
            "t.album_id",
            "JOIN tracks t ON t.key = w.track_key",
            since,
            top,
        )?;
        let mut top_albums = Vec::with_capacity(album_rows.len());
        {
            let mut stmt = conn.prepare_cached(&format!("{ALBUM_SELECT} WHERE al.id = ?1"))?;
            for (id, plays, ms) in album_rows {
                if let Some(album) = stmt.query_row(params![id], row_to_album).optional()? {
                    top_albums.push(AlbumPlays {
                        album,
                        plays: to_u64(plays),
                        played_ms: to_u64(ms),
                    });
                }
            }
        }

        let artist_rows = grouped(
            &conn,
            "al.artist_id",
            "JOIN tracks t ON t.key = w.track_key JOIN albums al ON al.id = t.album_id",
            since,
            top,
        )?;
        let mut top_artists = Vec::with_capacity(artist_rows.len());
        {
            let mut stmt = conn.prepare_cached(&format!("{ARTIST_SELECT} WHERE ar.id = ?1"))?;
            for (id, plays, ms) in artist_rows {
                if let Some(artist) = stmt.query_row(params![id], row_to_artist).optional()? {
                    top_artists.push(ArtistPlays {
                        artist,
                        plays: to_u64(plays),
                        played_ms: to_u64(ms),
                    });
                }
            }
        }

        Ok(ListeningStats {
            total_played_ms: to_u64(played_ms),
            plays: to_u64(plays),
            distinct_tracks: to_u64(distinct),
            top_tracks,
            top_albums,
            top_artists,
        })
    }
}

/// The best `top` groups of window plays, as `(group id, plays, played_ms)`. `group` is the
/// column to group by, and `joins` brings in the tables that hold it.
fn grouped(
    conn: &rusqlite::Connection,
    group: &str,
    joins: &str,
    since: i64,
    top: i64,
) -> Result<Vec<(i64, i64, i64)>> {
    let mut stmt = conn.prepare(&format!(
        "{WINDOW_PLAYS}
         SELECT {group}, SUM(w.plays), SUM(w.ms)
         FROM win w {joins}
         GROUP BY {group}
         ORDER BY SUM(w.ms) DESC, SUM(w.plays) DESC, {group}
         LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![since, top], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A non-negative SQL total as `u64`; negative or out-of-range values become 0.
fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{TempDir, meta, stamp};

    fn open(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).unwrap()
    }

    fn catalogue(lib: &Library, key: &str, artist: &str, album: &str) {
        lib.upsert_track(&stamp(key, 1), &meta(key, artist, album), None)
            .unwrap();
    }

    fn play_at(lib: &Library, key: &str, played_at: i64, played_ms: i64) {
        let conn = lib.lock();
        conn.execute(
            "INSERT INTO history (track_key, played_at, played_ms) VALUES (?1, ?2, ?3)",
            params![key, played_at, played_ms],
        )
        .unwrap();
    }

    fn track_keys(s: &ListeningStats) -> Vec<&str> {
        s.top_tracks.iter().map(|t| t.track.key.as_str()).collect()
    }

    #[test]
    fn windows_and_totals() {
        let dir = TempDir::new();
        let lib = open(&dir);
        catalogue(&lib, "A/One/01 - a.flac", "Artist A", "One");
        catalogue(&lib, "A/One/02 - b.flac", "Artist A", "One");
        catalogue(&lib, "B/Two/01 - c.flac", "Artist B", "Two");
        let now = now_ms();
        play_at(&lib, "A/One/01 - a.flac", now - 2 * DAY_MS, 1_000); // week, month, year, all
        play_at(&lib, "A/One/01 - a.flac", now - 20 * DAY_MS, 2_000); // month, year, all
        play_at(&lib, "A/One/02 - b.flac", now - 200 * DAY_MS, 4_000); // year, all
        play_at(&lib, "B/Two/01 - c.flac", now - 400 * DAY_MS, 8_000); // all only

        let week = lib.listening_stats(StatsRange::Week, 10).unwrap();
        assert_eq!(week.plays, 1);
        assert_eq!(week.total_played_ms, 1_000);
        assert_eq!(week.distinct_tracks, 1);

        let month = lib.listening_stats(StatsRange::Month, 10).unwrap();
        assert_eq!(month.plays, 2);
        assert_eq!(month.total_played_ms, 3_000);
        assert_eq!(month.distinct_tracks, 1);

        let year = lib.listening_stats(StatsRange::Year, 10).unwrap();
        assert_eq!(year.plays, 3);
        assert_eq!(year.total_played_ms, 7_000);
        assert_eq!(year.distinct_tracks, 2);

        let all = lib.listening_stats(StatsRange::AllTime, 10).unwrap();
        assert_eq!(all.plays, 4);
        assert_eq!(all.total_played_ms, 15_000);
        assert_eq!(all.distinct_tracks, 3);
        assert_eq!(
            track_keys(&all),
            vec![
                "B/Two/01 - c.flac",
                "A/One/02 - b.flac",
                "A/One/01 - a.flac"
            ]
        );
        assert_eq!(all.top_tracks[0].plays, 1);
        assert_eq!(all.top_tracks[0].played_ms, 8_000);
    }

    #[test]
    fn albums_and_artists_aggregate_their_tracks() {
        let dir = TempDir::new();
        let lib = open(&dir);
        catalogue(&lib, "A/One/01 - a.flac", "Artist A", "One");
        catalogue(&lib, "A/One/02 - b.flac", "Artist A", "One");
        catalogue(&lib, "A/Solo/01 - s.flac", "Artist A", "Solo");
        catalogue(&lib, "B/Two/01 - c.flac", "Artist B", "Two");
        let now = now_ms();
        play_at(&lib, "A/One/01 - a.flac", now - 1, 100);
        play_at(&lib, "A/One/02 - b.flac", now - 1, 300);
        play_at(&lib, "A/One/01 - a.flac", now - 1, 100);
        play_at(&lib, "A/Solo/01 - s.flac", now - 1, 250);
        play_at(&lib, "B/Two/01 - c.flac", now - 1, 50);

        let s = lib.listening_stats(StatsRange::AllTime, 10).unwrap();
        let album_titles: Vec<(&str, u64, u64)> = s
            .top_albums
            .iter()
            .map(|a| (a.album.title.as_str(), a.plays, a.played_ms))
            .collect();
        // One: 3 plays, 500 ms. Solo: 1 play, 250 ms. Two: 1 play, 50 ms.
        assert_eq!(
            album_titles,
            vec![("One", 3, 500), ("Solo", 1, 250), ("Two", 1, 50)]
        );

        let artists: Vec<(&str, u64, u64)> = s
            .top_artists
            .iter()
            .map(|a| (a.artist.name.as_str(), a.plays, a.played_ms))
            .collect();
        // Artist A: 4 plays, 750 ms. Artist B: 1 play, 50 ms.
        assert_eq!(artists, vec![("Artist A", 4, 750), ("Artist B", 1, 50)]);

        let top1 = lib.listening_stats(StatsRange::AllTime, 1).unwrap();
        assert_eq!(top1.top_tracks.len(), 1);
        assert_eq!(top1.top_albums.len(), 1);
        assert_eq!(top1.top_artists.len(), 1);
        assert_eq!(top1.plays, 5, "top limits lists, not totals");
    }

    #[test]
    fn history_rows_of_deleted_tracks_are_ignored() {
        let dir = TempDir::new();
        let lib = open(&dir);
        catalogue(&lib, "A/One/01 - a.flac", "Artist A", "One");
        catalogue(&lib, "A/One/02 - gone.flac", "Artist A", "One");
        let now = now_ms();
        play_at(&lib, "A/One/01 - a.flac", now - 1, 100);
        play_at(&lib, "A/One/02 - gone.flac", now - 1, 9_000);
        lib.remove_tracks(&["A/One/02 - gone.flac".to_owned()])
            .unwrap();
        play_at(&lib, "A/One/02 - gone.flac", now - 1, 9_000);

        let s = lib.listening_stats(StatsRange::AllTime, 10).unwrap();
        assert_eq!(s.plays, 1);
        assert_eq!(s.total_played_ms, 100);
        assert_eq!(s.distinct_tracks, 1);
        assert_eq!(track_keys(&s), vec!["A/One/01 - a.flac"]);
        assert_eq!(s.top_albums.len(), 1);
        assert_eq!(s.top_albums[0].plays, 1);
    }

    #[test]
    fn empty_history_gives_zeroes() {
        let dir = TempDir::new();
        let lib = open(&dir);
        catalogue(&lib, "A/One/01 - a.flac", "Artist A", "One");
        let s = lib.listening_stats(StatsRange::Year, 5).unwrap();
        assert_eq!(s.plays, 0);
        assert_eq!(s.total_played_ms, 0);
        assert_eq!(s.distinct_tracks, 0);
        assert!(s.top_tracks.is_empty() && s.top_albums.is_empty() && s.top_artists.is_empty());
    }
}
