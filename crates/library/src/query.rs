//! Read queries over the catalogue: tracks, albums, artists, search, covers, stats.

use std::path::PathBuf;

use rusqlite::{OptionalExtension, Row, params};

use crate::db::{Library, opt_conv};
use crate::error::{LibraryError, Result};
use crate::lrc::{LyricLine, parse_lrc};
use crate::model::{
    Album, AlbumSort, Artist, LibraryStats, LyricsSource, SearchResults, Track, TrackLyrics,
};
use crate::quality::{QualityFilter, quality_condition};

/// Track columns plus the album and artist names; every track read goes through this.
pub(crate) const TRACK_SELECT: &str =
    "SELECT t.id, t.key, t.size, t.etag, t.title, t.artist, ar.name, al.title,
        t.album_id, al.artist_id, t.track_no, t.disc_no, t.year, t.genre, t.duration_ms,
        t.sample_rate, t.bits_per_sample, t.channels, t.codec, t.cover_id,
        ((t.lyrics IS NOT NULL AND t.lyrics <> '')
            OR EXISTS (SELECT 1 FROM lyrics_sidecars s WHERE s.track_key = t.key AND s.text <> '')),
        t.added_at,
        t.rg_track_gain, t.rg_track_peak, t.rg_album_gain, t.rg_album_peak
    FROM tracks t
    JOIN albums al ON al.id = t.album_id
    JOIN artists ar ON ar.id = al.artist_id";

pub(crate) const ALBUM_SELECT: &str = "SELECT al.id, al.title, ar.name, al.artist_id, al.year,
        (SELECT COUNT(*) FROM tracks t WHERE t.album_id = al.id),
        (SELECT COALESCE(SUM(t.duration_ms), 0) FROM tracks t WHERE t.album_id = al.id),
        al.cover_id, al.added_at
    FROM albums al
    JOIN artists ar ON ar.id = al.artist_id";

pub(crate) const ARTIST_SELECT: &str = "SELECT ar.id, ar.name,
        (SELECT COUNT(*) FROM albums al WHERE al.artist_id = ar.id),
        (SELECT COUNT(*) FROM tracks t JOIN albums al ON al.id = t.album_id
            WHERE al.artist_id = ar.id)
    FROM artists ar";

pub(crate) fn row_to_track(row: &Row<'_>) -> rusqlite::Result<Track> {
    let size: i64 = row.get(2)?;
    Ok(Track {
        id: row.get(0)?,
        key: row.get(1)?,
        size: u64::try_from(size).unwrap_or(0),
        etag: row.get(3)?,
        title: row.get(4)?,
        artist: row.get(5)?,
        album_artist: row.get(6)?,
        album: row.get(7)?,
        album_id: row.get(8)?,
        artist_id: row.get(9)?,
        track_no: opt_conv(row, 10)?,
        disc_no: opt_conv(row, 11)?,
        year: opt_conv(row, 12)?,
        genre: row.get(13)?,
        duration_ms: opt_conv(row, 14)?,
        sample_rate: opt_conv(row, 15)?,
        bits_per_sample: opt_conv(row, 16)?,
        channels: opt_conv(row, 17)?,
        codec: row.get(18)?,
        cover_id: row.get(19)?,
        has_lyrics: row.get(20)?,
        added_at: row.get(21)?,
        track_gain_db: row.get(22)?,
        track_peak: row.get(23)?,
        album_gain_db: row.get(24)?,
        album_peak: row.get(25)?,
    })
}

pub(crate) fn row_to_album(row: &Row<'_>) -> rusqlite::Result<Album> {
    let track_count: i64 = row.get(5)?;
    let duration_ms: i64 = row.get(6)?;
    Ok(Album {
        id: row.get(0)?,
        title: row.get(1)?,
        artist: row.get(2)?,
        artist_id: row.get(3)?,
        year: opt_conv(row, 4)?,
        track_count: u32::try_from(track_count).unwrap_or(0),
        duration_ms: u64::try_from(duration_ms).unwrap_or(0),
        cover_id: row.get(7)?,
        added_at: row.get(8)?,
    })
}

pub(crate) fn row_to_artist(row: &Row<'_>) -> rusqlite::Result<Artist> {
    let album_count: i64 = row.get(2)?;
    let track_count: i64 = row.get(3)?;
    Ok(Artist {
        id: row.get(0)?,
        name: row.get(1)?,
        album_count: u32::try_from(album_count).unwrap_or(0),
        track_count: u32::try_from(track_count).unwrap_or(0),
    })
}

/// Track order of the artist album sort: album artist, album, disc, track, then title and key.
/// Used by the browse lists (smart playlists and quality browses).
pub(crate) fn track_order_by_artist() -> String {
    format!(
        "{}, t.disc_no NULLS LAST, t.track_no NULLS LAST, t.title, t.key",
        album_order(AlbumSort::Artist)
    )
}

fn album_order(sort: AlbumSort) -> &'static str {
    match sort {
        AlbumSort::Artist => "ar.name COLLATE NOCASE, al.year, al.title COLLATE NOCASE",
        AlbumSort::Title => "al.title COLLATE NOCASE, al.id",
        AlbumSort::Year => "al.year DESC NULLS LAST, al.title COLLATE NOCASE",
        AlbumSort::RecentlyAdded => "al.added_at DESC, al.id DESC",
    }
}

/// Splits user input into search tokens: anything that is not alphanumeric or an apostrophe
/// becomes a separator.
fn search_tokens(query: &str) -> Vec<String> {
    let cleaned: String = query
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect();
    cleaned.split_whitespace().map(str::to_owned).collect()
}

/// FTS5 expression requiring every token as a quoted prefix term, e.g. `"tok1"* AND "tok2"*`.
fn fts_terms(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Chooses the lyrics to show from the sidecar text and the embedded text. Blank text counts as
/// absent.
///
/// In order: a sidecar that parses as synced; embedded text that parses as synced; plain sidecar
/// text; plain embedded text. `plain` is always set: the line texts joined with '\n' for synced
/// lyrics, or the text itself. `None` when neither text is usable.
pub(crate) fn resolve_lyrics(sidecar: Option<&str>, embedded: Option<&str>) -> Option<TrackLyrics> {
    let sidecar = sidecar.filter(|s| !s.trim().is_empty());
    let embedded = embedded.filter(|s| !s.trim().is_empty());
    if let Some(lines) = sidecar.and_then(parse_lrc) {
        return Some(synced(lines, LyricsSource::Sidecar));
    }
    if let Some(lines) = embedded.and_then(parse_lrc) {
        return Some(synced(lines, LyricsSource::Embedded));
    }
    if let Some(text) = sidecar {
        return Some(plain(text, LyricsSource::Sidecar));
    }
    embedded.map(|text| plain(text, LyricsSource::Embedded))
}

fn synced(lines: Vec<LyricLine>, source: LyricsSource) -> TrackLyrics {
    let text = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    TrackLyrics {
        synced: Some(lines),
        plain: Some(text),
        source,
    }
}

fn plain(text: &str, source: LyricsSource) -> TrackLyrics {
    TrackLyrics {
        synced: None,
        plain: Some(text.to_owned()),
        source,
    }
}

impl Library {
    /// The track with `id`, or `LibraryError::NotFound`.
    pub fn track(&self, id: i64) -> Result<Track> {
        let conn = self.lock();
        let found = conn
            .query_row(
                &format!("{TRACK_SELECT} WHERE t.id = ?1"),
                params![id],
                row_to_track,
            )
            .optional()?;
        found.ok_or_else(|| LibraryError::NotFound(format!("track {id}")))
    }

    /// Tracks for `ids` in the same order; unknown ids are skipped.
    pub fn tracks_by_ids(&self, ids: &[i64]) -> Result<Vec<Track>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!("{TRACK_SELECT} WHERE t.id = ?1"))?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(track) = stmt.query_row(params![id], row_to_track).optional()? {
                out.push(track);
            }
        }
        Ok(out)
    }

    pub fn track_by_key(&self, key: &str) -> Result<Option<Track>> {
        let conn = self.lock();
        let found = conn
            .query_row(
                &format!("{TRACK_SELECT} WHERE t.key = ?1"),
                params![key],
                row_to_track,
            )
            .optional()?;
        Ok(found)
    }

    /// Lyrics text for a track; `None` if the track is missing or has no lyrics.
    pub fn lyrics(&self, track_id: i64) -> Result<Option<String>> {
        let conn = self.lock();
        let lyrics: Option<Option<String>> = conn
            .query_row(
                "SELECT lyrics FROM tracks WHERE id = ?1",
                params![track_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(lyrics.flatten().filter(|s| !s.is_empty()))
    }

    /// Lyrics of a track, or `None` if the track is missing or has no lyrics. See
    /// `resolve_lyrics` for the order in which the sidecar and embedded texts are used.
    pub fn track_lyrics(&self, track_id: i64) -> Result<Option<TrackLyrics>> {
        let found: Option<(Option<String>, Option<String>)> = {
            let conn = self.lock();
            conn.query_row(
                "SELECT t.lyrics, s.text FROM tracks t
                 LEFT JOIN lyrics_sidecars s ON s.track_key = t.key
                 WHERE t.id = ?1",
                params![track_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        };
        Ok(found.and_then(|(embedded, sidecar)| {
            resolve_lyrics(sidecar.as_deref(), embedded.as_deref())
        }))
    }

    pub fn albums(&self, sort: AlbumSort, limit: u32, offset: u32) -> Result<Vec<Album>> {
        let conn = self.lock();
        let sql = format!(
            "{ALBUM_SELECT} ORDER BY {} LIMIT ?1 OFFSET ?2",
            album_order(sort)
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![i64::from(limit), i64::from(offset)], row_to_album)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// The album with `id`, or `LibraryError::NotFound`.
    pub fn album(&self, id: i64) -> Result<Album> {
        let conn = self.lock();
        let found = conn
            .query_row(
                &format!("{ALBUM_SELECT} WHERE al.id = ?1"),
                params![id],
                row_to_album,
            )
            .optional()?;
        found.ok_or_else(|| LibraryError::NotFound(format!("album {id}")))
    }

    /// Tracks of an album in disc/track order (unnumbered tracks last, then by title).
    pub fn album_tracks(&self, album_id: i64) -> Result<Vec<Track>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{TRACK_SELECT} WHERE t.album_id = ?1
             ORDER BY t.disc_no NULLS LAST, t.track_no NULLS LAST, t.title"
        ))?;
        let rows = stmt.query_map(params![album_id], row_to_track)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    pub fn artists(&self, limit: u32, offset: u32) -> Result<Vec<Artist>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{ARTIST_SELECT} ORDER BY ar.name COLLATE NOCASE, ar.id LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = stmt.query_map(params![i64::from(limit), i64::from(offset)], row_to_artist)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// The artist with `id`, or `LibraryError::NotFound`.
    pub fn artist(&self, id: i64) -> Result<Artist> {
        let conn = self.lock();
        let found = conn
            .query_row(
                &format!("{ARTIST_SELECT} WHERE ar.id = ?1"),
                params![id],
                row_to_artist,
            )
            .optional()?;
        found.ok_or_else(|| LibraryError::NotFound(format!("artist {id}")))
    }

    /// Albums by an artist, ordered by year then title.
    pub fn artist_albums(&self, artist_id: i64) -> Result<Vec<Album>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{ALBUM_SELECT} WHERE al.artist_id = ?1 ORDER BY al.year, al.title COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map(params![artist_id], row_to_album)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Full-text search over the `track_fts` index. Every token must match as a prefix
    /// (AND), with case and diacritics folded, so `beyonce` finds `Beyoncé`.
    /// Tracks match on any of title, artist, album or album artist (ranked by bm25); albums
    /// match on their title and artists on their name (the album-artist column), each only
    /// within that column. Same as `search_filtered` with `QualityFilter::Any`.
    pub fn search(&self, query: &str, limit: u32) -> Result<SearchResults> {
        self.search_filtered(query, limit, QualityFilter::Any)
    }

    /// `search`, keeping only tracks of one audio quality (see `QualityFilter`). Albums and
    /// artists are kept only when one of their matching tracks passes the filter.
    ///
    /// With no searchable tokens, `Any` returns nothing. Any other filter browses: it returns the
    /// tracks of that quality in artist album order, at most `limit`, with no albums or artists.
    pub fn search_filtered(
        &self,
        query: &str,
        limit: u32,
        filter: QualityFilter,
    ) -> Result<SearchResults> {
        let quality = quality_condition(filter);
        let limit = i64::from(limit);
        let tokens = search_tokens(query);
        if tokens.is_empty() {
            if filter == QualityFilter::Any {
                return Ok(SearchResults::default());
            }
            let conn = self.lock();
            let mut stmt = conn.prepare(&format!(
                "{TRACK_SELECT} WHERE {quality} ORDER BY {} LIMIT ?1",
                track_order_by_artist()
            ))?;
            let rows = stmt.query_map(params![limit], row_to_track)?;
            let tracks = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            return Ok(SearchResults {
                tracks,
                ..SearchResults::default()
            });
        }
        let terms = fts_terms(&tokens);
        let album_match = format!("album : ({terms})");
        let artist_match = format!("album_artist : ({terms})");
        let conn = self.lock();

        let tracks = {
            let mut stmt = conn.prepare(&format!(
                "{TRACK_SELECT} JOIN track_fts ON track_fts.rowid = t.id
                 WHERE track_fts MATCH ?1 AND {quality}
                 ORDER BY bm25(track_fts) LIMIT ?2"
            ))?;
            let rows = stmt.query_map(params![terms, limit], row_to_track)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let albums = {
            let mut stmt = conn.prepare(&format!(
                "{ALBUM_SELECT} WHERE al.id IN (
                    SELECT t.album_id FROM tracks t
                    JOIN track_fts ON track_fts.rowid = t.id
                    WHERE track_fts MATCH ?1 AND {quality})
                 ORDER BY al.title COLLATE NOCASE, al.id LIMIT ?2"
            ))?;
            let rows = stmt.query_map(params![album_match, limit], row_to_album)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let artists = {
            let mut stmt = conn.prepare(&format!(
                "{ARTIST_SELECT} WHERE ar.id IN (
                    SELECT al.artist_id FROM tracks t
                    JOIN albums al ON al.id = t.album_id
                    JOIN track_fts ON track_fts.rowid = t.id
                    WHERE track_fts MATCH ?1 AND {quality})
                 ORDER BY ar.name COLLATE NOCASE, ar.id LIMIT ?2"
            ))?;
            let rows = stmt.query_map(params![artist_match, limit], row_to_artist)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        Ok(SearchResults {
            tracks,
            albums,
            artists,
        })
    }

    /// Absolute path of a cover file, or `None` if no such cover row exists.
    pub fn cover_path(&self, cover_id: i64) -> Result<Option<PathBuf>> {
        let conn = self.lock();
        let file: Option<String> = conn
            .query_row(
                "SELECT file FROM covers WHERE id = ?1",
                params![cover_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(file.map(|f| self.covers_dir().join(f)))
    }

    pub fn stats(&self) -> Result<LibraryStats> {
        let conn = self.lock();
        let (tracks, albums, artists, total_bytes, total_duration_ms): (i64, i64, i64, i64, i64) =
            conn.query_row(
                "SELECT (SELECT COUNT(*) FROM tracks),
                        (SELECT COUNT(*) FROM albums),
                        (SELECT COUNT(*) FROM artists),
                        (SELECT COALESCE(SUM(size), 0) FROM tracks),
                        (SELECT COALESCE(SUM(duration_ms), 0) FROM tracks)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
        let to_u64 = |v: i64| u64::try_from(v).unwrap_or(0);
        Ok(LibraryStats {
            tracks: to_u64(tracks),
            albums: to_u64(albums),
            artists: to_u64(artists),
            total_bytes: to_u64(total_bytes),
            total_duration_ms: to_u64(total_duration_ms),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{TempDir, meta, stamp};
    use crate::model::{ObjectStamp, TrackMeta};

    fn lib(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).unwrap()
    }

    fn add(lib: &Library, key: &str, m: TrackMeta, cover: Option<i64>) -> i64 {
        let s: ObjectStamp = stamp(key, 1000);
        lib.upsert_track(&s, &m, cover).unwrap()
    }

    #[test]
    fn resolve_lyrics_order() {
        use crate::lrc::LyricLine;

        assert_eq!(resolve_lyrics(None, None), None);
        assert_eq!(
            resolve_lyrics(Some("  \n"), Some("")),
            None,
            "blank is absent"
        );

        assert_eq!(
            resolve_lyrics(None, Some("words")),
            Some(TrackLyrics {
                synced: None,
                plain: Some("words".into()),
                source: LyricsSource::Embedded,
            })
        );
        assert_eq!(
            resolve_lyrics(Some("side words"), Some("words")),
            Some(TrackLyrics {
                synced: None,
                plain: Some("side words".into()),
                source: LyricsSource::Sidecar,
            })
        );

        let side_wins = resolve_lyrics(Some("[00:01.00]side"), Some("[00:02.00]emb")).unwrap();
        assert_eq!(side_wins.source, LyricsSource::Sidecar);
        assert_eq!(
            side_wins.synced,
            Some(vec![LyricLine {
                time_ms: 1_000,
                text: "side".into()
            }])
        );
        assert_eq!(side_wins.plain.as_deref(), Some("side"));

        let embedded_synced =
            resolve_lyrics(Some("plain side"), Some("[00:02.00]e\n[00:03.00]f")).unwrap();
        assert_eq!(embedded_synced.source, LyricsSource::Embedded);
        assert_eq!(embedded_synced.synced.map(|v| v.len()), Some(2));
        assert_eq!(embedded_synced.plain.as_deref(), Some("e\nf"));

        let gap = resolve_lyrics(None, Some("[00:01.00]a\n[00:02.00]\n[00:03.00]c")).unwrap();
        assert_eq!(gap.plain.as_deref(), Some("a\n\nc"), "gaps keep their line");
    }

    #[test]
    fn sidecar_counts_as_lyrics_and_track_lyrics_reads_it() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        let key = "Artist A/Album One/01 - a.flac";
        let id = add(&lib, key, meta("a", "Artist A", "Album One"), None);
        assert!(!lib.track(id).unwrap().has_lyrics);
        assert_eq!(lib.track_lyrics(id).unwrap(), None);

        lib.upsert_sidecar(
            key,
            "Artist A/Album One/01 - a.lrc",
            16,
            None,
            "[00:01.00]side",
        )
        .unwrap();
        assert!(
            lib.track(id).unwrap().has_lyrics,
            "sidecar row sets has_lyrics"
        );
        assert!(lib.track_by_key(key).unwrap().unwrap().has_lyrics);
        let lyrics = lib.track_lyrics(id).unwrap().expect("lyrics");
        assert_eq!(lyrics.source, LyricsSource::Sidecar);
        assert_eq!(lyrics.synced.map(|v| v.len()), Some(1));

        lib.upsert_sidecar(key, "Artist A/Album One/01 - a.lrc", 2, None, "")
            .unwrap();
        assert!(
            !lib.track(id).unwrap().has_lyrics,
            "empty sidecar is no lyrics"
        );
        assert_eq!(lib.track_lyrics(id).unwrap(), None);
        assert_eq!(lib.track_lyrics(9999).unwrap(), None);
    }

    #[test]
    fn album_tracks_order_disc_then_track_nulls_last() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        let album = |title: &str, disc: Option<u32>, track: Option<u32>| TrackMeta {
            title: Some(title.into()),
            artist: Some("Artist A".into()),
            album: Some("Album One".into()),
            disc_no: disc,
            track_no: track,
            ..Default::default()
        };
        add(
            &lib,
            "Artist A/Album One/a.flac",
            album("Loose B", None, None),
            None,
        );
        add(
            &lib,
            "Artist A/Album One/b.flac",
            album("Disc2 Track1", Some(2), Some(1)),
            None,
        );
        add(
            &lib,
            "Artist A/Album One/c.flac",
            album("Disc1 Track2", Some(1), Some(2)),
            None,
        );
        add(
            &lib,
            "Artist A/Album One/d.flac",
            album("Disc1 Track1", Some(1), Some(1)),
            None,
        );
        add(
            &lib,
            "Artist A/Album One/e.flac",
            album("Loose A", None, None),
            None,
        );

        let album_id = lib
            .track_by_key("Artist A/Album One/d.flac")
            .unwrap()
            .unwrap()
            .album_id;
        let titles: Vec<String> = lib
            .album_tracks(album_id)
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(
            titles,
            [
                "Disc1 Track1",
                "Disc1 Track2",
                "Disc2 Track1",
                "Loose A",
                "Loose B"
            ]
        );
    }

    #[test]
    fn track_lookups_and_lyrics() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        let with = TrackMeta {
            title: Some("Lyrics Song".into()),
            artist: Some("Artist A".into()),
            album: Some("Album One".into()),
            lyrics: Some("la la la".into()),
            ..Default::default()
        };
        let without = TrackMeta {
            lyrics: Some(String::new()),
            ..meta("Quiet", "Artist A", "Album One")
        };
        let id1 = add(&lib, "Artist A/Album One/01 - a.flac", with, None);
        let id2 = add(&lib, "Artist A/Album One/02 - b.flac", without, None);

        let t = lib.track(id1).unwrap();
        assert_eq!(t.title, "Lyrics Song");
        assert!(t.has_lyrics);
        assert_eq!(t.album_artist, "Artist A");
        assert_eq!(lib.lyrics(id1).unwrap().as_deref(), Some("la la la"));

        let t2 = lib.track(id2).unwrap();
        assert!(!t2.has_lyrics);
        assert_eq!(lib.lyrics(id2).unwrap(), None);
        assert_eq!(lib.lyrics(9999).unwrap(), None);

        assert!(matches!(lib.track(9999), Err(LibraryError::NotFound(_))));
        assert_eq!(
            lib.track_by_key("Artist A/Album One/02 - b.flac")
                .unwrap()
                .unwrap()
                .id,
            id2
        );
        assert!(lib.track_by_key("missing").unwrap().is_none());

        let ids = lib.tracks_by_ids(&[id2, 9999, id1]).unwrap();
        let got: Vec<i64> = ids.iter().map(|t| t.id).collect();
        assert_eq!(got, vec![id2, id1]);
    }

    #[test]
    fn search_prefix_diacritics_and_multi_token() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        add(
            &lib,
            "Artist A/Album One/01 - First Song.flac",
            TrackMeta {
                title: Some("First Song".into()),
                artist: Some("Artist A".into()),
                album: Some("Album One".into()),
                ..Default::default()
            },
            None,
        );
        add(
            &lib,
            "Artist B/Album Two/01 - Halo.flac",
            TrackMeta {
                title: Some("Halo".into()),
                artist: Some("Beyoncé".into()),
                album: Some("I Am".into()),
                ..Default::default()
            },
            None,
        );
        add(
            &lib,
            "Artist B/Album Two/02 - Wave Song.wav",
            TrackMeta {
                title: Some("Wave Song".into()),
                artist: Some("Artist B".into()),
                album: Some("Album Two".into()),
                ..Default::default()
            },
            None,
        );

        let r = lib.search("firs", 10).unwrap();
        assert_eq!(r.tracks.len(), 1);
        assert_eq!(r.tracks[0].title, "First Song");

        let r = lib.search("beyonce", 10).unwrap();
        assert_eq!(r.tracks.len(), 1);
        assert_eq!(r.tracks[0].title, "Halo");
        let r = lib.search("Beyoncé", 10).unwrap();
        assert_eq!(r.tracks.len(), 1);

        let r = lib.search("first artist", 10).unwrap();
        assert_eq!(r.tracks.len(), 1);
        assert_eq!(r.tracks[0].title, "First Song");
        assert!(lib.search("first halo", 10).unwrap().tracks.is_empty());

        let r = lib.search("song", 10).unwrap();
        assert_eq!(r.tracks.len(), 2);

        let r = lib.search("wave", 1).unwrap();
        assert_eq!(r.tracks.len(), 1);
    }

    #[test]
    fn search_special_characters_do_not_error() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        add(
            &lib,
            "A/B/01 - First Song.flac",
            meta("First Song", "A", "B"),
            None,
        );
        for q in [
            "\"",
            "*",
            "(",
            "\"*(",
            "AND OR NOT",
            "'",
            "a\"b",
            "NEAR(a b)",
            "%",
            "_",
            "\\",
        ] {
            lib.search(q, 10).unwrap();
        }
        assert!(lib.search("\"", 10).unwrap().tracks.is_empty());
        assert!(lib.search("   ", 10).unwrap().tracks.is_empty());
        assert_eq!(lib.search("first (song", 10).unwrap().tracks.len(), 1);
    }

    #[test]
    fn search_albums_and_artists_via_fts() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        add(
            &lib,
            "A/100% Pure/01 - x.flac",
            meta("x", "Artist A", "100% Pure"),
            None,
        );
        add(
            &lib,
            "A/Other/01 - y.flac",
            meta("y", "Artist A", "Other"),
            None,
        );
        add(
            &lib,
            "A/Album One/01 - p.flac",
            meta("p", "Artist A", "Album One"),
            None,
        );
        add(
            &lib,
            "A/Album Two/01 - q.flac",
            meta("q", "Artist A", "Album Two"),
            None,
        );
        add(
            &lib,
            "Beyoncé/Dangerously/01 - z.flac",
            meta("z", "Beyoncé", "Dangerously"),
            None,
        );

        // Diacritic folding and case folding on the artist column.
        for q in ["beyonce", "Beyoncé", "BEYONCE"] {
            let r = lib.search(q, 10).unwrap();
            assert_eq!(r.artists.len(), 1, "query {q:?}");
            assert_eq!(r.artists[0].name, "Beyoncé");
            let albums = lib.artist_albums(r.artists[0].id).unwrap();
            assert_eq!(albums.len(), 1);
            assert_eq!(albums[0].title, "Dangerously");
        }

        // Prefix match on album titles.
        let r = lib.search("dang", 10).unwrap();
        assert_eq!(r.albums.len(), 1);
        assert_eq!(r.albums[0].title, "Dangerously");

        // Punctuation is not an error; "100%" is tokenised to "100".
        let r = lib.search("100%", 10).unwrap();
        assert_eq!(r.albums.len(), 1);
        assert_eq!(r.albums[0].title, "100% Pure");

        // Each token must match the same column (AND); "artist" is not in any album title.
        let r = lib.search("artist a", 10).unwrap();
        assert_eq!(r.artists.len(), 1);
        assert_eq!(r.artists[0].name, "Artist A");
        assert!(r.albums.is_empty());

        // Multi-token AND within the album title.
        let r = lib.search("album one", 10).unwrap();
        assert_eq!(r.albums.len(), 1);
        assert_eq!(r.albums[0].title, "Album One");
    }

    #[test]
    fn album_sorts_and_paging() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        let with_year = |artist: &str, album: &str, year: Option<i32>| TrackMeta {
            title: Some("t".into()),
            artist: Some(artist.into()),
            album: Some(album.into()),
            year,
            ..Default::default()
        };
        add(
            &lib,
            "k1/Zeta/01 - a.flac",
            with_year("Artist B", "Zeta", Some(2001)),
            None,
        );
        add(
            &lib,
            "k2/Alpha/01 - b.flac",
            with_year("Artist B", "Alpha", None),
            None,
        );
        add(
            &lib,
            "k3/Mid/01 - c.flac",
            with_year("Artist A", "Mid", Some(1999)),
            None,
        );

        let titles = |sort| -> Vec<String> {
            lib.albums(sort, 10, 0)
                .unwrap()
                .into_iter()
                .map(|a| a.title)
                .collect()
        };
        assert_eq!(titles(AlbumSort::Artist), ["Mid", "Alpha", "Zeta"]);
        assert_eq!(titles(AlbumSort::Title), ["Alpha", "Mid", "Zeta"]);
        assert_eq!(titles(AlbumSort::Year), ["Zeta", "Mid", "Alpha"]);
        assert_eq!(titles(AlbumSort::RecentlyAdded), ["Mid", "Alpha", "Zeta"]);

        let page: Vec<String> = lib
            .albums(AlbumSort::Title, 2, 1)
            .unwrap()
            .into_iter()
            .map(|a| a.title)
            .collect();
        assert_eq!(page, ["Mid", "Zeta"]);

        let zeta = lib.albums(AlbumSort::Title, 10, 0).unwrap().pop().unwrap();
        assert_eq!(zeta.title, "Zeta");
        assert_eq!(zeta.year, Some(2001));
        assert_eq!(zeta.track_count, 1);
        assert_eq!(lib.album(zeta.id).unwrap(), zeta);
        assert!(matches!(lib.album(9999), Err(LibraryError::NotFound(_))));

        let artists = lib.artists(10, 0).unwrap();
        assert_eq!(
            artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Artist A", "Artist B"]
        );
        let b = artists[1].clone();
        assert_eq!(b.album_count, 2);
        assert_eq!(b.track_count, 2);
        assert_eq!(lib.artist(b.id).unwrap(), b);
        assert_eq!(lib.artist_albums(b.id).unwrap().len(), 2);
        assert!(matches!(lib.artist(9999), Err(LibraryError::NotFound(_))));
    }

    #[test]
    fn cover_path_and_stats() {
        let dir = TempDir::new();
        let covers = dir.path().join("covers");
        let lib = Library::open_in_memory(&covers).unwrap();
        let cover = lib.store_cover(b"img", "image/jpeg").unwrap();
        let s1 = ObjectStamp {
            key: "A/B/01 - x.flac".into(),
            size: 100,
            etag: None,
            last_modified: None,
        };
        lib.upsert_track(
            &s1,
            &TrackMeta {
                duration_ms: Some(1500),
                ..meta("x", "A", "B")
            },
            Some(cover),
        )
        .unwrap();
        lib.upsert_track(
            &ObjectStamp {
                key: "A/B/02 - y.flac".into(),
                size: 50,
                etag: None,
                last_modified: None,
            },
            &TrackMeta {
                duration_ms: Some(500),
                ..meta("y", "A", "B")
            },
            None,
        )
        .unwrap();

        let path = lib.cover_path(cover).unwrap().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"img");
        assert!(lib.cover_path(9999).unwrap().is_none());

        let t = lib.track_by_key("A/B/01 - x.flac").unwrap().unwrap();
        assert_eq!(t.cover_id, Some(cover));
        assert_eq!(lib.album(t.album_id).unwrap().cover_id, Some(cover));

        let stats = lib.stats().unwrap();
        assert_eq!(
            stats,
            LibraryStats {
                tracks: 2,
                albums: 1,
                artists: 1,
                total_bytes: 150,
                total_duration_ms: 2000,
            }
        );
    }
}
