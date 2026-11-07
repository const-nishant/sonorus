//! Integration tests for the upload methods of `SonorusCore` that need no bucket.
//!
//! The S3 paths (prepare-time lookups, multipart upload, resume, cancel with abort) need a
//! bucket and are not run here. The pure parts (tags, hashing, key choice, dedupe through the
//! queue) are unit tests in `src/upload.rs`. Nothing here touches the network.

#![cfg(unix)]

use std::fs::{self, File};
use std::os::unix::io::IntoRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sonorus_core::types::CoreConfig;
use sonorus_core::{CoreError, SonorusCore};

/// Fixture with tags, shared with the library crate.
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../library/tests/fixtures/Artist B/Album Two/01 - Wave Song.wav"
);

/// A directory under the system temp dir, removed when dropped (even on a panic).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "sonorus-core-uploads-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn open_core(dir: &TempDir) -> Arc<SonorusCore> {
    SonorusCore::new(CoreConfig {
        data_dir: dir.path().to_string_lossy().into_owned(),
        s3: None,
        local_music_dir: None,
    })
    .expect("open core")
}

/// Gives up ownership of a fresh descriptor for the fixture, as the shell does.
fn fixture_fd() -> i32 {
    File::open(FIXTURE).expect("open fixture").into_raw_fd()
}

#[test]
fn prepare_needs_a_bucket() {
    let dir = TempDir::new("prepare");
    let core = open_core(&dir);
    let result = core.upload_prepare(
        fixture_fd(),
        "content://picked/1".into(),
        "01 - Wave Song.wav".into(),
    );
    assert!(matches!(result, Err(CoreError::NotConfigured)));
    assert!(core.uploads().expect("uploads").is_empty());
}

#[test]
fn a_local_folder_is_not_an_upload_backend() {
    let dir = TempDir::new("local");
    let core = open_core(&dir);
    let fixtures = Path::new(FIXTURE).parent().unwrap();
    core.configure_local(fixtures.to_string_lossy().into_owned())
        .expect("configure local");
    assert!(matches!(
        core.upload_prepare(fixture_fd(), "u".into(), "01 - Wave Song.wav".into()),
        Err(CoreError::NotConfigured)
    ));
    assert!(matches!(
        core.upload_run(1, fixture_fd()),
        Err(CoreError::NotConfigured)
    ));
}

#[test]
fn run_needs_a_bucket() {
    let dir = TempDir::new("run");
    let core = open_core(&dir);
    assert!(matches!(
        core.upload_run(1, fixture_fd()),
        Err(CoreError::NotConfigured)
    ));
}

#[test]
fn negative_descriptor_is_refused() {
    let dir = TempDir::new("negative");
    let core = open_core(&dir);
    assert!(matches!(
        core.upload_prepare(-1, "u".into(), "a.flac".into()),
        Err(CoreError::Internal { .. })
    ));
    assert!(matches!(
        core.upload_run(1, -1),
        Err(CoreError::Internal { .. })
    ));
}

#[test]
fn unknown_job_is_not_found_for_cancel() {
    let dir = TempDir::new("cancel");
    let core = open_core(&dir);
    assert!(matches!(
        core.upload_cancel(999),
        Err(CoreError::NotFound { .. })
    ));
}

#[test]
fn queue_starts_empty_and_clearing_it_deletes_nothing() {
    let dir = TempDir::new("queue");
    let core = open_core(&dir);
    assert!(core.uploads().expect("uploads").is_empty());
    assert_eq!(core.clear_finished_uploads().expect("clear"), 0);
}
