//! Integration tests for `SonorusCore`: scanning and querying the local fixtures, playback with
//! the null sink, playlists and favorites, and the unconfigured core.
//!
//! These are plain `#[test]`s. The core owns its tokio runtime and must not be dropped inside
//! another runtime, so no test here is async.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sonorus_core::types::{
    AlbumInfo, AlbumSort, CoreConfig, CoreEvent, PlaybackState, S3Settings, TrackInfo,
};
use sonorus_core::{CoreError, EventListener, SonorusCore};

/// Fixture library shared with the library crate: two albums, four tracks, one cover file.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../library/tests/fixtures");
/// How long a test waits for an event before failing.
const WAIT: Duration = Duration::from_secs(10);
/// Polling interval of `Recorder::wait_for`.
const POLL: Duration = Duration::from_millis(20);

/// A directory under the system temp dir, removed when dropped (even on a panic).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "sonorus-core-it-{tag}-{}-{nanos}",
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

/// Stores every event it receives.
#[derive(Default)]
struct Recorder(Mutex<Vec<CoreEvent>>);

impl Recorder {
    fn events(&self) -> Vec<CoreEvent> {
        self.0.lock().expect("recorder lock").clone()
    }

    /// Polls until an event matches `pred`, then returns it. Fails the test after `WAIT`.
    fn wait_for(&self, pred: impl Fn(&CoreEvent) -> bool, label: &str) -> CoreEvent {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(found) = self.events().into_iter().find(|e| pred(e)) {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {label}; events so far: {:#?}",
                self.events()
            );
            thread::sleep(POLL);
        }
    }
}

impl EventListener for Recorder {
    fn on_event(&self, event: CoreEvent) {
        self.0.lock().expect("recorder lock").push(event);
    }
}

fn config(data: &Path, local_music_dir: Option<&str>) -> CoreConfig {
    CoreConfig {
        data_dir: data.to_string_lossy().into_owned(),
        s3: None,
        local_music_dir: local_music_dir.map(str::to_owned),
    }
}

/// Opens a core over `dir` and records its events.
fn open(dir: &TempDir, local_music_dir: Option<&str>) -> (Arc<SonorusCore>, Arc<Recorder>) {
    let core = SonorusCore::new(config(dir.path(), local_music_dir)).expect("open core");
    let rec = Arc::new(Recorder::default());
    core.set_listener(rec.clone());
    (core, rec)
}

fn album_by_title(core: &SonorusCore, title: &str) -> AlbumInfo {
    core.albums(AlbumSort::Title, 50, 0)
        .expect("albums")
        .into_iter()
        .find(|a| a.title == title)
        .unwrap_or_else(|| panic!("album {title:?} not found"))
}

/// Writes a silent 16-bit stereo 44.1 kHz WAV of `seconds` length.
fn write_silent_wav(path: &Path, seconds: u32) {
    let rate: u32 = 44_100;
    let channels: u16 = 2;
    let bits: u16 = 16;
    let block_align = channels * bits / 8;
    let data_len = rate * u32::from(block_align) * seconds;
    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVEfmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&rate.to_le_bytes());
    buf.extend_from_slice(&(rate * u32::from(block_align)).to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    buf.resize(44 + data_len as usize, 0);
    fs::write(path, buf).expect("write wav");
}

#[test]
fn scan_fixtures_then_query() {
    let dir = TempDir::new("scan");
    let (core, rec) = open(&dir, Some(FIXTURES));

    let summary = core.scan_library().expect("scan");
    assert_eq!(summary.progress.added, 4, "{summary:?}");
    assert!(summary.failures.is_empty(), "{summary:?}");

    let events = rec.events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::ScanProgress { .. })),
        "no ScanProgress in {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::ScanFinished { .. })),
        "no ScanFinished in {events:?}"
    );

    let albums = core.albums(AlbumSort::Title, 50, 0).expect("albums");
    let one = album_by_title(&core, "Album One");
    assert_eq!(one.track_count, 2);
    let cover = one.cover_path.as_deref().expect("Album One has a cover");
    assert!(Path::new(cover).is_file(), "cover file missing: {cover}");
    assert!(albums.iter().any(|a| a.title == "Album Two"));

    let found = core.search("wave".to_owned(), 10).expect("search");
    assert!(
        found.tracks.iter().any(|t| t.key.ends_with(".wav")),
        "{found:?}"
    );

    assert_eq!(core.stats().expect("stats").tracks, 4);

    // The scanning flag is reset: a second sequential scan runs and adds nothing.
    let again = core.scan_library().expect("second scan");
    assert_eq!(again.progress.added, 0);
    assert!(again.failures.is_empty());
}

#[test]
fn playback_plays_album_in_order_and_reports_events() {
    let dir = TempDir::new("play");
    let (core, rec) = open(&dir, Some(FIXTURES));
    core.scan_library().expect("scan");

    let album = album_by_title(&core, "Album One");
    let tracks: Vec<TrackInfo> = core.album_tracks(album.id).expect("album tracks");
    assert_eq!(tracks.len(), 2);
    let (first, second) = (tracks[0].id, tracks[1].id);

    core.play_album(album.id, 0).expect("play album");
    rec.wait_for(
        |e| matches!(e, CoreEvent::TrackChanged { track: Some(t), .. } if t.title == "First Song"),
        "first track",
    );
    rec.wait_for(
        |e| {
            matches!(
                e,
                CoreEvent::StateChanged {
                    state: PlaybackState::Playing
                }
            )
        },
        "playing",
    );
    rec.wait_for(
        |e| matches!(e, CoreEvent::Position { track_id, .. } if *track_id == first),
        "position of the first track",
    );
    rec.wait_for(
        |e| matches!(e, CoreEvent::TrackEnded { track_id } if *track_id == first),
        "first track ended",
    );
    rec.wait_for(
        |e| matches!(e, CoreEvent::TrackEnded { track_id } if *track_id == second),
        "second track ended",
    );
    rec.wait_for(
        |e| {
            matches!(
                e,
                CoreEvent::StateChanged {
                    state: PlaybackState::Idle
                }
            )
        },
        "idle",
    );

    let events = rec.events();
    let index_of = |pred: &dyn Fn(&CoreEvent) -> bool| {
        events
            .iter()
            .position(pred)
            .unwrap_or_else(|| panic!("missing event; events: {events:#?}"))
    };
    let first_changed =
        index_of(&|e| matches!(e, CoreEvent::TrackChanged { track: Some(t), .. } if t.id == first));
    let first_ended =
        index_of(&|e| matches!(e, CoreEvent::TrackEnded { track_id } if *track_id == first));
    let second_changed = index_of(
        &|e| matches!(e, CoreEvent::TrackChanged { track: Some(t), .. } if t.id == second),
    );
    let second_ended =
        index_of(&|e| matches!(e, CoreEvent::TrackEnded { track_id } if *track_id == second));
    let idle = index_of(&|e| {
        matches!(
            e,
            CoreEvent::StateChanged {
                state: PlaybackState::Idle
            }
        )
    });
    assert!(
        first_changed < first_ended
            && first_ended < second_changed
            && second_changed < second_ended
            && second_ended < idle,
        "events out of order: {events:#?}"
    );

    // Short fixtures (300 ms, threshold 150 ms) still count: listening is measured from 0.
    let played: Vec<i64> = core
        .recently_played(10)
        .expect("history")
        .iter()
        .map(|t| t.id)
        .collect();
    assert!(!played.is_empty(), "album playback recorded no history");
    assert!(
        played.iter().all(|id| *id == first || *id == second),
        "{played:?}"
    );

    assert!(
        events.iter().any(|e| matches!(e,
            CoreEvent::QueueChanged { items, .. }
                if items.len() == 2 && items[0].track_id == first && items[1].track_id == second)),
        "no two-entry QueueChanged in {events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::PlaybackError { .. })),
        "playback failed: {events:#?}"
    );
}

#[test]
fn playlists_and_favorites_round_trip() {
    let dir = TempDir::new("lists");
    let (core, _rec) = open(&dir, Some(FIXTURES));
    core.scan_library().expect("scan");

    let album = album_by_title(&core, "Album Two");
    let tracks = core.album_tracks(album.id).expect("album tracks");
    assert_eq!(tracks.len(), 2);
    let (a, b) = (tracks[0].id, tracks[1].id);

    // Playlists.
    let playlist = core
        .create_playlist("Mix".to_owned())
        .expect("create playlist");
    assert_eq!(playlist.track_count, 0);
    core.add_to_playlist(playlist.id, vec![a, 999_999, b])
        .expect("add to playlist");
    let entries = core.playlist_entries(playlist.id).expect("entries");
    assert_eq!(entries.len(), 2, "unknown id is skipped: {entries:?}");
    assert_eq!(entries[0].track.as_ref().map(|t| t.id), Some(a));
    assert_eq!(entries[1].track.as_ref().map(|t| t.id), Some(b));
    assert!(matches!(
        core.add_to_playlist(playlist.id, vec![999_999]),
        Err(CoreError::NotFound { .. })
    ));
    let listed = core.playlists().expect("playlists");
    assert!(
        listed
            .iter()
            .any(|p| p.id == playlist.id && p.track_count == 2),
        "{listed:?}"
    );
    core.rename_playlist(playlist.id, "Renamed".to_owned())
        .expect("rename");
    core.remove_from_playlist(playlist.id, 0)
        .expect("remove slot");
    let entries = core.playlist_entries(playlist.id).expect("entries");
    assert_eq!(entries.len(), 1);
    core.delete_playlist(playlist.id).expect("delete");
    assert!(
        !core
            .playlists()
            .expect("playlists")
            .iter()
            .any(|p| p.id == playlist.id)
    );

    // Favorites.
    assert!(matches!(
        core.set_favorite(999_999, true),
        Err(CoreError::NotFound { .. })
    ));
    core.set_favorite(a, true).expect("favorite");
    let favorites = core.favorites().expect("favorites");
    assert_eq!(favorites.len(), 1);
    assert_eq!(favorites[0].id, a);
    assert!(favorites[0].is_favorite);

    let tracks = core.album_tracks(album.id).expect("album tracks");
    let flagged = tracks.iter().find(|t| t.id == a).expect("track a");
    assert!(flagged.is_favorite);
    let other = tracks.iter().find(|t| t.id == b).expect("track b");
    assert!(!other.is_favorite);

    core.set_favorite(a, false).expect("unfavorite");
    assert!(core.favorites().expect("favorites").is_empty());
}

#[test]
fn unconfigured_core_refuses_backend_work() {
    let dir = TempDir::new("unconfigured");
    let core = SonorusCore::new(config(dir.path(), None)).expect("open core");
    assert!(!core.is_configured());

    assert!(matches!(core.scan_library(), Err(CoreError::NotConfigured)));
    assert!(matches!(
        core.play_tracks(vec![1], 0),
        Err(CoreError::NotConfigured)
    ));
    assert!(matches!(
        core.append(vec![1]),
        Err(CoreError::NotConfigured)
    ));
    // Transport calls are silent no-ops without a player.
    core.pause();
    core.play();
    core.stop();
    assert!(
        core.albums(AlbumSort::Artist, 10, 0)
            .expect("albums")
            .is_empty()
    );

    let empty_bucket = S3Settings {
        endpoint: "https://s3.us-west-004.backblazeb2.com".to_owned(),
        region: String::new(),
        bucket: "  ".to_owned(),
        access_key_id: "FAKEKEYID0000000000".to_owned(),
        secret_access_key: "fake-secret".to_owned(),
        path_style: true,
        prefix: String::new(),
    };
    assert!(matches!(
        core.configure_s3(empty_bucket),
        Err(CoreError::InvalidConfig { .. })
    ));
    assert!(!core.is_configured());

    // Plausible settings: stored without touching the network (no scan or play is made).
    let settings = S3Settings {
        endpoint: "https://s3.us-west-004.backblazeb2.com".to_owned(),
        region: String::new(),
        bucket: "music".to_owned(),
        access_key_id: "FAKEKEYID0000000000".to_owned(),
        secret_access_key: "fake-secret".to_owned(),
        path_style: true,
        prefix: "/library/".to_owned(),
    };
    core.configure_s3(settings).expect("configure bucket");
    assert!(core.is_configured());

    assert!(matches!(
        core.configure_local("/definitely/not/a/sonorus/dir".to_owned()),
        Err(CoreError::InvalidConfig { .. })
    ));
    assert!(core.is_configured(), "a rejected local dir changes nothing");
    core.shutdown();
    core.shutdown(); // idempotent
}

#[test]
fn local_backend_has_no_user_data_sync() {
    let dir = TempDir::new("sync");
    let (core, _rec) = open(&dir, Some(FIXTURES));
    assert!(matches!(
        core.sync_user_data(),
        Err(CoreError::NotConfigured)
    ));
}

#[test]
fn long_play_is_recorded_in_history() {
    let dir = TempDir::new("history");
    let music = dir.path().join("music");
    fs::create_dir_all(&music).expect("create music dir");
    // Two seconds: the play threshold is one second (half the duration).
    write_silent_wav(&music.join("Tone.wav"), 2);
    let (core, rec) = open(&dir, Some(music.to_str().expect("utf-8 path")));

    let summary = core.scan_library().expect("scan");
    assert_eq!(summary.progress.added, 1, "{summary:?}");
    let album = core
        .albums(AlbumSort::Title, 10, 0)
        .expect("albums")
        .into_iter()
        .next()
        .expect("one album");
    let track = core
        .album_tracks(album.id)
        .expect("album tracks")
        .into_iter()
        .next()
        .expect("one track");
    assert!(core.recently_played(10).expect("history").is_empty());

    core.play_tracks(vec![track.id], 0).expect("play");
    rec.wait_for(
        |e| matches!(e, CoreEvent::TrackEnded { track_id } if *track_id == track.id),
        "track ended",
    );

    // The pump records the play before it forwards TrackEnded, so the history is up to date.
    let history = core.recently_played(10).expect("history");
    assert_eq!(
        history.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![track.id]
    );
    let recent: Vec<AlbumInfo> = core.recent_albums(10).expect("recent albums");
    assert_eq!(
        recent.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![album.id]
    );

    core.clear_history().expect("clear history");
    assert!(core.recently_played(10).expect("history").is_empty());
}
