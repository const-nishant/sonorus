//! Tag, property and cover-art extraction via lofty.

use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{Read, Seek};

use lofty::config::ParseOptions;
#[cfg(test)]
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, Tag};

use crate::error::{LibraryError, Result};
use crate::model::{CoverImage, TrackMeta};

const DEFAULT_COVER_MIME: &str = "image/jpeg";

/// Folder cover names in priority order (lowercase).
const FOLDER_COVER_NAMES: [&str; 6] = [
    "cover.jpg",
    "cover.png",
    "folder.jpg",
    "folder.png",
    "front.jpg",
    "front.png",
];

fn tag_error(e: impl std::fmt::Display) -> LibraryError {
    LibraryError::Tags(e.to_string())
}

/// Trims `s`; returns `None` when nothing is left.
fn text(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

/// Parses the leading four ASCII digits of `s` as a year.
fn leading_year(s: &str) -> Option<i32> {
    let bytes = s.trim().as_bytes();
    if bytes.len() >= 4 && bytes[..4].iter().all(u8::is_ascii_digit) {
        std::str::from_utf8(&bytes[..4]).ok()?.parse().ok()
    } else {
        None
    }
}

/// Maps a file-type hint from the object extension to a lofty `FileType`.
fn file_type_from_hint(ext: &str) -> Option<FileType> {
    match ext.to_ascii_lowercase().as_str() {
        "flac" => Some(FileType::Flac),
        "m4a" => Some(FileType::Mp4),
        "wav" => Some(FileType::Wav),
        "aif" | "aiff" => Some(FileType::Aiff),
        _ => None,
    }
}

/// Codec name stored for a file. MP4 is a container: lofty only reports a bit depth for its
/// lossless streams (ALAC, or FLAC in MP4), so an MP4 without one is AAC (or the rare MP3-in-MP4,
/// which is just as lossy).
fn codec_name(file_type: FileType, bit_depth: Option<u8>) -> String {
    match file_type {
        FileType::Flac => "FLAC".to_owned(),
        FileType::Mp4 if bit_depth.is_some() => "ALAC".to_owned(),
        FileType::Mp4 => "AAC".to_owned(),
        FileType::Wav => "WAV".to_owned(),
        FileType::Aiff => "AIFF".to_owned(),
        other => format!("{other:?}").to_uppercase(),
    }
}

/// Largest accepted ReplayGain gain magnitude, in dB.
const MAX_GAIN_DB: f32 = 64.0;
/// Largest accepted ReplayGain peak (linear amplitude; 1.0 is full scale).
const MAX_PEAK: f32 = 10.0;

/// Removes a case-insensitive `dB` suffix and the whitespace before it.
fn strip_db_suffix(s: &str) -> &str {
    match s.len().checked_sub(2).and_then(|i| s.get(i..)) {
        Some(tail) if tail.eq_ignore_ascii_case("db") => s[..s.len() - 2].trim_end(),
        _ => s,
    }
}

/// Parses a ReplayGain gain value such as `"-6.54 dB"`, `"+2.10 dB"`, `"-6.54"` or `"−6.54 dB"`
/// (with a Unicode minus sign). Surrounding whitespace and a case-insensitive `dB` suffix are
/// optional. Returns `None` for text that is not a number, for non-finite values, and for gains
/// outside ±64 dB.
pub fn parse_gain_db(s: &str) -> Option<f32> {
    let number = strip_db_suffix(s.trim()).trim();
    let value: f32 = number.replace('\u{2212}', "-").parse().ok()?;
    (value.is_finite() && value.abs() <= MAX_GAIN_DB).then_some(value)
}

/// Parses a ReplayGain peak such as `"0.988525"` or `"1.000000"`. Returns `None` for text that is
/// not a number, for non-finite values, and for peaks that are not in `(0, 10]`.
pub fn parse_peak(s: &str) -> Option<f32> {
    let value: f32 = s.trim().parse().ok()?;
    (value.is_finite() && value > 0.0 && value <= MAX_PEAK).then_some(value)
}

fn tag_text(tag: Option<&Tag>, get: impl Fn(&Tag) -> Option<Cow<'_, str>>) -> Option<String> {
    tag.and_then(get).and_then(|s| text(&s))
}

fn tag_item(tag: Option<&Tag>, key: ItemKey) -> Option<String> {
    tag.and_then(|t| t.get_string(key)).and_then(text)
}

/// Reads tags, technical properties and embedded cover art from an audio file.
///
/// `ext_hint` is the object's extension (e.g. `"flac"`); it is only used when lofty cannot
/// identify the format from the content. Strings are trimmed and empty strings become `None`.
pub fn read_meta<R: Read + Seek>(reader: R, ext_hint: Option<&str>) -> Result<TrackMeta> {
    let options = ParseOptions::new().read_cover_art(true);
    let mut probe = Probe::new(reader)
        .options(options)
        .guess_file_type()
        .map_err(tag_error)?;
    if probe.file_type().is_none()
        && let Some(file_type) = ext_hint.and_then(file_type_from_hint)
    {
        probe = probe.set_file_type(file_type);
    }
    let tagged = probe.read().map_err(tag_error)?;

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let props = tagged.properties();

    let year = tag
        .and_then(|t| t.date())
        .map(|d| i32::from(d.year))
        .or_else(|| {
            tag_item(tag, ItemKey::Year)
                .or_else(|| tag_item(tag, ItemKey::RecordingDate))
                .and_then(|s| leading_year(&s))
        });

    let lyrics = tag_item(tag, ItemKey::Lyrics).or_else(|| tag_item(tag, ItemKey::UnsyncLyrics));

    // lofty maps ID3v2 `TXXX` frames to these keys by their description (case-insensitively),
    // so MP3 files need no separate lookup.
    let gain = |key| tag_item(tag, key).and_then(|s| parse_gain_db(&s));
    let peak = |key| tag_item(tag, key).and_then(|s| parse_peak(&s));

    let cover = tag.and_then(|t| {
        let pictures = t.pictures();
        pictures
            .iter()
            .find(|p| p.pic_type() == PictureType::CoverFront)
            .or_else(|| pictures.first())
            .map(|p| CoverImage {
                data: p.data().to_vec(),
                mime: p
                    .mime_type()
                    .map(|m| m.as_str())
                    .and_then(text)
                    .unwrap_or_else(|| DEFAULT_COVER_MIME.to_owned()),
            })
    });

    Ok(TrackMeta {
        title: tag_text(tag, |t| t.title()),
        artist: tag_text(tag, |t| t.artist()),
        album_artist: tag_item(tag, ItemKey::AlbumArtist),
        album: tag_text(tag, |t| t.album()),
        genre: tag_text(tag, |t| t.genre()),
        lyrics,
        codec: Some(codec_name(tagged.file_type(), props.bit_depth())),
        track_no: tag.and_then(|t| t.track()),
        track_total: tag.and_then(|t| t.track_total()),
        disc_no: tag.and_then(|t| t.disk()),
        disc_total: tag.and_then(|t| t.disk_total()),
        year,
        duration_ms: u64::try_from(props.duration().as_millis())
            .ok()
            .filter(|ms| *ms > 0),
        sample_rate: props.sample_rate(),
        bits_per_sample: props.bit_depth(),
        channels: props.channels(),
        bitrate_kbps: props.audio_bitrate(),
        cover,
        track_gain_db: gain(ItemKey::ReplayGainTrackGain),
        track_peak: peak(ItemKey::ReplayGainTrackPeak),
        album_gain_db: gain(ItemKey::ReplayGainAlbumGain),
        album_peak: peak(ItemKey::ReplayGainAlbumPeak),
    })
}

/// Finds a folder-level cover image for `audio_key` among `all_keys`.
///
/// Looks only in the same folder (the text up to and including the last `/`) for `cover.jpg`,
/// `cover.png`, `folder.jpg`, `folder.png`, `front.jpg`, `front.png` (case-insensitive), in that
/// priority order. Returns the key exactly as it appears in `all_keys`.
pub fn folder_cover_key(audio_key: &str, all_keys: &HashSet<String>) -> Option<String> {
    let folder = audio_key.rfind('/').map_or("", |i| &audio_key[..=i]);
    let mut best: Option<(usize, &String)> = None;
    for key in all_keys {
        let Some(rest) = key.strip_prefix(folder) else {
            continue;
        };
        if rest.contains('/') {
            continue;
        }
        let lower = rest.to_ascii_lowercase();
        if let Some(rank) = FOLDER_COVER_NAMES.iter().position(|name| *name == lower)
            && best.is_none_or(|(r, _)| rank < r)
        {
            best = Some((rank, key));
        }
    }
    best.map(|(_, key)| key.clone())
}

/// Writes `items` into the primary tag of the audio file at `path` (creating the tag if the file
/// has none), keeping the file's other tags. Used by tests to build tagged copies of fixtures.
#[cfg(test)]
pub(crate) fn write_test_tags(path: &std::path::Path, items: &[(ItemKey, &str)]) {
    let mut tagged = lofty::read_from_path(path).expect("read fixture for tagging");
    let tag_type = tagged.primary_tag_type();
    if tagged.tag_mut(tag_type).is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged.tag_mut(tag_type).expect("tag was just inserted");
    for (key, value) in items {
        tag.insert_text(*key, (*value).to_owned());
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .expect("write tags");
}

/// A silent MPEG-1 Layer III stream (128 kbps, 44.1 kHz, stereo) of `frames` frames, without tags.
#[cfg(test)]
pub(crate) fn silent_mp3(frames: usize) -> Vec<u8> {
    const FRAME_LEN: usize = 417; // 144 * 128000 / 44100
    let mut out = Vec::with_capacity(FRAME_LEN * frames);
    for _ in 0..frames {
        let mut frame = vec![0u8; FRAME_LEN];
        frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        out.extend_from_slice(&frame);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn mp4_codec_follows_the_bit_depth() {
        assert_eq!(codec_name(FileType::Mp4, Some(24)), "ALAC");
        assert_eq!(codec_name(FileType::Mp4, None), "AAC");
        assert_eq!(codec_name(FileType::Flac, None), "FLAC");
        assert_eq!(codec_name(FileType::Mpeg, None), "MPEG");
    }

    #[test]
    fn leading_year_parses_full_and_partial_dates() {
        assert_eq!(leading_year("2021"), Some(2021));
        assert_eq!(leading_year(" 2021-05-01T10:00 "), Some(2021));
        assert_eq!(leading_year("21"), None);
        assert_eq!(leading_year("abcd"), None);
        assert_eq!(leading_year(""), None);
    }

    #[test]
    fn hints_map_to_file_types() {
        assert_eq!(file_type_from_hint("FLAC"), Some(FileType::Flac));
        assert_eq!(file_type_from_hint("m4a"), Some(FileType::Mp4));
        assert_eq!(file_type_from_hint("wav"), Some(FileType::Wav));
        assert_eq!(file_type_from_hint("aif"), Some(FileType::Aiff));
        assert_eq!(file_type_from_hint("aiff"), Some(FileType::Aiff));
        assert_eq!(file_type_from_hint("mp3"), None);
    }

    #[test]
    fn folder_cover_prefers_priority_order() {
        let all = keys(&[
            "Artist/Album/front.png",
            "Artist/Album/Folder.JPG",
            "Artist/Album/01 - x.flac",
        ]);
        assert_eq!(
            folder_cover_key("Artist/Album/01 - x.flac", &all),
            Some("Artist/Album/Folder.JPG".to_owned())
        );

        let all = keys(&["Artist/Album/Cover.PNG", "Artist/Album/cover.jpg"]);
        assert_eq!(
            folder_cover_key("Artist/Album/02.m4a", &all),
            Some("Artist/Album/cover.jpg".to_owned())
        );
    }

    #[test]
    fn folder_cover_ignores_other_folders_and_subfolders() {
        let all = keys(&[
            "Artist/cover.jpg",
            "Artist/Album/Disc 1/cover.jpg",
            "Artist/Album2/cover.jpg/",
        ]);
        assert_eq!(folder_cover_key("Artist/Album/01.flac", &all), None);
        assert_eq!(
            folder_cover_key("Artist/Album2/01.flac", &all),
            None,
            "directory markers are not covers"
        );
        assert_eq!(
            folder_cover_key("Artist/Album/Disc 1/01.flac", &all),
            Some("Artist/Album/Disc 1/cover.jpg".to_owned())
        );
    }

    #[test]
    fn folder_cover_root_level_audio() {
        let all = keys(&["Artist/cover.jpg", "cover.png"]);
        assert_eq!(
            folder_cover_key("song.flac", &all),
            Some("cover.png".to_owned())
        );
        assert_eq!(folder_cover_key("song.flac", &keys(&["x/cover.png"])), None);
    }

    #[test]
    fn gain_parses_common_spellings() {
        assert_eq!(parse_gain_db("-6.54 dB"), Some(-6.54));
        assert_eq!(parse_gain_db("+2.10 dB"), Some(2.1));
        assert_eq!(parse_gain_db("-6.54"), Some(-6.54));
        assert_eq!(parse_gain_db("\u{2212}6.54 dB"), Some(-6.54));
        assert_eq!(parse_gain_db("  -6.54 DB  "), Some(-6.54));
        assert_eq!(parse_gain_db("-6.54dB"), Some(-6.54));
        assert_eq!(parse_gain_db("0.00 dB"), Some(0.0));
        assert_eq!(parse_gain_db("64 dB"), Some(64.0));
        assert_eq!(parse_gain_db("-64.0 dB"), Some(-64.0));
    }

    #[test]
    fn gain_rejects_bad_values() {
        assert_eq!(parse_gain_db(""), None);
        assert_eq!(parse_gain_db("dB"), None);
        assert_eq!(parse_gain_db("   "), None);
        assert_eq!(parse_gain_db("loud"), None);
        assert_eq!(parse_gain_db("inf dB"), None);
        assert_eq!(parse_gain_db("NaN"), None);
        assert_eq!(parse_gain_db("65 dB"), None);
        assert_eq!(parse_gain_db("-64.01 dB"), None);
        assert_eq!(parse_gain_db("1e9"), None);
    }

    #[test]
    fn peak_parses_and_rejects() {
        assert_eq!(parse_peak("0.988525"), Some(0.988525));
        assert_eq!(parse_peak(" 1.000000 "), Some(1.0));
        assert_eq!(parse_peak("10"), Some(10.0));
        assert_eq!(parse_peak("0"), None);
        assert_eq!(parse_peak("-0.5"), None);
        assert_eq!(parse_peak("10.5"), None);
        assert_eq!(parse_peak("inf"), None);
        assert_eq!(parse_peak("NaN"), None);
        assert_eq!(parse_peak("0.5 dB"), None);
        assert_eq!(parse_peak(""), None);
    }

    #[test]
    fn folder_cover_none_when_absent() {
        let all = keys(&["Artist/Album/01.flac", "Artist/Album/notes.txt"]);
        assert_eq!(folder_cover_key("Artist/Album/02.flac", &all), None);
    }
}

#[cfg(test)]
mod fixture_tests {
    use super::*;
    use std::path::PathBuf;

    fn read_fixture(rel: &str) -> TrackMeta {
        let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/")).join(rel);
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_owned);
        let file = std::fs::File::open(&path).expect("open fixture");
        read_meta(file, ext.as_deref()).expect("read_meta")
    }

    #[test]
    fn flac_full_tags_and_cover() {
        let m = read_fixture("Artist A/Album One/01 - First Song.flac");
        assert_eq!(m.title.as_deref(), Some("First Song"));
        assert_eq!(m.artist.as_deref(), Some("Artist A"));
        assert_eq!(m.album_artist.as_deref(), Some("Artist A"));
        assert_eq!(m.album.as_deref(), Some("Album One"));
        assert_eq!(m.genre.as_deref(), Some("Ambient"));
        assert_eq!(m.lyrics.as_deref(), Some("[00:00.00]hello"));
        assert_eq!(m.track_no, Some(1));
        assert_eq!(m.track_total, Some(2));
        assert_eq!(m.disc_no, Some(1));
        assert_eq!(m.disc_total, Some(1));
        assert_eq!(m.year, Some(2021));
        assert_eq!(m.codec.as_deref(), Some("FLAC"));
        assert_eq!(m.sample_rate, Some(96000));
        assert_eq!(m.bits_per_sample, Some(24));
        assert_eq!(m.channels, Some(2));
        assert!(m.bitrate_kbps.is_some());
        let duration = m.duration_ms.expect("duration");
        assert!((250..=350).contains(&duration), "duration {duration}");
        let cover = m.cover.expect("embedded cover");
        assert_eq!(cover.mime, "image/jpeg");
        assert!(cover.data.starts_with(&[0xFF, 0xD8, 0xFF]), "JPEG magic");
    }

    #[test]
    fn m4a_alac_tags_and_cover() {
        let m = read_fixture("Artist A/Album One/02 - Second Song.m4a");
        assert_eq!(m.title.as_deref(), Some("Second Song"));
        assert_eq!(m.artist.as_deref(), Some("Artist A feat. C"));
        assert_eq!(m.album_artist.as_deref(), Some("Artist A"));
        assert_eq!(m.album.as_deref(), Some("Album One"));
        assert_eq!(m.track_no, Some(2));
        assert_eq!(m.track_total, Some(2));
        assert_eq!(m.disc_no, None);
        assert_eq!(m.year, Some(2021));
        assert_eq!(m.codec.as_deref(), Some("ALAC"));
        assert_eq!(m.sample_rate, Some(96000));
        assert_eq!(m.bits_per_sample, Some(24));
        assert_eq!(m.channels, Some(2));
        let duration = m.duration_ms.expect("duration");
        assert!((250..=350).contains(&duration), "duration {duration}");
        let cover = m.cover.expect("embedded cover");
        assert_eq!(cover.mime, "image/jpeg");
        assert!(!cover.data.is_empty());
    }

    #[test]
    fn wav_info_tags_no_cover() {
        let m = read_fixture("Artist B/Album Two/01 - Wave Song.wav");
        assert_eq!(m.title.as_deref(), Some("Wave Song"));
        assert_eq!(m.artist.as_deref(), Some("Artist B"));
        assert_eq!(m.album.as_deref(), Some("Album Two"));
        assert_eq!(m.album_artist, None);
        assert_eq!(m.track_no, Some(1));
        assert_eq!(m.codec.as_deref(), Some("WAV"));
        assert_eq!(m.sample_rate, Some(44100));
        assert_eq!(m.bits_per_sample, Some(16));
        assert_eq!(m.channels, Some(2));
        let duration = m.duration_ms.expect("duration");
        assert!((250..=350).contains(&duration), "duration {duration}");
        assert!(m.cover.is_none());
    }

    #[test]
    fn untagged_flac_has_properties_only() {
        let m = read_fixture("Artist B/Album Two/03 - Untagged Track.flac");
        assert_eq!(m.title, None);
        assert_eq!(m.artist, None);
        assert_eq!(m.album_artist, None);
        assert_eq!(m.album, None);
        assert_eq!(m.track_no, None);
        assert_eq!(m.year, None);
        assert_eq!(m.lyrics, None);
        assert_eq!(m.codec.as_deref(), Some("FLAC"));
        assert_eq!(m.sample_rate, Some(44100));
        assert_eq!(m.bits_per_sample, Some(16));
        assert_eq!(m.channels, Some(2));
        assert!(m.duration_ms.is_some());
        assert!(m.cover.is_none());
        assert_eq!(m.track_gain_db, None);
        assert_eq!(m.track_peak, None);
        assert_eq!(m.album_gain_db, None);
        assert_eq!(m.album_peak, None);
    }

    const REPLAYGAIN: [(ItemKey, &str); 4] = [
        (ItemKey::ReplayGainTrackGain, "-6.54 dB"),
        (ItemKey::ReplayGainTrackPeak, "0.988525"),
        (ItemKey::ReplayGainAlbumGain, "+2.10 dB"),
        (ItemKey::ReplayGainAlbumPeak, "1.000000"),
    ];

    fn assert_replaygain(m: &TrackMeta) {
        assert_close(m.track_gain_db, -6.54);
        assert_close(m.track_peak, 0.988_525);
        assert_close(m.album_gain_db, 2.10);
        assert_close(m.album_peak, 1.0);
    }

    fn assert_close(actual: Option<f32>, expected: f32) {
        let actual = actual.expect("replaygain value");
        assert!((actual - expected).abs() < 1e-4, "{actual} != {expected}");
    }

    #[test]
    fn replaygain_tags_are_read_from_flac() {
        let dir = crate::db::test_support::TempDir::new();
        let path = dir.path().join("replaygain.flac");
        let src = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/Artist A/Album One/01 - First Song.flac"
        );
        std::fs::copy(src, &path).expect("copy fixture");
        write_test_tags(&path, &REPLAYGAIN);

        let file = std::fs::File::open(&path).expect("open tagged copy");
        let m = read_meta(file, Some("flac")).expect("read_meta");
        assert_replaygain(&m);
        assert_eq!(
            m.title.as_deref(),
            Some("First Song"),
            "other tags are kept"
        );
    }

    #[test]
    fn replaygain_tags_are_read_from_mp3_txxx_frames() {
        let dir = crate::db::test_support::TempDir::new();
        let path = dir.path().join("replaygain.mp3");
        std::fs::write(&path, silent_mp3(40)).expect("write mp3");
        write_test_tags(&path, &REPLAYGAIN);

        let file = std::fs::File::open(&path).expect("open tagged mp3");
        let m = read_meta(file, Some("mp3")).expect("read_meta");
        assert_replaygain(&m);
    }

    #[test]
    fn replaygain_invalid_values_are_ignored() {
        let dir = crate::db::test_support::TempDir::new();
        let path = dir.path().join("bad.flac");
        let src = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/Artist A/Album One/01 - First Song.flac"
        );
        std::fs::copy(src, &path).expect("copy fixture");
        write_test_tags(
            &path,
            &[
                (ItemKey::ReplayGainTrackGain, "loud"),
                (ItemKey::ReplayGainTrackPeak, "0"),
                (ItemKey::ReplayGainAlbumGain, "200 dB"),
                (ItemKey::ReplayGainAlbumPeak, "11.0"),
            ],
        );
        let file = std::fs::File::open(&path).expect("open tagged copy");
        let m = read_meta(file, Some("flac")).expect("read_meta");
        assert_eq!(m.track_gain_db, None);
        assert_eq!(m.track_peak, None);
        assert_eq!(m.album_gain_db, None);
        assert_eq!(m.album_peak, None);
    }

    #[test]
    fn extension_hint_is_ignored_when_content_is_recognised() {
        let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/"))
            .join("Artist B/Album Two/01 - Wave Song.wav");
        let file = std::fs::File::open(&path).expect("open fixture");
        let m = read_meta(file, Some("flac")).expect("read_meta");
        assert_eq!(m.codec.as_deref(), Some("WAV"));
    }

    #[test]
    fn garbage_without_hint_is_an_error() {
        let junk = std::io::Cursor::new(vec![0u8; 64]);
        assert!(matches!(read_meta(junk, None), Err(LibraryError::Tags(_))));
    }
}
