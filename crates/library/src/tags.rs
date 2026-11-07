//! Tag, property and cover-art extraction via lofty.

use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{Read, Seek};

use lofty::config::ParseOptions;
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

fn codec_name(file_type: FileType) -> String {
    match file_type {
        FileType::Flac => "FLAC".to_owned(),
        FileType::Mp4 => "ALAC".to_owned(),
        FileType::Wav => "WAV".to_owned(),
        FileType::Aiff => "AIFF".to_owned(),
        other => format!("{other:?}").to_uppercase(),
    }
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
        codec: Some(codec_name(tagged.file_type())),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
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
