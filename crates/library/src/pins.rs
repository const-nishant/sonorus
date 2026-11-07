//! Offline pins: the albums and playlists the user keeps on this device. The core downloads the
//! tracks they cover into its disk cache and keeps them there.
//!
//! `pins.ref_id` cannot be a foreign key (it names an album or a playlist), so pins to removed
//! albums or deleted playlists are removed by [`prune_pins`] and by `delete_playlist`.

use std::collections::HashSet;

use rusqlite::{Connection, params};

use crate::db::{Library, now_ms};
use crate::error::{LibraryError, Result};

/// A track the cache can hold: `(track id, object key, size in bytes)`.
pub type PinnedTrack = (i64, String, u64);

/// What a pin refers to. Stored as [`PinKind::as_str`] in the `pins.kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinKind {
    /// Every track of an album.
    Album,
    /// Every catalogued entry of a playlist.
    Playlist,
}

impl PinKind {
    /// The text stored in `pins.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            PinKind::Album => "album",
            PinKind::Playlist => "playlist",
        }
    }

    fn parse(s: &str) -> Option<PinKind> {
        match s {
            "album" => Some(PinKind::Album),
            "playlist" => Some(PinKind::Playlist),
            _ => None,
        }
    }
}

/// One pin row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pin {
    /// Album or playlist.
    pub kind: PinKind,
    /// Album id or playlist id, depending on `kind`.
    pub ref_id: i64,
    /// Unix milliseconds when the pin was made. Pins are downloaded in this order.
    pub created_at: i64,
}

/// Pins an album or playlist. Pinning an existing pin changes nothing (its `created_at` stays).
/// NotFound when the album does not exist, or the playlist is missing or deleted.
pub fn pin(lib: &Library, kind: PinKind, ref_id: i64) -> Result<()> {
    let conn = lib.lock();
    if !target_exists(&conn, kind, ref_id)? {
        return Err(LibraryError::NotFound(format!(
            "{} {ref_id}",
            kind.as_str()
        )));
    }
    conn.execute(
        "INSERT OR IGNORE INTO pins (kind, ref_id, created_at) VALUES (?1, ?2, ?3)",
        params![kind.as_str(), ref_id, now_ms()],
    )?;
    Ok(())
}

/// Removes a pin. Removing a pin that does not exist changes nothing.
pub fn unpin(lib: &Library, kind: PinKind, ref_id: i64) -> Result<()> {
    let conn = lib.lock();
    conn.execute(
        "DELETE FROM pins WHERE kind = ?1 AND ref_id = ?2",
        params![kind.as_str(), ref_id],
    )?;
    Ok(())
}

/// Whether the album or playlist has a pin.
pub fn is_pinned(lib: &Library, kind: PinKind, ref_id: i64) -> Result<bool> {
    let conn = lib.lock();
    let pinned: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pins WHERE kind = ?1 AND ref_id = ?2)",
        params![kind.as_str(), ref_id],
        |r| r.get(0),
    )?;
    Ok(pinned)
}

/// All pins, oldest first (then by kind and id).
pub fn list(lib: &Library) -> Result<Vec<Pin>> {
    let conn = lib.lock();
    let mut stmt = conn
        .prepare("SELECT kind, ref_id, created_at FROM pins ORDER BY created_at, kind, ref_id")?;
    let rows = stmt.query_map([], |r| {
        let kind: String = r.get(0)?;
        Ok((kind, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (kind, ref_id, created_at) = row?;
        // The CHECK constraint allows only the two known kinds.
        if let Some(kind) = PinKind::parse(&kind) {
            out.push(Pin {
                kind,
                ref_id,
                created_at,
            });
        }
    }
    Ok(out)
}

/// The distinct tracks one album or playlist covers, in album order or playlist order. A
/// playlist entry whose track is not catalogued, and a deleted playlist, contribute nothing.
pub fn tracks_of(lib: &Library, kind: PinKind, ref_id: i64) -> Result<Vec<PinnedTrack>> {
    let conn = lib.lock();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    collect_tracks(&conn, kind, ref_id, &mut seen, &mut out)?;
    Ok(out)
}

/// The distinct tracks covered by all pins, in pin order (oldest pin first). A track covered by
/// several pins appears once, at its first position.
pub fn pinned_tracks(lib: &Library) -> Result<Vec<PinnedTrack>> {
    let conn = lib.lock();
    let mut stmt =
        conn.prepare("SELECT kind, ref_id FROM pins ORDER BY created_at, kind, ref_id")?;
    let pins = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (kind, ref_id) in pins {
        if let Some(kind) = PinKind::parse(&kind) {
            collect_tracks(&conn, kind, ref_id, &mut seen, &mut out)?;
        }
    }
    Ok(out)
}

/// Deletes pins whose album no longer exists or whose playlist is missing or deleted. Returns
/// how many pins were removed.
pub fn prune_pins(lib: &Library) -> Result<u32> {
    let conn = lib.lock();
    let removed = conn.execute(
        "DELETE FROM pins WHERE
            (kind = 'album' AND ref_id NOT IN (SELECT id FROM albums))
         OR (kind = 'playlist' AND ref_id NOT IN (SELECT id FROM playlists WHERE deleted = 0))",
        [],
    )?;
    Ok(u32::try_from(removed).unwrap_or(u32::MAX))
}

/// Whether the album exists, or the playlist is live.
fn target_exists(conn: &Connection, kind: PinKind, ref_id: i64) -> Result<bool> {
    let sql = match kind {
        PinKind::Album => "SELECT EXISTS(SELECT 1 FROM albums WHERE id = ?1)",
        PinKind::Playlist => "SELECT EXISTS(SELECT 1 FROM playlists WHERE id = ?1 AND deleted = 0)",
    };
    let exists: bool = conn.query_row(sql, params![ref_id], |r| r.get(0))?;
    Ok(exists)
}

/// Appends the tracks of one pin to `out`, skipping ids already in `seen`.
fn collect_tracks(
    conn: &Connection,
    kind: PinKind,
    ref_id: i64,
    seen: &mut HashSet<i64>,
    out: &mut Vec<PinnedTrack>,
) -> Result<()> {
    let sql = match kind {
        PinKind::Album => {
            "SELECT id, key, size FROM tracks WHERE album_id = ?1
             ORDER BY disc_no NULLS LAST, track_no NULLS LAST, title"
        }
        PinKind::Playlist => {
            "SELECT t.id, t.key, t.size FROM playlist_items pi
             JOIN playlists p ON p.id = pi.playlist_id AND p.deleted = 0
             JOIN tracks t ON t.key = pi.track_key
             WHERE pi.playlist_id = ?1
             ORDER BY pi.position"
        }
    };
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(params![ref_id], |r| {
        let size: i64 = r.get(2)?;
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            u64::try_from(size).unwrap_or(0),
        ))
    })?;
    for row in rows {
        let track = row?;
        if seen.insert(track.0) {
            out.push(track);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{TempDir, meta, stamp};

    fn library(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).unwrap()
    }

    /// Catalogues a track of `album` by `Artist` and returns its id.
    fn add_track(lib: &Library, key: &str, album: &str) -> i64 {
        lib.upsert_track(&stamp(key, 100), &meta("t", "Artist", album), None)
            .unwrap()
    }

    /// Sets the pin timestamps so that the order is known.
    fn set_created(lib: &Library, kind: PinKind, ref_id: i64, at: i64) {
        lib.lock()
            .execute(
                "UPDATE pins SET created_at = ?3 WHERE kind = ?1 AND ref_id = ?2",
                params![kind.as_str(), ref_id, at],
            )
            .unwrap();
    }

    #[test]
    fn pin_unpin_round_trip_keeps_first_timestamp() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let t = add_track(&lib, "Artist/A/01 - One.flac", "A");
        let album = lib.track(t).unwrap().album_id;

        assert!(!is_pinned(&lib, PinKind::Album, album).unwrap());
        pin(&lib, PinKind::Album, album).unwrap();
        assert!(is_pinned(&lib, PinKind::Album, album).unwrap());
        set_created(&lib, PinKind::Album, album, 42);
        pin(&lib, PinKind::Album, album).unwrap();
        let pins = list(&lib).unwrap();
        assert_eq!(
            pins,
            vec![Pin {
                kind: PinKind::Album,
                ref_id: album,
                created_at: 42
            }]
        );

        unpin(&lib, PinKind::Album, album).unwrap();
        unpin(&lib, PinKind::Album, album).unwrap();
        assert!(!is_pinned(&lib, PinKind::Album, album).unwrap());
        assert!(list(&lib).unwrap().is_empty());
    }

    #[test]
    fn pinning_missing_or_deleted_targets_is_not_found() {
        let dir = TempDir::new();
        let lib = library(&dir);
        assert!(matches!(
            pin(&lib, PinKind::Album, 999),
            Err(LibraryError::NotFound(_))
        ));
        assert!(matches!(
            pin(&lib, PinKind::Playlist, 999),
            Err(LibraryError::NotFound(_))
        ));
        let pl = lib.create_playlist("P").unwrap();
        lib.delete_playlist(pl.id).unwrap();
        assert!(matches!(
            pin(&lib, PinKind::Playlist, pl.id),
            Err(LibraryError::NotFound(_))
        ));
        assert!(list(&lib).unwrap().is_empty());
    }

    #[test]
    fn pinned_tracks_is_distinct_union_in_pin_order() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let a1 = add_track(&lib, "Artist/A/01 - One.flac", "A");
        let a2 = add_track(&lib, "Artist/A/02 - Two.flac", "A");
        let b1 = add_track(&lib, "Artist/B/01 - Three.flac", "B");
        let album_a = lib.track(a1).unwrap().album_id;
        let album_b = lib.track(b1).unwrap().album_id;
        let pl = lib.create_playlist("Mix").unwrap();
        // The playlist repeats a1 and adds b1 again: both are already covered.
        lib.add_to_playlist(
            pl.id,
            &[
                "Artist/A/01 - One.flac".to_owned(),
                "Artist/B/01 - Three.flac".to_owned(),
                "Artist/Missing/01 - Gone.flac".to_owned(),
            ],
        )
        .unwrap();

        pin(&lib, PinKind::Playlist, pl.id).unwrap();
        pin(&lib, PinKind::Album, album_a).unwrap();
        pin(&lib, PinKind::Album, album_b).unwrap();
        set_created(&lib, PinKind::Album, album_b, 1);
        set_created(&lib, PinKind::Album, album_a, 2);
        set_created(&lib, PinKind::Playlist, pl.id, 3);

        let keys: Vec<i64> = pinned_tracks(&lib).unwrap().iter().map(|t| t.0).collect();
        assert_eq!(keys, vec![b1, a1, a2]);
        let tracks = pinned_tracks(&lib).unwrap();
        assert_eq!(tracks[1].1, "Artist/A/01 - One.flac");
        assert_eq!(tracks[1].2, 100);

        let of_playlist: Vec<i64> = tracks_of(&lib, PinKind::Playlist, pl.id)
            .unwrap()
            .iter()
            .map(|t| t.0)
            .collect();
        assert_eq!(of_playlist, vec![a1, b1]);
    }

    #[test]
    fn tombstoned_playlist_contributes_nothing_and_is_pruned() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let t = add_track(&lib, "Artist/A/01 - One.flac", "A");
        let pl = lib.create_playlist("P").unwrap();
        lib.add_to_playlist(pl.id, &["Artist/A/01 - One.flac".to_owned()])
            .unwrap();
        pin(&lib, PinKind::Playlist, pl.id).unwrap();
        assert_eq!(pinned_tracks(&lib).unwrap().len(), 1);

        // What a sync tombstone looks like: the row is deleted, the pin is left behind.
        lib.lock()
            .execute(
                "UPDATE playlists SET deleted = 1 WHERE id = ?1",
                params![pl.id],
            )
            .unwrap();
        assert!(pinned_tracks(&lib).unwrap().is_empty());
        assert_eq!(prune_pins(&lib).unwrap(), 1);
        assert!(list(&lib).unwrap().is_empty());
        assert!(!is_pinned(&lib, PinKind::Album, t).unwrap());
    }

    #[test]
    fn deleting_a_pinned_playlist_drops_its_pin() {
        let dir = TempDir::new();
        let lib = library(&dir);
        add_track(&lib, "Artist/A/01 - One.flac", "A");
        let pl = lib.create_playlist("P").unwrap();
        pin(&lib, PinKind::Playlist, pl.id).unwrap();
        lib.delete_playlist(pl.id).unwrap();
        assert!(list(&lib).unwrap().is_empty());
        assert_eq!(prune_pins(&lib).unwrap(), 0);
    }

    #[test]
    fn scan_removal_leaves_no_pin_to_a_gone_album() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let t = add_track(&lib, "Artist/A/01 - One.flac", "A");
        let album = lib.track(t).unwrap().album_id;
        pin(&lib, PinKind::Album, album).unwrap();

        lib.remove_tracks(&["Artist/A/01 - One.flac".to_owned()])
            .unwrap();
        lib.prune().unwrap();
        assert_eq!(prune_pins(&lib).unwrap(), 1);
        assert!(list(&lib).unwrap().is_empty());
        assert!(pinned_tracks(&lib).unwrap().is_empty());
    }
}
