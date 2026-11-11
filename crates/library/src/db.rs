//! SQLite catalogue: schema and migrations, metadata resolution rules, and the write paths
//! (upserts, removals, covers, pruning). Read queries live in `query.rs`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{LibraryError, Result};
use crate::model::{ObjectStamp, PruneStats, TrackMeta};

const SCHEMA_VERSION: i64 = 7;

const SCHEMA_V1: &str = "
CREATE TABLE artists (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE covers (
    id    INTEGER PRIMARY KEY,
    hash  TEXT NOT NULL UNIQUE,
    mime  TEXT NOT NULL,
    file  TEXT NOT NULL,
    bytes INTEGER NOT NULL
);
CREATE TABLE albums (
    id        INTEGER PRIMARY KEY,
    title     TEXT NOT NULL COLLATE NOCASE,
    artist_id INTEGER NOT NULL REFERENCES artists(id),
    year      INTEGER,
    cover_id  INTEGER REFERENCES covers(id),
    added_at  INTEGER NOT NULL,
    UNIQUE(title, artist_id)
);
CREATE INDEX albums_artist ON albums(artist_id);
CREATE TABLE tracks (
    id              INTEGER PRIMARY KEY,
    key             TEXT NOT NULL UNIQUE,
    size            INTEGER NOT NULL,
    etag            TEXT,
    last_modified   INTEGER,
    title           TEXT NOT NULL,
    artist          TEXT NOT NULL,
    album_id        INTEGER NOT NULL REFERENCES albums(id),
    track_no        INTEGER,
    track_total     INTEGER,
    disc_no         INTEGER,
    disc_total      INTEGER,
    year            INTEGER,
    genre           TEXT,
    duration_ms     INTEGER,
    sample_rate     INTEGER,
    bits_per_sample INTEGER,
    channels        INTEGER,
    bitrate_kbps    INTEGER,
    codec           TEXT,
    cover_id        INTEGER REFERENCES covers(id),
    lyrics          TEXT,
    added_at        INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX tracks_album ON tracks(album_id);
CREATE VIRTUAL TABLE track_fts USING fts5(
    title, artist, album, album_artist,
    tokenize = 'unicode61 remove_diacritics 2'
);
";

/// v2: user data. Keyed by object key (stable across rescans and devices), not track id.
/// Timestamps in these tables are Unix milliseconds.
const SCHEMA_V2: &str = "
CREATE TABLE playlists (
    id         INTEGER PRIMARY KEY,
    uuid       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted    INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE playlist_items (
    id          INTEGER PRIMARY KEY,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    track_key   TEXT NOT NULL
);
CREATE INDEX playlist_items_pl ON playlist_items(playlist_id, position);
CREATE TABLE favorites (
    track_key  TEXT PRIMARY KEY,
    favorite   INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE history (
    id        INTEGER PRIMARY KEY,
    track_key TEXT NOT NULL,
    played_at INTEGER NOT NULL,
    played_ms INTEGER NOT NULL
);
CREATE INDEX history_played ON history(played_at);
CREATE INDEX history_key ON history(track_key);
";

/// v3: the upload queue. One row per file sent to the bucket from this device. Timestamps are
/// Unix milliseconds. `state` values are listed in `uploads::UploadState`.
const SCHEMA_V3: &str = "
CREATE TABLE uploads (
    id           INTEGER PRIMARY KEY,
    source_uri   TEXT NOT NULL,
    display_name TEXT NOT NULL,
    size         INTEGER NOT NULL,
    blake3       TEXT NOT NULL,
    key          TEXT NOT NULL,
    content_type TEXT NOT NULL,
    upload_id    TEXT,
    state        TEXT NOT NULL CHECK (state IN ('queued', 'uploading', 'done', 'duplicate', 'failed', 'cancelled')),
    bytes_done   INTEGER NOT NULL DEFAULT 0,
    error        TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX uploads_blake3 ON uploads(blake3);
";

/// v4: offline pins. One row per album or playlist the user keeps on this device. `ref_id` is an
/// album id or a playlist id, so it cannot be a foreign key: pins are dropped by
/// `pins::prune_pins` and when their playlist is deleted.
const SCHEMA_V4: &str = "
CREATE TABLE pins (
    kind       TEXT NOT NULL CHECK(kind IN ('album','playlist')),
    ref_id     INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(kind, ref_id)
);
";

/// v5: ReplayGain values from the tags, plus the flag that makes the next scan re-read old rows.
///
/// Rows written before v5 have no ReplayGain values, and nothing in their stored stamp
/// (size, etag) shows that they are stale. A NULL etag cannot serve as the trigger: a bucket
/// object always has an etag, so NULL differs and re-reads, but a local-folder object has no
/// etag (`None`), and `NULL` compared with `None` counts as unchanged, so it would never re-read.
/// The migration therefore sets `needs_reread = 1` on every existing row. The scan treats that
/// flag as a change for the key, and the upsert that stores the re-read track clears it. A row
/// whose re-read fails keeps the flag and is retried by the next scan.
const SCHEMA_V5: &str = "
ALTER TABLE tracks ADD COLUMN rg_track_gain REAL;
ALTER TABLE tracks ADD COLUMN rg_track_peak REAL;
ALTER TABLE tracks ADD COLUMN rg_album_gain REAL;
ALTER TABLE tracks ADD COLUMN rg_album_peak REAL;
ALTER TABLE tracks ADD COLUMN needs_reread INTEGER NOT NULL DEFAULT 0;
UPDATE tracks SET needs_reread = 1;
";

/// v6: sidecar `.lrc` lyrics, one row per track that has a sidecar object next to its audio file.
///
/// The row is keyed by the track's object key, like the user-data tables, so it survives re-reads
/// of the audio file. `lrc_key` names the `.lrc` object the text came from; `size` and `etag` are
/// its stamp, compared by the scan to decide whether to fetch it again. There is no foreign key:
/// the tables that hang off a track by key (`track_fts` aside) are cleaned up by hand, and
/// `Library::remove_tracks` deletes the row together with its track. `text` is empty when the
/// sidecar holds only whitespace.
const SCHEMA_V6: &str = "
CREATE TABLE lyrics_sidecars (
    track_key TEXT PRIMARY KEY,
    lrc_key   TEXT NOT NULL,
    size      INTEGER NOT NULL,
    etag      TEXT,
    text      TEXT NOT NULL
);
";

/// v7: no new columns. Before v7 every MP4 file was stored with codec "ALAC", so AAC `.m4a` files
/// counted as lossless (and as hi-res above 48 kHz). The scanner now tells them apart; this flags
/// the affected rows so the next scan re-reads them (see `needs_reread` in `SCHEMA_V5`).
const SCHEMA_V7: &str = "
UPDATE tracks SET needs_reread = 1 WHERE codec = 'ALAC';
";

const UPSERT_TRACK_SQL: &str = "
INSERT INTO tracks (key, size, etag, last_modified, title, artist, album_id, track_no, track_total,
    disc_no, disc_total, year, genre, duration_ms, sample_rate, bits_per_sample, channels,
    bitrate_kbps, codec, cover_id, lyrics, added_at, updated_at,
    rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak, needs_reread)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
    ?21, ?22, ?23, ?24, ?25, ?26, ?27, 0)
ON CONFLICT(key) DO UPDATE SET
    size = excluded.size,
    etag = excluded.etag,
    last_modified = excluded.last_modified,
    title = excluded.title,
    artist = excluded.artist,
    album_id = excluded.album_id,
    track_no = excluded.track_no,
    track_total = excluded.track_total,
    disc_no = excluded.disc_no,
    disc_total = excluded.disc_total,
    year = excluded.year,
    genre = excluded.genre,
    duration_ms = excluded.duration_ms,
    sample_rate = excluded.sample_rate,
    bits_per_sample = excluded.bits_per_sample,
    channels = excluded.channels,
    bitrate_kbps = excluded.bitrate_kbps,
    codec = excluded.codec,
    cover_id = excluded.cover_id,
    lyrics = excluded.lyrics,
    updated_at = excluded.updated_at,
    rg_track_gain = excluded.rg_track_gain,
    rg_track_peak = excluded.rg_track_peak,
    rg_album_gain = excluded.rg_album_gain,
    rg_album_peak = excluded.rg_album_peak,
    needs_reread = 0
RETURNING id
";

const UNKNOWN_ARTIST: &str = "Unknown Artist";
const UNKNOWN_ALBUM: &str = "Unknown Album";

/// The SQLite catalogue plus the directory that holds cover image files.
pub struct Library {
    conn: Mutex<Connection>,
    covers_dir: PathBuf,
}

impl Library {
    /// Opens (or creates) the database at `db_path`, creating `covers_dir` if needed.
    pub fn open(db_path: &Path, covers_dir: &Path) -> Result<Library> {
        fs::create_dir_all(covers_dir)?;
        let mut conn = Connection::open(db_path)?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        migrate(&mut conn)?;
        Ok(Library {
            conn: Mutex::new(conn),
            covers_dir: covers_dir.to_path_buf(),
        })
    }

    /// Opens a fresh in-memory database; covers are still written to `covers_dir`.
    pub fn open_in_memory(covers_dir: &Path) -> Result<Library> {
        fs::create_dir_all(covers_dir)?;
        let mut conn = Connection::open_in_memory()?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        migrate(&mut conn)?;
        Ok(Library {
            conn: Mutex::new(conn),
            covers_dir: covers_dir.to_path_buf(),
        })
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn covers_dir(&self) -> &Path {
        &self.covers_dir
    }

    /// All catalogued tracks keyed by object key, for the scan diff.
    pub fn stamps(&self) -> Result<HashMap<String, ObjectStamp>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT key, size, etag, last_modified FROM tracks")?;
        let rows = stmt.query_map([], |r| {
            let size: i64 = r.get(1)?;
            Ok(ObjectStamp {
                key: r.get(0)?,
                size: u64::try_from(size).unwrap_or(0),
                etag: r.get(2)?,
                last_modified: r.get(3)?,
            })
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let stamp = row?;
            out.insert(stamp.key.clone(), stamp);
        }
        Ok(out)
    }

    /// Keys of catalogued tracks flagged for a re-read by the next scan (see `SCHEMA_V5`).
    pub(crate) fn keys_needing_reread(&self) -> Result<HashSet<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT key FROM tracks WHERE needs_reread = 1")?;
        let keys = stmt.query_map([], |r| r.get(0))?;
        Ok(keys.collect::<rusqlite::Result<HashSet<String>>>()?)
    }

    /// Inserts or updates the track for `stamp.key` together with its album and artist.
    /// Returns the track id.
    pub fn upsert_track(
        &self,
        stamp: &ObjectStamp,
        meta: &TrackMeta,
        cover_id: Option<i64>,
    ) -> Result<i64> {
        let names = resolve(meta, &stamp.key);
        let now = now_secs();
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        let artist_id = get_or_create_artist(&tx, &names.album_artist)?;
        let album_id = get_or_create_album(&tx, &names.album, artist_id, now)?;
        tx.execute(
            "UPDATE albums SET year = COALESCE(year, ?1), cover_id = COALESCE(cover_id, ?2)
             WHERE id = ?3",
            params![meta.year.map(i64::from), cover_id, album_id],
        )?;

        let track_id: i64 = tx.query_row(
            UPSERT_TRACK_SQL,
            params![
                stamp.key,
                to_i64(stamp.size),
                stamp.etag,
                stamp.last_modified,
                names.title,
                names.artist,
                album_id,
                names.track_no.map(i64::from),
                meta.track_total.map(i64::from),
                names.disc_no.map(i64::from),
                meta.disc_total.map(i64::from),
                meta.year.map(i64::from),
                meta.genre,
                meta.duration_ms.map(to_i64),
                meta.sample_rate.map(i64::from),
                meta.bits_per_sample.map(i64::from),
                meta.channels.map(i64::from),
                meta.bitrate_kbps.map(i64::from),
                meta.codec,
                cover_id,
                meta.lyrics.as_deref().filter(|s| !s.is_empty()),
                now,
                now,
                meta.track_gain_db,
                meta.track_peak,
                meta.album_gain_db,
                meta.album_peak,
            ],
            |r| r.get(0),
        )?;

        tx.execute("DELETE FROM track_fts WHERE rowid = ?1", params![track_id])?;
        tx.execute(
            "INSERT INTO track_fts (rowid, title, artist, album, album_artist)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                track_id,
                names.title,
                names.artist,
                names.album,
                names.album_artist
            ],
        )?;
        tx.commit()?;
        Ok(track_id)
    }

    /// Stored sidecar rows keyed by track key (see `SCHEMA_V6`).
    pub(crate) fn sidecar_rows(&self) -> Result<HashMap<String, SidecarRow>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT track_key, lrc_key, size, etag FROM lyrics_sidecars")?;
        let rows = stmt.query_map([], |r| {
            let size: i64 = r.get(2)?;
            Ok((
                r.get::<_, String>(0)?,
                SidecarRow {
                    lrc_key: r.get(1)?,
                    size: u64::try_from(size).unwrap_or(0),
                    etag: r.get(3)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }

    /// Stores (or replaces) the sidecar row of `track_key`. `text` is the decoded `.lrc` text.
    pub(crate) fn upsert_sidecar(
        &self,
        track_key: &str,
        lrc_key: &str,
        size: u64,
        etag: Option<&str>,
        text: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO lyrics_sidecars (track_key, lrc_key, size, etag, text)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(track_key) DO UPDATE SET
                lrc_key = excluded.lrc_key,
                size = excluded.size,
                etag = excluded.etag,
                text = excluded.text",
            params![track_key, lrc_key, to_i64(size), etag, text],
        )?;
        Ok(())
    }

    /// Deletes the sidecar row of `track_key`, if there is one.
    pub(crate) fn remove_sidecar(&self, track_key: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM lyrics_sidecars WHERE track_key = ?1",
            params![track_key],
        )?;
        Ok(())
    }

    /// Deletes the tracks (and their search and sidecar rows) for `keys`. Returns how many
    /// existed.
    pub fn remove_tracks(&self, keys: &[String]) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let mut removed = 0;
        for key in keys {
            tx.execute(
                "DELETE FROM lyrics_sidecars WHERE track_key = ?1",
                params![key],
            )?;
            let id: Option<i64> = tx
                .query_row("SELECT id FROM tracks WHERE key = ?1", params![key], |r| {
                    r.get(0)
                })
                .optional()?;
            if let Some(id) = id {
                tx.execute("DELETE FROM track_fts WHERE rowid = ?1", params![id])?;
                tx.execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
                removed += 1;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Stores cover bytes content-addressed by BLAKE3 hash. Identical images share one row.
    pub fn store_cover(&self, data: &[u8], mime: &str) -> Result<i64> {
        let hash = blake3::hash(data).to_hex().to_string();
        let conn = self.lock();
        let existing: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, file FROM covers WHERE hash = ?1",
                params![hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let file = format!("{hash}.{}", cover_ext(mime));
        if let Some((id, existing_file)) = existing {
            // Re-create the file if it vanished (e.g. the covers folder was cleared).
            let path = self.covers_dir.join(&existing_file);
            if !path.is_file() {
                write_atomic(&self.covers_dir, &existing_file, data)?;
            }
            return Ok(id);
        }

        write_atomic(&self.covers_dir, &file, data)?;

        let id: i64 = conn.query_row(
            "INSERT INTO covers (hash, mime, file, bytes) VALUES (?1, ?2, ?3, ?4) RETURNING id",
            params![hash, mime, file, to_i64(len_u64(data.len()))],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    /// Removes albums without tracks, artists without albums, and covers referenced by
    /// nothing (deleting their files; a missing file is not an error).
    pub fn prune(&self) -> Result<PruneStats> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        let albums = tx.execute(
            "DELETE FROM albums WHERE id NOT IN (SELECT album_id FROM tracks)",
            [],
        )?;
        let artists = tx.execute(
            "DELETE FROM artists WHERE id NOT IN (SELECT artist_id FROM albums)",
            [],
        )?;

        let orphans: Vec<(i64, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, file FROM covers
                 WHERE id NOT IN (SELECT cover_id FROM tracks WHERE cover_id IS NOT NULL)
                   AND id NOT IN (SELECT cover_id FROM albums WHERE cover_id IS NOT NULL)",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, _) in &orphans {
            tx.execute("DELETE FROM covers WHERE id = ?1", params![id])?;
        }
        tx.commit()?;

        for (_, file) in &orphans {
            match fs::remove_file(self.covers_dir.join(file)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => log::warn!("could not remove cover file {file}: {e}"),
            }
        }

        Ok(PruneStats {
            albums: len_u64(albums),
            artists: len_u64(artists),
            covers: len_u64(orphans.len()),
        })
    }
}

/// What a stored sidecar row remembers about its `.lrc` object (see `SCHEMA_V6`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidecarRow {
    /// Object key of the `.lrc` the text was read from.
    pub lrc_key: String,
    /// Size of that object when it was read.
    pub size: u64,
    /// Etag of that object when it was read; `None` for a local folder.
    pub etag: Option<String>,
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(LibraryError::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!(
                "library schema version {version} is newer than supported version {SCHEMA_VERSION}"
            )),
        )));
    }
    for (target, sql) in [
        (1, SCHEMA_V1),
        (2, SCHEMA_V2),
        (3, SCHEMA_V3),
        (4, SCHEMA_V4),
        (5, SCHEMA_V5),
        (6, SCHEMA_V6),
        (7, SCHEMA_V7),
    ] {
        if version < target {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", target)?;
            tx.commit()?;
        }
    }
    Ok(())
}

fn get_or_create_artist(conn: &Connection, name: &str) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO artists (name) VALUES (?1)",
        params![name],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM artists WHERE name = ?1",
        params![name],
        |r| r.get(0),
    )?;
    Ok(id)
}

fn get_or_create_album(conn: &Connection, title: &str, artist_id: i64, now: i64) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO albums (title, artist_id, added_at) VALUES (?1, ?2, ?3)",
        params![title, artist_id, now],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM albums WHERE title = ?1 AND artist_id = ?2",
        params![title, artist_id],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Writes `data` to `dir/file` via a temporary file and rename.
fn write_atomic(dir: &Path, file: &str, data: &[u8]) -> Result<()> {
    let tmp_path = dir.join(format!("{file}.tmp"));
    fs::write(&tmp_path, data)?;
    fs::rename(&tmp_path, dir.join(file))?;
    Ok(())
}

fn cover_ext(mime: &str) -> &'static str {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        _ => "bin",
    }
}

/// Current Unix time in seconds; 0 if the clock is before the epoch.
pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Current Unix time in milliseconds; 0 if the clock is before the epoch.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// A new `updated_at` for a row last stamped `prev`: now, but always strictly greater than
/// `prev`, so edits within the same millisecond (or after a clock step back) still win LWW.
pub(crate) fn next_stamp(prev: i64) -> i64 {
    now_ms().max(prev.saturating_add(1))
}

/// Stores a u64 as SQLite INTEGER, saturating at `i64::MAX`.
pub(crate) fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn len_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Reads a nullable INTEGER column into a narrower integer type; out-of-range values become None.
pub(crate) fn opt_conv<T: TryFrom<i64>>(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<T>> {
    Ok(row
        .get::<_, Option<i64>>(idx)?
        .and_then(|v| T::try_from(v).ok()))
}

/// Resolved catalogue values for one track, derived from its tags and object key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
}

/// Applies the naming rules: tags first, then the file name / folder name, then placeholders.
pub(crate) fn resolve(meta: &TrackMeta, key: &str) -> Resolved {
    let stem = file_stem(key);
    let (file_disc, file_track, file_title) = match parse_leading_number(stem) {
        Some((disc, track, rest)) => (disc, Some(track), if rest.is_empty() { stem } else { rest }),
        None => (None, None, stem),
    };

    let title = clean(meta.title.as_deref());
    let artist = clean(meta.artist.as_deref());
    let album_artist = clean(meta.album_artist.as_deref());
    // `Artist/Album/track.ext` layout: the grandparent folder names the artist.
    let folder_artist = grandparent_folder(key);

    Resolved {
        title: title.unwrap_or(file_title).to_owned(),
        artist: artist
            .or(album_artist)
            .or(folder_artist)
            .unwrap_or(UNKNOWN_ARTIST)
            .to_owned(),
        album_artist: album_artist
            .or(artist)
            .or(folder_artist)
            .unwrap_or(UNKNOWN_ARTIST)
            .to_owned(),
        album: clean(meta.album.as_deref())
            .or_else(|| parent_folder(key))
            .unwrap_or(UNKNOWN_ALBUM)
            .to_owned(),
        track_no: meta.track_no.or(file_track),
        disc_no: meta.disc_no.or(file_disc),
    }
}

/// Trims and drops empty strings.
fn clean(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// File name without its extension (the extension is the part after the last dot, if any
/// and not a leading dot).
fn file_stem(key: &str) -> &str {
    let name = key.rsplit('/').next().unwrap_or(key);
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

/// Name of the folder directly containing the object, if any.
fn parent_folder(key: &str) -> Option<&str> {
    let mut parts = key.rsplit('/');
    parts.next()?;
    parts.next().filter(|s| !s.is_empty())
}

/// Name of the folder two levels above the object (`A` in `A/B/x.flac`), if any.
fn grandparent_folder(key: &str) -> Option<&str> {
    let mut parts = key.rsplit('/');
    parts.next()?;
    parts.next()?;
    parts.next().map(str::trim).filter(|s| !s.is_empty())
}

fn leading_digits(s: &str) -> usize {
    s.bytes().take_while(u8::is_ascii_digit).count()
}

/// Splits a leading track-number prefix off a file stem: `"01 - x"`, `"01. x"` and `"01 x"`
/// give track 1; `"1-02 x"` gives disc 1, track 2. Returns (disc, track, remaining title),
/// where the remaining title may be empty. `None` if there is no such prefix.
fn parse_leading_number(stem: &str) -> Option<(Option<u32>, u32, &str)> {
    let a_len = leading_digits(stem);
    if a_len == 0 {
        return None;
    }
    let (a, rest) = stem.split_at(a_len);
    let (disc, track, after) = match rest.strip_prefix('-') {
        Some(r) if leading_digits(r) > 0 => {
            let (b, after) = r.split_at(leading_digits(r));
            (Some(a.parse::<u32>().ok()?), b.parse::<u32>().ok()?, after)
        }
        _ => (None, a.parse::<u32>().ok()?, rest),
    };
    match after.chars().next() {
        Some(' ' | '.' | '-') => Some((
            disc,
            track,
            after.trim_start_matches([' ', '.', '-']).trim(),
        )),
        _ => None,
    }
}

/// Shared helpers for unit tests in this crate.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::model::{ObjectStamp, TrackMeta};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique directory under the system temp dir, removed on drop.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> TempDir {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "sonorus-lib-test-{}-{}-{}",
                std::process::id(),
                nanos,
                n
            ));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub fn stamp(key: &str, size: u64) -> ObjectStamp {
        ObjectStamp {
            key: key.to_owned(),
            size,
            etag: Some(format!("etag-{size}")),
            last_modified: Some(1_700_000_000),
        }
    }

    pub fn meta(title: &str, artist: &str, album: &str) -> TrackMeta {
        TrackMeta {
            title: Some(title.to_owned()),
            artist: Some(artist.to_owned()),
            album: Some(album.to_owned()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{TempDir, meta, stamp};
    use super::*;
    use crate::scan::{DirSource, MetaSource, scan};
    use std::sync::Arc;

    #[test]
    fn leading_number_forms() {
        assert_eq!(
            parse_leading_number("01 - First Song"),
            Some((None, 1, "First Song"))
        );
        assert_eq!(parse_leading_number("01. x"), Some((None, 1, "x")));
        assert_eq!(parse_leading_number("01 x"), Some((None, 1, "x")));
        assert_eq!(parse_leading_number("1-02 x"), Some((Some(1), 2, "x")));
        assert_eq!(parse_leading_number("01 -"), Some((None, 1, "")));
        assert_eq!(parse_leading_number("Plain"), None);
        assert_eq!(parse_leading_number("01"), None);
        assert_eq!(parse_leading_number("1-02"), None);
    }

    #[test]
    fn title_and_numbers_from_file_name() {
        let r = resolve(
            &TrackMeta::default(),
            "Artist A/Album One/01 - First Song.flac",
        );
        assert_eq!(r.title, "First Song");
        assert_eq!(r.track_no, Some(1));
        assert_eq!(r.disc_no, None);

        let r = resolve(&TrackMeta::default(), "x/y/1-02 Second.m4a");
        assert_eq!(r.title, "Second");
        assert_eq!(r.disc_no, Some(1));
        assert_eq!(r.track_no, Some(2));

        let r = resolve(&TrackMeta::default(), "Plain Name.wav");
        assert_eq!(r.title, "Plain Name");
        assert_eq!(r.track_no, None);
    }

    #[test]
    fn tags_win_over_file_name() {
        let m = TrackMeta {
            title: Some("  Tagged  ".into()),
            track_no: Some(9),
            disc_no: Some(2),
            ..Default::default()
        };
        let r = resolve(&m, "Artist/Album/01 - File.flac");
        assert_eq!(r.title, "Tagged");
        assert_eq!(r.track_no, Some(9));
        assert_eq!(r.disc_no, Some(2));
    }

    #[test]
    fn empty_title_tag_falls_back() {
        let m = TrackMeta {
            title: Some("   ".into()),
            ..Default::default()
        };
        assert_eq!(resolve(&m, "a/b/03 - Song.flac").title, "Song");
    }

    #[test]
    fn artist_and_album_artist_fallbacks() {
        let only_album_artist = TrackMeta {
            album_artist: Some("AA".into()),
            ..Default::default()
        };
        let r = resolve(&only_album_artist, "a/b/x.flac");
        assert_eq!(r.artist, "AA");
        assert_eq!(r.album_artist, "AA");

        let only_artist = TrackMeta {
            artist: Some("Feat Artist".into()),
            ..Default::default()
        };
        let r = resolve(&only_artist, "a/b/x.flac");
        assert_eq!(r.artist, "Feat Artist");
        assert_eq!(r.album_artist, "Feat Artist");

        let both = TrackMeta {
            artist: Some("Feat Artist".into()),
            album_artist: Some("Main".into()),
            ..Default::default()
        };
        let r = resolve(&both, "a/b/x.flac");
        assert_eq!(r.artist, "Feat Artist");
        assert_eq!(r.album_artist, "Main");

        let r = resolve(&TrackMeta::default(), "x.flac");
        assert_eq!(r.artist, UNKNOWN_ARTIST);
        assert_eq!(r.album_artist, UNKNOWN_ARTIST);
        let r = resolve(&TrackMeta::default(), "Album/x.flac");
        assert_eq!(r.artist, UNKNOWN_ARTIST);

        // Untagged file in an Artist/Album/ layout takes the artist from the folder.
        let r = resolve(
            &TrackMeta::default(),
            "music/Artist B/Album Two/03 - x.flac",
        );
        assert_eq!(r.artist, "Artist B");
        assert_eq!(r.album_artist, "Artist B");
        assert_eq!(r.album, "Album Two");
        // Tags still win over the folder.
        let r = resolve(&only_artist, "Folder Artist/Album/x.flac");
        assert_eq!(r.album_artist, "Feat Artist");
    }

    #[test]
    fn album_from_tag_or_folder() {
        let m = TrackMeta {
            album: Some("Tagged Album".into()),
            ..Default::default()
        };
        assert_eq!(resolve(&m, "Artist/Folder/x.flac").album, "Tagged Album");
        assert_eq!(
            resolve(&TrackMeta::default(), "Artist/Folder/x.flac").album,
            "Folder"
        );
        assert_eq!(
            resolve(&TrackMeta::default(), "x.flac").album,
            UNKNOWN_ALBUM
        );
    }

    #[test]
    fn upsert_groups_album_case_insensitively() {
        let dir = TempDir::new();
        let lib = Library::open_in_memory(&dir.path().join("covers")).unwrap();
        lib.upsert_track(
            &stamp("Artist A/Album One/01 - a.flac", 10),
            &meta("a", "Artist A", "Album One"),
            None,
        )
        .unwrap();
        lib.upsert_track(
            &stamp("Artist A/Album One/02 - b.flac", 10),
            &meta("b", "Artist A", "ALBUM one"),
            None,
        )
        .unwrap();
        let conn = lib.lock();
        let albums: i64 = conn
            .query_row("SELECT COUNT(*) FROM albums", [], |r| r.get(0))
            .unwrap();
        let artists: i64 = conn
            .query_row("SELECT COUNT(*) FROM artists", [], |r| r.get(0))
            .unwrap();
        let tracks: i64 = conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .unwrap();
        assert_eq!((albums, artists, tracks), (1, 1, 2));
    }

    #[test]
    fn update_keeps_id_and_added_at() {
        let dir = TempDir::new();
        let lib = Library::open_in_memory(&dir.path().join("covers")).unwrap();
        let s = stamp("A/B/01 - x.flac", 10);
        let id = lib.upsert_track(&s, &meta("x", "A", "B"), None).unwrap();
        lib.lock()
            .execute(
                "UPDATE tracks SET added_at = 1000 WHERE id = ?1",
                params![id],
            )
            .unwrap();

        let s2 = stamp("A/B/01 - x.flac", 99);
        let id2 = lib
            .upsert_track(&s2, &meta("renamed", "A", "B"), None)
            .unwrap();
        assert_eq!(id, id2);

        let (added, updated, size, title): (i64, i64, i64, String) = lib
            .lock()
            .query_row(
                "SELECT added_at, updated_at, size, title FROM tracks WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(added, 1000);
        assert!(updated >= now_secs() - 5);
        assert_eq!(size, 99);
        assert_eq!(title, "renamed");
    }

    #[test]
    fn store_cover_dedupes_and_names_by_mime() {
        let dir = TempDir::new();
        let covers = dir.path().join("covers");
        let lib = Library::open_in_memory(&covers).unwrap();
        let a = lib.store_cover(b"jpeg-bytes", "image/jpeg").unwrap();
        let b = lib.store_cover(b"jpeg-bytes", "image/jpeg").unwrap();
        assert_eq!(a, b);
        let hash = blake3::hash(b"jpeg-bytes").to_hex().to_string();
        assert!(covers.join(format!("{hash}.jpg")).is_file());
        assert!(!covers.join(format!("{hash}.jpg.tmp")).exists());

        let c = lib.store_cover(b"png-bytes", "image/png").unwrap();
        assert_ne!(a, c);
        let hash_png = blake3::hash(b"png-bytes").to_hex().to_string();
        assert!(covers.join(format!("{hash_png}.png")).is_file());

        let count: i64 = lib
            .lock()
            .query_row("SELECT COUNT(*) FROM covers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);

        std::fs::remove_file(covers.join(format!("{hash}.jpg"))).unwrap();
        assert_eq!(lib.store_cover(b"jpeg-bytes", "image/jpeg").unwrap(), a);
        assert!(
            covers.join(format!("{hash}.jpg")).is_file(),
            "missing file re-created"
        );
    }

    #[test]
    fn remove_then_prune_cleans_orphans_and_cover_files() {
        let dir = TempDir::new();
        let covers = dir.path().join("covers");
        let lib = Library::open_in_memory(&covers).unwrap();

        let c1 = lib.store_cover(b"cover-one", "image/jpeg").unwrap();
        let c2 = lib.store_cover(b"cover-two", "image/jpeg").unwrap();
        lib.upsert_track(
            &stamp("Art X/Alb X/01 - a.flac", 1),
            &meta("a", "Art X", "Alb X"),
            Some(c1),
        )
        .unwrap();
        lib.upsert_track(
            &stamp("Art Y/Alb Y/01 - b.flac", 1),
            &meta("b", "Art Y", "Alb Y"),
            Some(c2),
        )
        .unwrap();

        let removed = lib
            .remove_tracks(&["Art X/Alb X/01 - a.flac".to_owned()])
            .unwrap();
        assert_eq!(removed, 1);
        assert_eq!(lib.remove_tracks(&["nope".to_owned()]).unwrap(), 0);

        let stats = lib.prune().unwrap();
        assert_eq!(
            stats,
            PruneStats {
                albums: 1,
                artists: 1,
                covers: 1
            }
        );
        let hash1 = blake3::hash(b"cover-one").to_hex().to_string();
        let hash2 = blake3::hash(b"cover-two").to_hex().to_string();
        assert!(!covers.join(format!("{hash1}.jpg")).exists());
        assert!(covers.join(format!("{hash2}.jpg")).is_file());

        assert_eq!(lib.prune().unwrap(), PruneStats::default());
    }

    #[test]
    fn prune_tolerates_missing_cover_file() {
        let dir = TempDir::new();
        let covers = dir.path().join("covers");
        let lib = Library::open_in_memory(&covers).unwrap();
        lib.store_cover(b"gone", "image/png").unwrap();
        let hash = blake3::hash(b"gone").to_hex().to_string();
        std::fs::remove_file(covers.join(format!("{hash}.png"))).unwrap();
        let stats = lib.prune().unwrap();
        assert_eq!(stats.covers, 1);
    }

    #[test]
    fn reopen_migrates_once_and_rejects_newer_schema() {
        let dir = TempDir::new();
        let db = dir.path().join("lib.db");
        let covers = dir.path().join("covers");
        {
            let lib = Library::open(&db, &covers).unwrap();
            lib.upsert_track(&stamp("a/b/x.flac", 1), &meta("x", "a", "b"), None)
                .unwrap();
        }
        let lib = Library::open(&db, &covers).unwrap();
        assert_eq!(lib.stamps().unwrap().len(), 1);
        drop(lib);

        {
            let conn = Connection::open(&db).unwrap();
            conn.pragma_update(None, "user_version", 99).unwrap();
        }
        assert!(Library::open(&db, &covers).is_err());
    }

    #[test]
    fn v1_database_upgrades_to_latest() {
        let dir = TempDir::new();
        let db = dir.path().join("v1.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
        }
        let lib = Library::open(&db, &dir.path().join("covers")).unwrap();
        let conn = lib.lock();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM playlists", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM uploads", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn v2_database_upgrades_to_v3_keeping_its_rows() {
        let dir = TempDir::new();
        let db = dir.path().join("v2.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
            conn.execute(
                "INSERT INTO favorites (track_key, favorite, updated_at) VALUES ('a/b.flac', 1, 5)",
                [],
            )
            .unwrap();
        }
        let lib = Library::open(&db, &dir.path().join("covers")).unwrap();
        let conn = lib.lock();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let favorites: i64 = conn
            .query_row("SELECT COUNT(*) FROM favorites", [], |r| r.get(0))
            .unwrap();
        assert_eq!(favorites, 1);
        let uploads: i64 = conn
            .query_row("SELECT COUNT(*) FROM uploads", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uploads, 0);
    }

    #[test]
    fn v3_database_upgrades_to_v4_with_empty_pins() {
        let dir = TempDir::new();
        let db = dir.path().join("v3.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            conn.execute_batch(SCHEMA_V3).unwrap();
            conn.pragma_update(None, "user_version", 3).unwrap();
            conn.execute(
                "INSERT INTO favorites (track_key, favorite, updated_at) VALUES ('a/b.flac', 1, 5)",
                [],
            )
            .unwrap();
        }
        let lib = Library::open(&db, &dir.path().join("covers")).unwrap();
        let conn = lib.lock();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let favorites: i64 = conn
            .query_row("SELECT COUNT(*) FROM favorites", [], |r| r.get(0))
            .unwrap();
        assert_eq!(favorites, 1);
        let pins: i64 = conn
            .query_row("SELECT COUNT(*) FROM pins", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pins, 0);
    }

    #[test]
    fn v4_database_upgrades_to_v5_flagging_existing_rows() {
        let dir = TempDir::new();
        let db = dir.path().join("v4.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            conn.execute_batch(SCHEMA_V3).unwrap();
            conn.execute_batch(SCHEMA_V4).unwrap();
            conn.pragma_update(None, "user_version", 4).unwrap();
            conn.execute_batch(
                "INSERT INTO artists (name) VALUES ('A');
                 INSERT INTO albums (title, artist_id, added_at) VALUES ('B', 1, 0);
                 INSERT INTO tracks (key, size, etag, title, artist, album_id, added_at, updated_at)
                    VALUES ('a/b.flac', 5, 'etag-1', 't', 'A', 1, 0, 0);",
            )
            .unwrap();
        }
        let lib = Library::open(&db, &dir.path().join("covers")).unwrap();
        {
            let conn = lib.lock();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, SCHEMA_VERSION);
            let (gain, peak, flag): (Option<f64>, Option<f64>, i64) = conn
                .query_row(
                    "SELECT rg_track_gain, rg_track_peak, needs_reread FROM tracks WHERE key = 'a/b.flac'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!((gain, peak, flag), (None, None, 1));
        }
        assert!(lib.keys_needing_reread().unwrap().contains("a/b.flac"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn v4_rows_are_reread_by_the_next_scan() {
        let fixtures = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"));
        let key = "Artist A/Album One/01 - First Song.flac";
        let size = std::fs::metadata(fixtures.join(key)).unwrap().len();
        let dir = TempDir::new();
        let db = dir.path().join("v4.db");
        {
            // A local-folder row: its etag is NULL, which equals the local source's `None`, so
            // only the migration flag can make the scan read it again.
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            conn.execute_batch(SCHEMA_V3).unwrap();
            conn.execute_batch(SCHEMA_V4).unwrap();
            conn.pragma_update(None, "user_version", 4).unwrap();
            conn.execute_batch(
                "INSERT INTO artists (name) VALUES ('Artist A');
                 INSERT INTO albums (title, artist_id, added_at) VALUES ('Album One', 1, 0);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tracks (key, size, etag, title, artist, album_id, added_at, updated_at)
                 VALUES (?1, ?2, NULL, 'First Song', 'Artist A', 1, 0, 0)",
                params![key, size as i64],
            )
            .unwrap();
        }
        let lib = Library::open(&db, &dir.path().join("covers")).unwrap();
        let source: Arc<dyn MetaSource> = Arc::new(DirSource::new(&fixtures));

        let report = scan(&lib, Arc::clone(&source), "", 2, |_| {})
            .await
            .unwrap();
        assert_eq!(report.progress.updated, 1, "migrated row is re-read");
        assert_eq!(report.progress.added, 3);
        assert!(lib.keys_needing_reread().unwrap().is_empty());

        let report = scan(&lib, source, "", 2, |_| {}).await.unwrap();
        assert_eq!(
            report.progress.total, 0,
            "flag is cleared after the re-read"
        );
    }

    #[test]
    fn v6_database_upgrades_to_v7_flagging_mp4_rows() {
        let dir = TempDir::new();
        let db = dir.path().join("v6.db");
        {
            let conn = Connection::open(&db).unwrap();
            for sql in [
                SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6,
            ] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", 6).unwrap();
            conn.execute_batch(
                "INSERT INTO artists (name) VALUES ('A');
                 INSERT INTO albums (title, artist_id, added_at) VALUES ('B', 1, 0);
                 INSERT INTO tracks (key, size, title, artist, album_id, codec, added_at, updated_at)
                    VALUES ('a/b/song.m4a', 5, 't', 'A', 1, 'ALAC', 0, 0),
                           ('a/b/other.flac', 5, 't', 'A', 1, 'FLAC', 0, 0);",
            )
            .unwrap();
        }
        let lib = Library::open(&db, &dir.path().join("covers")).unwrap();
        let conn = lib.lock();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let flagged: Vec<String> = conn
            .prepare("SELECT key FROM tracks WHERE needs_reread = 1 ORDER BY key")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(flagged, vec!["a/b/song.m4a".to_owned()]);
    }

    #[test]
    fn v5_database_upgrades_to_v6_with_no_sidecars() {
        let dir = TempDir::new();
        let db = dir.path().join("v5.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            conn.execute_batch(SCHEMA_V3).unwrap();
            conn.execute_batch(SCHEMA_V4).unwrap();
            conn.execute_batch(SCHEMA_V5).unwrap();
            conn.pragma_update(None, "user_version", 5).unwrap();
            conn.execute_batch(
                "INSERT INTO artists (name) VALUES ('A');
                 INSERT INTO albums (title, artist_id, added_at) VALUES ('B', 1, 0);
                 INSERT INTO tracks (key, size, etag, title, artist, album_id, added_at, updated_at)
                    VALUES ('a/b/c.flac', 5, 'etag-1', 't', 'A', 1, 0, 0);",
            )
            .unwrap();
        }
        let lib = Library::open(&db, &dir.path().join("covers")).unwrap();
        {
            let conn = lib.lock();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, SCHEMA_VERSION);
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM lyrics_sidecars", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0);
        }
        assert!(lib.sidecar_rows().unwrap().is_empty());

        lib.upsert_sidecar("a/b/c.flac", "a/b/c.lrc", 9, Some("etag-1"), "[00:01.00]x")
            .unwrap();
        assert_eq!(
            lib.sidecar_rows().unwrap().get("a/b/c.flac"),
            Some(&SidecarRow {
                lrc_key: "a/b/c.lrc".into(),
                size: 9,
                etag: Some("etag-1".into()),
            })
        );
        assert_eq!(lib.remove_tracks(&["a/b/c.flac".to_owned()]).unwrap(), 1);
        assert!(
            lib.sidecar_rows().unwrap().is_empty(),
            "removing a track removes its sidecar row"
        );
    }

    #[test]
    fn next_stamp_is_strictly_increasing() {
        let far_future = now_ms() + 1_000_000;
        assert_eq!(next_stamp(far_future), far_future + 1);
        assert!(next_stamp(0) >= now_ms() - 1000);
    }
}
