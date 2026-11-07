//! The upload queue: one row per audio file the user sends to the bucket from this device.
//! Rows survive restarts, so an interrupted multipart upload can be resumed from its upload id.
//!
//! Functions take the [`Library`] as their first argument. Timestamps are Unix milliseconds.

use rusqlite::types::Type;
use rusqlite::{OptionalExtension, Row, params};

use crate::db::{Library, now_ms, to_i64};
use crate::error::{LibraryError, Result};

/// Where an upload job stands. `Done`, `Duplicate` and `Cancelled` never change again. `Failed`
/// jobs keep their upload id and can be resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadState {
    /// Added, not started.
    Queued,
    /// A multipart upload is in progress.
    Uploading,
    /// The object is stored in the bucket.
    Done,
    /// The file's content is already in the bucket or queue; nothing was uploaded.
    Duplicate,
    /// A storage or read error stopped the upload. Resumable.
    Failed,
    /// The user cancelled; the multipart upload was aborted.
    Cancelled,
}

impl UploadState {
    /// The value stored in the `state` column.
    pub fn as_str(self) -> &'static str {
        match self {
            UploadState::Queued => "queued",
            UploadState::Uploading => "uploading",
            UploadState::Done => "done",
            UploadState::Duplicate => "duplicate",
            UploadState::Failed => "failed",
            UploadState::Cancelled => "cancelled",
        }
    }

    /// Parses a stored `state` value. `None` for anything unknown.
    pub fn parse(s: &str) -> Option<UploadState> {
        match s {
            "queued" => Some(UploadState::Queued),
            "uploading" => Some(UploadState::Uploading),
            "done" => Some(UploadState::Done),
            "duplicate" => Some(UploadState::Duplicate),
            "failed" => Some(UploadState::Failed),
            "cancelled" => Some(UploadState::Cancelled),
            _ => None,
        }
    }

    /// True for states that never change again: done, duplicate and cancelled.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            UploadState::Done | UploadState::Duplicate | UploadState::Cancelled
        )
    }
}

/// One row of the `uploads` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRow {
    pub id: i64,
    /// Where the bytes came from (a content URI), for display and re-opening.
    pub source_uri: String,
    pub display_name: String,
    /// Source file size in bytes when the job was prepared.
    pub size: u64,
    /// Lowercase hex BLAKE3 hash of the whole file.
    pub blake3: String,
    /// Object key in the bucket (for a duplicate, the key of the existing object).
    pub key: String,
    pub content_type: String,
    /// Multipart upload id, once the upload has started.
    pub upload_id: Option<String>,
    pub state: UploadState,
    /// Bytes of the source that are stored in the bucket.
    pub bytes_done: u64,
    /// Short description of the last failure, if any.
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Values for a new row. `state` is the initial state (`Queued` or `Duplicate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUpload {
    pub source_uri: String,
    pub display_name: String,
    pub size: u64,
    pub blake3: String,
    pub key: String,
    pub content_type: String,
    pub state: UploadState,
}

const SELECT_SQL: &str = "SELECT id, source_uri, display_name, size, blake3, key, content_type,
    upload_id, state, bytes_done, error, created_at, updated_at FROM uploads";

/// Converts a non-negative SQLite integer to `u64`; negative values (never written) become 0.
fn u64_col(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn row_to_upload(r: &Row<'_>) -> rusqlite::Result<UploadRow> {
    let state: String = r.get(8)?;
    let state = UploadState::parse(&state).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            Type::Text,
            format!("unknown upload state {state:?}").into(),
        )
    })?;
    Ok(UploadRow {
        id: r.get(0)?,
        source_uri: r.get(1)?,
        display_name: r.get(2)?,
        size: u64_col(r.get(3)?),
        blake3: r.get(4)?,
        key: r.get(5)?,
        content_type: r.get(6)?,
        upload_id: r.get(7)?,
        state,
        bytes_done: u64_col(r.get(9)?),
        error: r.get(10)?,
        created_at: r.get(11)?,
        updated_at: r.get(12)?,
    })
}

/// `NotFound` when an update touched no row.
fn expect_one(changed: usize, id: i64) -> Result<()> {
    if changed == 0 {
        return Err(LibraryError::NotFound(format!("upload {id}")));
    }
    Ok(())
}

/// Adds a row and returns its id. `bytes_done` starts at 0.
pub fn insert(lib: &Library, new: &NewUpload) -> Result<i64> {
    let now = now_ms();
    let conn = lib.lock();
    let id: i64 = conn.query_row(
        "INSERT INTO uploads (source_uri, display_name, size, blake3, key, content_type, state,
            bytes_done, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?8)
         RETURNING id",
        params![
            new.source_uri,
            new.display_name,
            to_i64(new.size),
            new.blake3,
            new.key,
            new.content_type,
            new.state.as_str(),
            now,
        ],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// The row with `id`, or `None`.
pub fn get(lib: &Library, id: i64) -> Result<Option<UploadRow>> {
    let conn = lib.lock();
    let row = conn
        .query_row(
            &format!("{SELECT_SQL} WHERE id = ?1"),
            params![id],
            row_to_upload,
        )
        .optional()?;
    Ok(row)
}

/// All rows, newest first.
pub fn list(lib: &Library) -> Result<Vec<UploadRow>> {
    let conn = lib.lock();
    let mut stmt = conn.prepare(&format!("{SELECT_SQL} ORDER BY id DESC"))?;
    let rows = stmt.query_map([], row_to_upload)?;
    let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

/// Stores the multipart upload id (`None` clears it). `NotFound` for an unknown id.
pub fn set_upload_id(lib: &Library, id: i64, upload_id: Option<&str>) -> Result<()> {
    let conn = lib.lock();
    let changed = conn.execute(
        "UPDATE uploads SET upload_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![upload_id, now_ms(), id],
    )?;
    expect_one(changed, id)
}

/// Stores the number of source bytes that are in the bucket. `NotFound` for an unknown id.
pub fn set_progress(lib: &Library, id: i64, bytes_done: u64) -> Result<()> {
    let conn = lib.lock();
    let changed = conn.execute(
        "UPDATE uploads SET bytes_done = ?1, updated_at = ?2 WHERE id = ?3",
        params![to_i64(bytes_done), now_ms(), id],
    )?;
    expect_one(changed, id)
}

/// Sets the state and the error text (`None` clears it). `NotFound` for an unknown id.
pub fn set_state(lib: &Library, id: i64, state: UploadState, error: Option<&str>) -> Result<()> {
    let conn = lib.lock();
    let changed = conn.execute(
        "UPDATE uploads SET state = ?1, error = ?2, updated_at = ?3 WHERE id = ?4",
        params![state.as_str(), error, now_ms(), id],
    )?;
    expect_one(changed, id)
}

/// A row whose content hash is `blake3` and whose state is done, queued or uploading. The most
/// recent such row when there are several. Failed, cancelled and duplicate rows never match.
pub fn find_by_hash(lib: &Library, blake3: &str) -> Result<Option<UploadRow>> {
    let conn = lib.lock();
    let row = conn
        .query_row(
            &format!(
                "{SELECT_SQL} WHERE blake3 = ?1 AND state IN ('done', 'queued', 'uploading')
                 ORDER BY id DESC LIMIT 1"
            ),
            params![blake3],
            row_to_upload,
        )
        .optional()?;
    Ok(row)
}

/// Whether a job that may still write `key` (queued, uploading or failed, since a failed job
/// can resume) already holds it. Used to avoid giving two files the same object key.
pub fn key_is_reserved(lib: &Library, key: &str) -> Result<bool> {
    let conn = lib.lock();
    let found: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM uploads
            WHERE key = ?1 AND state IN ('queued', 'uploading', 'failed'))",
        params![key],
        |r| r.get(0),
    )?;
    Ok(found != 0)
}

/// Deletes the rows in a final state (done, duplicate, cancelled). Returns how many rows were
/// deleted. Queued, uploading and failed rows are kept: a failed row may still own a multipart
/// upload in the bucket, which only a retry (completes it) or a cancel (aborts it) cleans up.
pub fn delete_finished(lib: &Library) -> Result<u32> {
    let conn = lib.lock();
    let deleted = conn.execute(
        "DELETE FROM uploads WHERE state IN ('done', 'duplicate', 'cancelled')",
        [],
    )?;
    Ok(u32::try_from(deleted).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::TempDir;

    fn lib(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).unwrap()
    }

    fn new_row(key: &str, blake3: &str, state: UploadState) -> NewUpload {
        NewUpload {
            source_uri: format!("content://media/{key}"),
            display_name: key.rsplit('/').next().unwrap().to_owned(),
            size: 1234,
            blake3: blake3.to_owned(),
            key: key.to_owned(),
            content_type: "audio/flac".to_owned(),
            state,
        }
    }

    #[test]
    fn state_strings_round_trip() {
        let all = [
            UploadState::Queued,
            UploadState::Uploading,
            UploadState::Done,
            UploadState::Duplicate,
            UploadState::Failed,
            UploadState::Cancelled,
        ];
        for state in all {
            assert_eq!(UploadState::parse(state.as_str()), Some(state));
        }
        assert_eq!(UploadState::parse("Queued"), None);
        assert_eq!(UploadState::parse(""), None);
    }

    #[test]
    fn only_done_duplicate_and_cancelled_are_terminal() {
        assert!(UploadState::Done.is_terminal());
        assert!(UploadState::Duplicate.is_terminal());
        assert!(UploadState::Cancelled.is_terminal());
        assert!(!UploadState::Queued.is_terminal());
        assert!(!UploadState::Uploading.is_terminal());
        assert!(!UploadState::Failed.is_terminal());
    }

    #[test]
    fn insert_then_get_round_trips_every_column() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        let id = insert(
            &lib,
            &new_row("A/B/01 - x.flac", "abc", UploadState::Queued),
        )
        .unwrap();
        let row = get(&lib, id).unwrap().expect("row");
        assert_eq!(row.id, id);
        assert_eq!(row.source_uri, "content://media/A/B/01 - x.flac");
        assert_eq!(row.display_name, "01 - x.flac");
        assert_eq!(row.size, 1234);
        assert_eq!(row.blake3, "abc");
        assert_eq!(row.key, "A/B/01 - x.flac");
        assert_eq!(row.content_type, "audio/flac");
        assert_eq!(row.upload_id, None);
        assert_eq!(row.state, UploadState::Queued);
        assert_eq!(row.bytes_done, 0);
        assert_eq!(row.error, None);
        assert!(row.created_at > 0);
        assert_eq!(row.created_at, row.updated_at);
    }

    #[test]
    fn get_unknown_id_is_none() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        assert_eq!(get(&lib, 42).unwrap(), None);
    }

    #[test]
    fn list_is_newest_first() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        let a = insert(&lib, &new_row("a.flac", "1", UploadState::Queued)).unwrap();
        let b = insert(&lib, &new_row("b.flac", "2", UploadState::Queued)).unwrap();
        let c = insert(&lib, &new_row("c.flac", "3", UploadState::Duplicate)).unwrap();
        let ids: Vec<i64> = list(&lib).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![c, b, a]);
    }

    #[test]
    fn set_upload_id_progress_and_state_update_the_row() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        let id = insert(&lib, &new_row("x.flac", "h", UploadState::Queued)).unwrap();

        set_upload_id(&lib, id, Some("up-1")).unwrap();
        set_progress(&lib, id, 8 * 1024 * 1024).unwrap();
        set_state(&lib, id, UploadState::Uploading, None).unwrap();
        let row = get(&lib, id).unwrap().unwrap();
        assert_eq!(row.upload_id.as_deref(), Some("up-1"));
        assert_eq!(row.bytes_done, 8 * 1024 * 1024);
        assert_eq!(row.state, UploadState::Uploading);

        set_state(&lib, id, UploadState::Failed, Some("network error")).unwrap();
        let row = get(&lib, id).unwrap().unwrap();
        assert_eq!(row.state, UploadState::Failed);
        assert_eq!(row.error.as_deref(), Some("network error"));

        // Moving on clears the error; the upload id survives.
        set_state(&lib, id, UploadState::Queued, None).unwrap();
        let row = get(&lib, id).unwrap().unwrap();
        assert_eq!(row.error, None);
        assert_eq!(row.upload_id.as_deref(), Some("up-1"));

        set_upload_id(&lib, id, None).unwrap();
        assert_eq!(get(&lib, id).unwrap().unwrap().upload_id, None);
    }

    #[test]
    fn updates_to_unknown_id_are_not_found() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        assert!(matches!(
            set_progress(&lib, 9, 1),
            Err(LibraryError::NotFound(_))
        ));
        assert!(matches!(
            set_upload_id(&lib, 9, Some("x")),
            Err(LibraryError::NotFound(_))
        ));
        assert!(matches!(
            set_state(&lib, 9, UploadState::Done, None),
            Err(LibraryError::NotFound(_))
        ));
    }

    #[test]
    fn find_by_hash_matches_done_queued_and_uploading_only() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        for (state, hash) in [
            (UploadState::Done, "d"),
            (UploadState::Queued, "q"),
            (UploadState::Uploading, "u"),
            (UploadState::Failed, "f"),
            (UploadState::Cancelled, "c"),
            (UploadState::Duplicate, "dup"),
        ] {
            insert(&lib, &new_row(&format!("{hash}.flac"), hash, state)).unwrap();
        }
        assert!(find_by_hash(&lib, "d").unwrap().is_some());
        assert!(find_by_hash(&lib, "q").unwrap().is_some());
        assert!(find_by_hash(&lib, "u").unwrap().is_some());
        assert!(find_by_hash(&lib, "f").unwrap().is_none());
        assert!(find_by_hash(&lib, "c").unwrap().is_none());
        assert!(find_by_hash(&lib, "dup").unwrap().is_none());
        assert!(find_by_hash(&lib, "missing").unwrap().is_none());

        let hit = find_by_hash(&lib, "d").unwrap().unwrap();
        assert_eq!(hit.key, "d.flac");
    }

    #[test]
    fn find_by_hash_prefers_the_most_recent_row() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        insert(&lib, &new_row("old.flac", "same", UploadState::Done)).unwrap();
        insert(&lib, &new_row("new.flac", "same", UploadState::Queued)).unwrap();
        assert_eq!(find_by_hash(&lib, "same").unwrap().unwrap().key, "new.flac");
    }

    #[test]
    fn key_reservation_covers_queued_uploading_and_failed() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        insert(&lib, &new_row("q.flac", "1", UploadState::Queued)).unwrap();
        insert(&lib, &new_row("u.flac", "2", UploadState::Uploading)).unwrap();
        insert(&lib, &new_row("f.flac", "3", UploadState::Failed)).unwrap();
        insert(&lib, &new_row("d.flac", "4", UploadState::Done)).unwrap();
        insert(&lib, &new_row("c.flac", "5", UploadState::Cancelled)).unwrap();
        assert!(key_is_reserved(&lib, "q.flac").unwrap());
        assert!(key_is_reserved(&lib, "u.flac").unwrap());
        assert!(key_is_reserved(&lib, "f.flac").unwrap());
        assert!(!key_is_reserved(&lib, "d.flac").unwrap());
        assert!(!key_is_reserved(&lib, "c.flac").unwrap());
        assert!(!key_is_reserved(&lib, "none.flac").unwrap());
    }

    #[test]
    fn delete_finished_removes_only_final_rows() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        insert(&lib, &new_row("1.flac", "1", UploadState::Done)).unwrap();
        insert(&lib, &new_row("2.flac", "2", UploadState::Duplicate)).unwrap();
        insert(&lib, &new_row("3.flac", "3", UploadState::Cancelled)).unwrap();
        let keep_f = insert(&lib, &new_row("4.flac", "4", UploadState::Failed)).unwrap();
        let keep_q = insert(&lib, &new_row("5.flac", "5", UploadState::Queued)).unwrap();
        let keep_u = insert(&lib, &new_row("6.flac", "6", UploadState::Uploading)).unwrap();

        assert_eq!(delete_finished(&lib).unwrap(), 3);
        let ids: Vec<i64> = list(&lib).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![keep_u, keep_q, keep_f]);
        assert_eq!(delete_finished(&lib).unwrap(), 0);
    }

    #[test]
    fn schema_rejects_unknown_states() {
        let dir = TempDir::new();
        let lib = lib(&dir);
        let conn = lib.lock();
        let result = conn.execute(
            "INSERT INTO uploads (source_uri, display_name, size, blake3, key, content_type, state,
                created_at, updated_at)
             VALUES ('s', 'n', 1, 'h', 'k', 'audio/flac', 'paused', 0, 0)",
            [],
        );
        assert!(result.is_err(), "CHECK constraint must reject 'paused'");
    }
}
