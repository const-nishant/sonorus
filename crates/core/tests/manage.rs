//! Integration tests for the bucket management API of `SonorusCore`, run against a local music
//! folder (the local backend applies the same key rules and filesystem operations as the bucket).
//!
//! These are plain `#[test]`s: the core owns its tokio runtime and must not be dropped inside
//! another runtime.

use std::fs;
use std::os::fd::IntoRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sonorus_core::types::{AlbumSort, CoreConfig, TagEditInfo, TrackInfo};
use sonorus_core::{CoreError, SonorusCore};

/// Fixture library shared with the library crate: two albums, four tracks, one cover file.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../library/tests/fixtures");
const FLAC: &str = "Artist A/Album One/01 - First Song.flac";
const M4A: &str = "Artist A/Album One/02 - Second Song.m4a";
const WAV: &str = "Artist B/Album Two/01 - Wave Song.wav";
const LRC: &str = "Artist A/Album One/01 - First Song.lrc";

/// A directory under the system temp dir, removed when dropped (even on a panic).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "sonorus-core-manage-it-{tag}-{}-{nanos}",
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

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dir");
    for entry in fs::read_dir(src).expect("read dir") {
        let entry = entry.expect("entry");
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).expect("copy");
        }
    }
}

/// A music folder with the fixtures and a sidecar for the FLAC, opened and scanned as a local
/// backend. The catalogue lives beside it in the same temp dir.
struct Fixture {
    dir: TempDir,
    music: PathBuf,
    core: Arc<SonorusCore>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = TempDir::new(tag);
        let music = dir.path().join("music");
        copy_dir(Path::new(FIXTURES), &music);
        fs::write(music.join(LRC), b"[00:00.00]hello\n[00:02.00]world").expect("lrc");
        let core = SonorusCore::new(CoreConfig {
            data_dir: dir.path().join("data").to_string_lossy().into_owned(),
            s3: None,
            local_music_dir: Some(music.to_string_lossy().into_owned()),
        })
        .expect("open core");
        core.scan_library().expect("scan");
        Self { dir, music, core }
    }

    fn exists(&self, key: &str) -> bool {
        self.music.join(key).is_file()
    }

    fn track(&self, key: &str) -> TrackInfo {
        self.core
            .albums(AlbumSort::Title, 50, 0)
            .expect("albums")
            .iter()
            .flat_map(|a| self.core.album_tracks(a.id).expect("tracks"))
            .find(|t| t.key == key)
            .unwrap_or_else(|| panic!("no track {key}"))
    }
}

/// A 1x1 PNG-shaped byte string: the signature and an IHDR chunk.
fn png_bytes() -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
    out.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    out
}

/// Passes `bytes` to the core as a file descriptor, the way the Android shell does.
fn fd_of(dir: &TempDir, name: &str, bytes: &[u8]) -> i32 {
    let path = dir.path().join(name);
    fs::write(&path, bytes).expect("write image");
    fs::File::open(&path).expect("open image").into_raw_fd()
}

#[test]
fn browse_shows_folders_files_and_track_ids_and_hides_the_internal_folder() {
    let fx = Fixture::new("browse");
    fs::create_dir_all(fx.music.join("_sonorus")).unwrap();
    let root = fx.core.browse(String::new()).expect("root");
    assert_eq!(root.path, "");
    let names: Vec<&str> = root.folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["Artist A", "Artist B"]);

    let album = fx.core.browse("Artist A/Album One".into()).expect("album");
    assert_eq!(album.path, "Artist A/Album One/");
    let flac = album.files.iter().find(|f| f.key == FLAC).expect("flac");
    assert_eq!(flac.track_id, Some(fx.track(FLAC).id));
    assert!(!flac.is_folder);
    assert!(flac.size > 0);
    let lrc = album.files.iter().find(|f| f.key == LRC).expect("lrc");
    assert_eq!(lrc.track_id, None, "a sidecar is not a track");

    assert!(matches!(
        fx.core.browse("../outside".into()),
        Err(CoreError::Refused { .. })
    ));
}

#[test]
fn rename_keeps_favorites_playlists_and_lyrics_and_moves_the_sidecar() {
    let fx = Fixture::new("rename");
    let track = fx.track(FLAC);
    fx.core.set_favorite(track.id, true).expect("favorite");
    let playlist = fx.core.create_playlist("mix".into()).expect("playlist");
    fx.core
        .add_to_playlist(playlist.id, vec![track.id])
        .expect("add");

    let new_key = fx
        .core
        .rename_object(FLAC.into(), "Renamed Song.flac".into())
        .expect("rename");
    assert_eq!(new_key, "Artist A/Album One/Renamed Song.flac");
    assert!(!fx.exists(FLAC));
    assert!(fx.exists(&new_key));
    assert!(
        fx.music
            .join("Artist A/Album One/Renamed Song.lrc")
            .is_file()
    );
    assert!(!fx.music.join(LRC).exists());

    let renamed = fx.track(&new_key);
    assert_eq!(renamed.id, track.id, "same catalogue row");
    assert!(renamed.is_favorite, "favorite follows the key");
    let entries = fx.core.playlist_entries(playlist.id).expect("entries");
    assert_eq!(entries[0].track.as_ref().map(|t| t.id), Some(track.id));
    let lyrics = fx
        .core
        .track_lyrics(track.id)
        .expect("lyrics")
        .expect("some");
    assert!(lyrics.plain.contains("hello"), "sidecar text kept");
}

#[test]
fn rename_refuses_to_overwrite_and_leaves_both_files_alone() {
    let fx = Fixture::new("no-overwrite");
    let before = fs::read(fx.music.join(FLAC)).unwrap();
    let err = fx
        .core
        .rename_object(FLAC.into(), "02 - Second Song.m4a".into())
        .expect_err("name is taken");
    assert!(matches!(err, CoreError::Refused { .. }), "{err:?}");
    assert!(fx.exists(M4A));
    assert_eq!(fs::read(fx.music.join(FLAC)).unwrap(), before);
}

#[test]
fn rename_refuses_names_that_are_not_plain_audio_names() {
    let fx = Fixture::new("bad-names");
    for name in ["sub/x.flac", "", "notes.txt", "..", "x.mp3"] {
        assert!(
            matches!(
                fx.core.rename_object(FLAC.into(), name.into()),
                Err(CoreError::Refused { .. })
            ),
            "{name:?} must be refused"
        );
    }
    assert!(fx.exists(FLAC));
}

#[test]
fn move_folder_keeps_its_structure_and_refuses_moving_into_itself() {
    let fx = Fixture::new("move");
    fs::create_dir_all(fx.music.join("Library")).unwrap();
    let report = fx
        .core
        .move_objects(vec!["Artist A/".into()], "Library/".into())
        .expect("move");
    assert_eq!(report.moved, 3, "two tracks and the sidecar");
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(fx.exists("Library/Artist A/Album One/01 - First Song.flac"));
    assert!(fx.exists("Library/Artist A/Album One/02 - Second Song.m4a"));
    assert!(
        fx.music
            .join("Library/Artist A/Album One/01 - First Song.lrc")
            .is_file()
    );
    assert!(!fx.music.join("Artist A").exists(), "source folder removed");
    assert_eq!(
        fx.track("Library/Artist A/Album One/01 - First Song.flac")
            .title,
        "First Song",
        "catalogue follows the move"
    );

    assert!(matches!(
        fx.core
            .move_objects(vec!["Library/".into()], "Library/Inner/".into()),
        Err(CoreError::Refused { .. })
    ));
    assert!(fx.exists("Library/Artist A/Album One/01 - First Song.flac"));
}

#[test]
fn move_objects_refuses_the_root_and_keys_outside_the_prefix() {
    let fx = Fixture::new("move-refuse");
    assert!(matches!(
        fx.core.move_objects(vec![String::new()], "X/".into()),
        Err(CoreError::Refused { .. })
    ));
    assert!(matches!(
        fx.core
            .move_objects(vec!["../escape.flac".into()], "X/".into()),
        Err(CoreError::Refused { .. })
    ));
    assert!(fx.exists(FLAC));
}

#[test]
fn delete_removes_the_row_and_sidecar_and_keeps_playlist_items_dangling() {
    let fx = Fixture::new("delete");
    let track = fx.track(FLAC);
    let playlist = fx.core.create_playlist("keep".into()).expect("playlist");
    fx.core
        .add_to_playlist(playlist.id, vec![track.id])
        .expect("add");

    let report = fx.core.delete_objects(vec![FLAC.into()]).expect("delete");
    assert_eq!(report.deleted, 2, "file and sidecar");
    assert!(report.failed.is_empty());
    assert!(!fx.exists(FLAC));
    assert!(!fx.music.join(LRC).exists());

    let entries = fx.core.playlist_entries(playlist.id).expect("entries");
    assert_eq!(entries.len(), 1, "slot kept");
    assert!(entries[0].track.is_none(), "shown as unavailable");
    let albums = fx.core.albums(AlbumSort::Title, 50, 0).expect("albums");
    let album = albums
        .iter()
        .find(|a| a.title == "Album One")
        .expect("album kept");
    assert_eq!(album.track_count, 1, "the other track is still there");
}

#[test]
fn deleting_a_folder_removes_its_tracks_and_the_root_is_refused() {
    let fx = Fixture::new("delete-folder");
    assert!(matches!(
        fx.core.delete_folder(String::new()),
        Err(CoreError::Refused { .. })
    ));
    assert!(fx.exists(FLAC), "root delete changed nothing");

    let report = fx
        .core
        .delete_folder("Artist B/".into())
        .expect("delete folder");
    assert_eq!(report.deleted, 3);
    assert!(!fx.music.join("Artist B").exists());
    let artists = fx.core.artists(50, 0).expect("artists");
    assert!(
        artists.iter().all(|a| a.name != "Artist B"),
        "empty artist pruned"
    );
}

#[test]
fn edit_tags_changes_the_title_in_the_catalogue_and_the_file() {
    let fx = Fixture::new("edit-tags");
    let track = fx.track(FLAC);
    fx.core
        .edit_tags(
            track.id,
            TagEditInfo {
                title: Some("Retitled Song".into()),
                ..Default::default()
            },
        )
        .expect("edit");
    assert_eq!(
        fx.core.track(track.id).expect("track").title,
        "Retitled Song"
    );
    assert_eq!(fx.track(FLAC).title, "Retitled Song");
    let leftovers = fs::read_dir(fx.dir.path().join("data/tmp"))
        .map(|rd| rd.count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "temporary copy removed");
}

#[test]
fn set_album_cover_with_a_png_sets_the_cover_and_a_non_image_is_refused() {
    let fx = Fixture::new("cover");
    let album_id = fx.track(WAV).album_id;

    let fd = fd_of(&fx.dir, "cover.png", &png_bytes());
    fx.core.set_album_cover(album_id, fd).expect("set cover");
    assert!(fx.music.join("Artist B/Album Two/cover.png").is_file());
    assert!(
        !fx.music.join("Artist B/Album Two/cover.jpg").exists(),
        "other type removed"
    );
    let cover = fx.core.album(album_id).expect("album").cover_path;
    assert!(
        cover.is_some_and(|p| Path::new(&p).is_file()),
        "catalogue cover set"
    );

    let fd = fd_of(&fx.dir, "notes.txt", b"just text, not an image");
    assert!(matches!(
        fx.core.set_album_cover(album_id, fd),
        Err(CoreError::Refused { .. })
    ));
    assert!(
        fx.music.join("Artist B/Album Two/cover.png").is_file(),
        "unchanged"
    );
}

#[test]
fn management_needs_a_backend() {
    let dir = TempDir::new("no-backend");
    let core = SonorusCore::new(CoreConfig {
        data_dir: dir.path().join("data").to_string_lossy().into_owned(),
        s3: None,
        local_music_dir: None,
    })
    .expect("open");
    assert!(matches!(
        core.browse(String::new()),
        Err(CoreError::NotConfigured)
    ));
    assert!(matches!(
        core.delete_objects(vec![FLAC.into()]),
        Err(CoreError::NotConfigured)
    ));
}
