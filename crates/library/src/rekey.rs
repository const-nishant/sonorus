//! Catalogue updates behind bucket management: moving every row that is keyed by an object key
//! to a new key, forgetting the uploads of deleted objects, and setting an album's cover.
//!
//! Track keys are stored in `tracks.key`, `favorites.track_key`, `history.track_key`,
//! `playlist_items.track_key`, `lyrics_sidecars.track_key` and `lyrics_sidecars.lrc_key`, and
//! `uploads.key`. `track_fts` holds no key (its rowid is the track id), so it needs no change.
//! Pins refer to albums and playlists, not to keys, so they follow the rows automatically.

use rusqlite::{OptionalExtension, params};

use crate::db::{Library, now_ms, to_i64};
use crate::error::{LibraryError, Result};
use crate::model::ObjectStamp;

/// Moves the catalogue rows of object key `from` to `to`, in one transaction.
///
/// The track row (when there is one) takes `stamp` as its new key, size, etag and last-modified
/// time, since the object was copied to `to` and its bytes are the same. `lrc` is the `.lrc`
/// sidecar object that was moved next to it, with its new key; `None` removes the sidecar row,
/// because the sidecar no longer exists. The sidecar row is keyed by the track, so it moves
/// whether or not a track row exists.
///
/// Every row that refers to `from` moves: favorites, history, playlist items, and the uploads that
/// are done or duplicates. A favorite already stored under `to` (left over from an earlier object
/// with that key) is merged: the one written last wins. A sidecar row already under `to` belongs to
/// no track and is dropped.
///
/// Returns whether a track row moved. Fails, and changes nothing, when the catalogue already has a
/// track under `to` (the key is unique).
pub fn rekey_track(
    lib: &Library,
    from: &str,
    to: &str,
    stamp: &ObjectStamp,
    lrc: Option<&ObjectStamp>,
) -> Result<bool> {
    let mut conn = lib.lock();
    let tx = conn.transaction()?;

    tx.execute(
        "DELETE FROM lyrics_sidecars WHERE track_key = ?1",
        params![to],
    )?;
    tx.execute(
        "INSERT INTO favorites (track_key, favorite, updated_at)
         SELECT ?2, favorite, updated_at FROM favorites WHERE track_key = ?1
         ON CONFLICT(track_key) DO UPDATE SET
            favorite = excluded.favorite,
            updated_at = excluded.updated_at
         WHERE excluded.updated_at > favorites.updated_at",
        params![from, to],
    )?;
    tx.execute("DELETE FROM favorites WHERE track_key = ?1", params![from])?;
    tx.execute(
        "UPDATE history SET track_key = ?2 WHERE track_key = ?1",
        params![from, to],
    )?;
    // Playlists are synced last writer wins by `updated_at`. A playlist that changes its keys
    // here must look newer than the copy in the bucket, or the next sync brings the old keys back.
    tx.execute(
        "UPDATE playlists SET updated_at = MAX(updated_at + 1, ?2)
         WHERE id IN (SELECT playlist_id FROM playlist_items WHERE track_key = ?1)",
        params![from, now_ms()],
    )?;
    tx.execute(
        "UPDATE playlist_items SET track_key = ?2 WHERE track_key = ?1",
        params![from, to],
    )?;
    tx.execute(
        "UPDATE uploads SET key = ?2 WHERE key = ?1 AND state IN ('done', 'duplicate')",
        params![from, to],
    )?;
    match lrc {
        Some(lrc) => {
            tx.execute(
                "UPDATE lyrics_sidecars SET track_key = ?2, lrc_key = ?3, size = ?4, etag = ?5
                 WHERE track_key = ?1",
                params![from, to, lrc.key, to_i64(lrc.size), lrc.etag.as_deref()],
            )?;
        }
        None => {
            tx.execute(
                "DELETE FROM lyrics_sidecars WHERE track_key = ?1",
                params![from],
            )?;
        }
    }
    let moved = tx.execute(
        "UPDATE tracks SET key = ?2, size = ?3, etag = ?4, last_modified = ?5 WHERE key = ?1",
        params![
            from,
            to,
            to_i64(stamp.size),
            stamp.etag.as_deref(),
            stamp.last_modified
        ],
    )?;
    tx.commit()?;
    Ok(moved > 0)
}

/// Deletes the upload rows of `key` that finished (done or duplicate), so that a later upload of
/// the same content is sent again instead of being reported as a duplicate of an object that is
/// gone. Queued, uploading, failed and cancelled rows are kept. Returns how many rows were
/// deleted.
pub fn forget_uploads(lib: &Library, key: &str) -> Result<u32> {
    let conn = lib.lock();
    let deleted = conn.execute(
        "DELETE FROM uploads WHERE key = ?1 AND state IN ('done', 'duplicate')",
        params![key],
    )?;
    Ok(u32::try_from(deleted).unwrap_or(u32::MAX))
}

/// Points album `album_id` at `cover_id`, and every track of the album whose cover is unset or is
/// the album's previous cover at `cover_id` too. Tracks with their own embedded cover keep it.
/// `NotFound` when the album does not exist.
pub fn set_album_cover(lib: &Library, album_id: i64, cover_id: i64) -> Result<()> {
    let mut conn = lib.lock();
    let tx = conn.transaction()?;
    let previous: Option<Option<i64>> = tx
        .query_row(
            "SELECT cover_id FROM albums WHERE id = ?1",
            params![album_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(previous) = previous else {
        return Err(LibraryError::NotFound(format!("album {album_id}")));
    };
    tx.execute(
        "UPDATE albums SET cover_id = ?2 WHERE id = ?1",
        params![album_id, cover_id],
    )?;
    tx.execute(
        "UPDATE tracks SET cover_id = ?2
         WHERE album_id = ?1 AND (cover_id IS NULL OR cover_id IS ?3)",
        params![album_id, cover_id, previous],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{TempDir, meta, stamp};
    use crate::uploads::{self, NewUpload, UploadState};

    fn library(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).unwrap()
    }

    fn catalogue(lib: &Library, key: &str) -> i64 {
        lib.upsert_track(&stamp(key, 10), &meta("t", "Artist", "Album"), None)
            .unwrap()
    }

    fn stamp_of(key: &str, size: u64) -> ObjectStamp {
        ObjectStamp {
            key: key.to_owned(),
            size,
            etag: Some("new-etag".into()),
            last_modified: Some(1_800_000_000),
        }
    }

    fn upload(lib: &Library, key: &str, state: UploadState) -> i64 {
        let id = uploads::insert(
            lib,
            &NewUpload {
                source_uri: "content://x".into(),
                display_name: "x.flac".into(),
                size: 10,
                blake3: format!("hash-{key}-{}", state.as_str()),
                key: key.into(),
                content_type: "audio/flac".into(),
                state: UploadState::Queued,
            },
        )
        .unwrap();
        if state != UploadState::Queued {
            uploads::set_state(lib, id, state, None).unwrap();
        }
        id
    }

    fn count(lib: &Library, sql: &str, key: &str) -> i64 {
        let conn = lib.lock();
        conn.query_row(sql, params![key], |r| r.get(0)).unwrap()
    }

    #[test]
    fn rekey_moves_every_row_that_names_the_key() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let old = "Artist/Album/01 - a.flac";
        let new = "Artist/Album/01 - renamed.flac";
        let track_id = catalogue(&lib, old);
        lib.set_favorite(old, true).unwrap();
        lib.record_play(old, 1_000).unwrap();
        let pl = lib.create_playlist("mix").unwrap();
        lib.add_to_playlist(pl.id, &[old.to_owned()]).unwrap();
        let before = lib.playlist_records().unwrap();
        lib.upsert_sidecar(
            old,
            "Artist/Album/01 - a.lrc",
            9,
            Some("e1"),
            "[00:00.00]hi",
        )
        .unwrap();
        let done = upload(&lib, old, UploadState::Done);
        let queued = upload(&lib, old, UploadState::Queued);

        let lrc = ObjectStamp {
            key: "Artist/Album/01 - renamed.lrc".into(),
            size: 12,
            etag: Some("lrc-etag".into()),
            last_modified: None,
        };
        assert!(rekey_track(&lib, old, new, &stamp_of(new, 10), Some(&lrc)).unwrap());

        assert_eq!(lib.track(track_id).unwrap().key, new, "same row, new key");
        assert_eq!(
            lib.track(track_id).unwrap().etag.as_deref(),
            Some("new-etag")
        );
        assert!(lib.is_favorite(new).unwrap());
        assert!(!lib.is_favorite(old).unwrap());
        assert_eq!(
            count(
                &lib,
                "SELECT COUNT(*) FROM history WHERE track_key = ?1",
                new
            ),
            1
        );
        assert_eq!(
            count(
                &lib,
                "SELECT COUNT(*) FROM history WHERE track_key = ?1",
                old
            ),
            0
        );
        assert_eq!(
            count(
                &lib,
                "SELECT COUNT(*) FROM playlist_items WHERE track_key = ?1",
                new
            ),
            1
        );
        let entries = lib.playlist_entries(pl.id).unwrap();
        assert_eq!(entries[0].track.as_ref().map(|t| t.id), Some(track_id));
        let sidecar = lib.sidecar_rows().unwrap();
        assert_eq!(
            sidecar.get(new).map(|r| r.lrc_key.as_str()),
            Some(lrc.key.as_str())
        );
        assert_eq!(sidecar.get(new).map(|r| r.size), Some(12));
        assert!(!sidecar.contains_key(old));
        let after = lib.playlist_records().unwrap();
        assert_eq!(
            after[0].tracks,
            vec![new.to_owned()],
            "playlist item re-keyed"
        );
        assert!(
            after[0].updated_at > before[0].updated_at,
            "playlist looks newer to the next sync"
        );
        assert_eq!(
            upload_key(&lib, done),
            new,
            "finished upload follows the object"
        );
        assert_eq!(
            upload_key(&lib, queued),
            old,
            "pending upload is not rewritten"
        );
    }

    fn upload_key(lib: &Library, id: i64) -> String {
        uploads::get(lib, id).unwrap().unwrap().key
    }

    #[test]
    fn rekey_without_a_sidecar_drops_the_sidecar_row() {
        let dir = TempDir::new();
        let lib = library(&dir);
        catalogue(&lib, "a/x.flac");
        lib.upsert_sidecar("a/x.flac", "a/x.lrc", 3, None, "text")
            .unwrap();
        rekey_track(
            &lib,
            "a/x.flac",
            "a/y.flac",
            &stamp_of("a/y.flac", 10),
            None,
        )
        .unwrap();
        assert!(lib.sidecar_rows().unwrap().is_empty());
    }

    #[test]
    fn dangling_favorite_at_the_destination_merges_by_last_write() {
        let dir = TempDir::new();
        let lib = library(&dir);
        catalogue(&lib, "a/old.flac");
        lib.set_favorite("a/old.flac", true).unwrap();
        // A favorite left behind by an earlier object at the destination key, written before.
        lib.apply_favorite_records(&[crate::model::FavoriteRecord {
            track_key: "a/new.flac".into(),
            favorite: false,
            updated_at: 1,
        }])
        .unwrap();
        rekey_track(
            &lib,
            "a/old.flac",
            "a/new.flac",
            &stamp_of("a/new.flac", 10),
            None,
        )
        .unwrap();
        assert!(
            lib.is_favorite("a/new.flac").unwrap(),
            "the newer favorite wins"
        );
        let records = lib.favorite_records().unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn rekey_onto_an_existing_track_fails_and_changes_nothing() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let from_id = catalogue(&lib, "a/one.flac");
        catalogue(&lib, "a/two.flac");
        lib.set_favorite("a/one.flac", true).unwrap();
        assert!(
            rekey_track(
                &lib,
                "a/one.flac",
                "a/two.flac",
                &stamp_of("a/two.flac", 10),
                None
            )
            .is_err()
        );
        assert_eq!(lib.track(from_id).unwrap().key, "a/one.flac");
        assert!(lib.is_favorite("a/one.flac").unwrap());
    }

    #[test]
    fn rekey_of_an_uncatalogued_key_moves_only_user_rows() {
        let dir = TempDir::new();
        let lib = library(&dir);
        lib.set_favorite("gone.flac", true).unwrap();
        assert!(
            !rekey_track(
                &lib,
                "gone.flac",
                "here.flac",
                &stamp_of("here.flac", 1),
                None
            )
            .unwrap()
        );
        assert!(lib.is_favorite("here.flac").unwrap());
    }

    #[test]
    fn forget_uploads_keeps_pending_rows() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let done = upload(&lib, "a/x.flac", UploadState::Done);
        let dup = upload(&lib, "a/x.flac", UploadState::Duplicate);
        let queued = upload(&lib, "a/x.flac", UploadState::Queued);
        assert_eq!(forget_uploads(&lib, "a/x.flac").unwrap(), 2);
        assert!(uploads::get(&lib, done).unwrap().is_none());
        assert!(uploads::get(&lib, dup).unwrap().is_none());
        assert!(uploads::get(&lib, queued).unwrap().is_some());
    }

    #[test]
    fn album_cover_follows_tracks_without_their_own() {
        let dir = TempDir::new();
        let lib = library(&dir);
        let covers = dir.path().join("covers");
        std::fs::create_dir_all(&covers).unwrap();
        let old_cover = lib.store_cover(b"old", "image/jpeg").unwrap();
        let new_cover = lib.store_cover(b"new", "image/png").unwrap();
        let own = lib.store_cover(b"own", "image/jpeg").unwrap();
        let album_id = {
            let t = lib
                .upsert_track(
                    &stamp("a/1.flac", 1),
                    &meta("t1", "A", "Album"),
                    Some(old_cover),
                )
                .unwrap();
            lib.track(t).unwrap().album_id
        };
        lib.upsert_track(&stamp("a/2.flac", 1), &meta("t2", "A", "Album"), None)
            .unwrap();
        lib.upsert_track(&stamp("a/3.flac", 1), &meta("t3", "A", "Album"), Some(own))
            .unwrap();

        set_album_cover(&lib, album_id, new_cover).unwrap();

        assert_eq!(lib.album(album_id).unwrap().cover_id, Some(new_cover));
        let tracks = lib.album_tracks(album_id).unwrap();
        let cover_of = |key: &str| tracks.iter().find(|t| t.key == key).unwrap().cover_id;
        assert_eq!(cover_of("a/1.flac"), Some(new_cover));
        assert_eq!(cover_of("a/2.flac"), Some(new_cover));
        assert_eq!(cover_of("a/3.flac"), Some(own));
        assert!(matches!(
            set_album_cover(&lib, 999, new_cover),
            Err(LibraryError::NotFound(_))
        ));
    }
}
