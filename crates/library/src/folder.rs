//! Folder view of the catalogue: the folders and tracks directly under a path, derived from the
//! object keys. The scan prefix is not stored in the library, so callers pass it in.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};

use crate::db::Library;
use crate::error::{LibraryError, Result};
use crate::model::Track;
use crate::query::{TRACK_SELECT, row_to_track};

/// One folder directly under a catalogue folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEntry {
    /// The folder's own name (the last path segment).
    pub name: String,
    /// The folder's path relative to the scan prefix, with a trailing `/`.
    pub path: String,
    /// Number of catalogued tracks under this folder, at any depth.
    pub track_count: u64,
}

/// The contents of one catalogue folder: its child folders and the tracks directly in it.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogueFolder {
    /// The folder path, normalized: empty for the root, otherwise with a trailing `/`.
    pub path: String,
    /// Child folders, sorted by name, case-insensitively.
    pub folders: Vec<FolderEntry>,
    /// Tracks directly in this folder, sorted by object key.
    pub tracks: Vec<Track>,
}

/// Escape character for the `LIKE` patterns below. `!` is used so that backslashes in names
/// need no escaping of their own.
const LIKE_ESCAPE: char = '!';

/// Normalizes a folder path: `""` is the root; anything else must be relative, with no empty,
/// `.` or `..` segments, and gets a trailing `/`.
fn normalize_path(path: &str) -> Result<String> {
    if path.is_empty() {
        return Ok(String::new());
    }
    if path.starts_with('/') {
        return Err(LibraryError::InvalidPath(format!("{path:?} is absolute")));
    }
    let trimmed = path.strip_suffix('/').unwrap_or(path);
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(LibraryError::InvalidPath(format!(
                "{path:?} has an empty, '.' or '..' segment"
            )));
        }
    }
    Ok(format!("{trimmed}/"))
}

/// Escapes `%`, `_` and the escape character itself for a `LIKE ... ESCAPE '!'` pattern.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '%' || c == '_' || c == LIKE_ESCAPE {
            out.push(LIKE_ESCAPE);
        }
        out.push(c);
    }
    out
}

impl Library {
    /// The folders and tracks directly under `path`, for the catalogue whose object keys start
    /// with `prefix` (the scan prefix: empty for a local folder, or the bucket prefix).
    ///
    /// `path` is relative to `prefix`, with `/` separators: `""` is the root and
    /// `"Artist A/Album One/"` is a folder. A missing trailing `/` is added. Absolute paths and
    /// paths with empty, `.` or `..` segments are rejected with `LibraryError::InvalidPath`.
    /// Blocking: one `LIKE` query over the keys, grouped in Rust.
    pub fn catalogue_folder(&self, prefix: &str, path: &str) -> Result<CatalogueFolder> {
        let path = normalize_path(path)?;
        let full = format!("{prefix}{path}");
        let pattern = format!("{}%", like_escape(&full));

        let keys: Vec<String> = {
            let conn = self.lock();
            let mut stmt = conn.prepare(&format!(
                "SELECT t.key FROM tracks t WHERE t.key LIKE ?1 ESCAPE '{LIKE_ESCAPE}'"
            ))?;
            let rows = stmt.query_map(params![pattern], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        // LIKE is ASCII case-insensitive, so the candidates are checked against the exact
        // prefix here. Keys that are not under `full` are dropped.
        let mut counts: HashMap<String, u64> = HashMap::new();
        let mut direct: Vec<String> = Vec::new();
        for key in keys {
            let Some(rest) = key.strip_prefix(full.as_str()) else {
                continue;
            };
            match rest.split_once('/') {
                Some((name, _)) if !name.is_empty() => {
                    *counts.entry(name.to_owned()).or_insert(0) += 1;
                }
                Some(_) => {}
                None if !rest.is_empty() => direct.push(key),
                None => {}
            }
        }

        let mut folders: Vec<FolderEntry> = counts
            .into_iter()
            .map(|(name, track_count)| FolderEntry {
                path: format!("{path}{name}/"),
                name,
                track_count,
            })
            .collect();
        folders.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        });

        direct.sort();
        let mut tracks = Vec::with_capacity(direct.len());
        {
            let conn = self.lock();
            let mut stmt = conn.prepare_cached(&format!("{TRACK_SELECT} WHERE t.key = ?1"))?;
            for key in direct {
                if let Some(track) = stmt.query_row(params![key], row_to_track).optional()? {
                    tracks.push(track);
                }
            }
        }

        Ok(CatalogueFolder {
            path,
            folders,
            tracks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{TempDir, meta, stamp};

    fn open(dir: &TempDir) -> Library {
        Library::open_in_memory(&dir.path().join("covers")).unwrap()
    }

    fn catalogue(lib: &Library, key: &str) {
        lib.upsert_track(&stamp(key, 1), &meta(key, "A", "B"), None)
            .unwrap();
    }

    fn track_keys(f: &CatalogueFolder) -> Vec<&str> {
        f.tracks.iter().map(|t| t.key.as_str()).collect()
    }

    fn folder_names(f: &CatalogueFolder) -> Vec<(&str, &str, u64)> {
        f.folders
            .iter()
            .map(|e| (e.name.as_str(), e.path.as_str(), e.track_count))
            .collect()
    }

    fn library_with(keys: &[&str]) -> (TempDir, Library) {
        let dir = TempDir::new();
        let lib = open(&dir);
        for k in keys {
            catalogue(&lib, k);
        }
        (dir, lib)
    }

    #[test]
    fn root_lists_top_level_folders_and_loose_tracks() {
        let (_dir, lib) = library_with(&[
            "Artist b/Album/01 - x.flac",
            "Artist A/One/01 - a.flac",
            "Artist A/One/02 - b.flac",
            "Artist A/Two/01 - c.flac",
            "Zed/01 - loose.flac",
            "Zed/nested/deep/01 - d.flac",
        ]);
        let root = lib.catalogue_folder("", "").unwrap();
        assert_eq!(root.path, "");
        assert_eq!(
            folder_names(&root),
            vec![
                ("Artist A", "Artist A/", 3),
                ("Artist b", "Artist b/", 1),
                ("Zed", "Zed/", 2),
            ],
            "sorted case-insensitively; counts are recursive"
        );
        assert!(root.tracks.is_empty());
    }

    #[test]
    fn nested_folder_lists_children_and_its_own_tracks() {
        let (_dir, lib) = library_with(&[
            "Zed/02 - two.flac",
            "Zed/01 - one.flac",
            "Zed/b/01 - x.flac",
            "Zed/a/deep/01 - z.flac",
            "Zed/a/02 - y.flac",
            "Zed/B/01 - w.flac",
            "Other/01 - no.flac",
        ]);
        let zed = lib.catalogue_folder("", "Zed/").unwrap();
        assert_eq!(zed.path, "Zed/");
        assert_eq!(
            folder_names(&zed),
            vec![("a", "Zed/a/", 2), ("B", "Zed/B/", 1), ("b", "Zed/b/", 1),],
            "case-insensitive order; equal names tie-break on the exact name"
        );
        assert_eq!(
            track_keys(&zed),
            vec!["Zed/01 - one.flac", "Zed/02 - two.flac"],
            "direct tracks by key"
        );

        let deep = lib.catalogue_folder("", "Zed/a/deep").unwrap();
        assert_eq!(deep.path, "Zed/a/deep/", "trailing slash added");
        assert!(deep.folders.is_empty());
        assert_eq!(track_keys(&deep), vec!["Zed/a/deep/01 - z.flac"]);
    }

    #[test]
    fn unknown_folder_is_empty() {
        let (_dir, lib) = library_with(&["Zed/01 - one.flac"]);
        let none = lib.catalogue_folder("", "Nope/").unwrap();
        assert!(none.folders.is_empty() && none.tracks.is_empty());
    }

    #[test]
    fn prefix_limits_the_folder_view() {
        let (_dir, lib) = library_with(&[
            "music/Artist A/One/01 - a.flac",
            "music/Artist B/Two/01 - b.flac",
            "other/Artist C/01 - c.flac",
        ]);
        let root = lib.catalogue_folder("music/", "").unwrap();
        assert_eq!(
            folder_names(&root),
            vec![("Artist A", "Artist A/", 1), ("Artist B", "Artist B/", 1)]
        );
        let artist = lib.catalogue_folder("music/", "Artist A/One/").unwrap();
        assert_eq!(track_keys(&artist), vec!["music/Artist A/One/01 - a.flac"]);
        // With an empty prefix, "music/" is an ordinary folder of the root.
        let plain = lib.catalogue_folder("", "").unwrap();
        assert_eq!(
            folder_names(&plain),
            vec![("music", "music/", 2), ("other", "other/", 1)]
        );
    }

    #[test]
    fn wildcard_characters_in_names_match_literally() {
        let (_dir, lib) = library_with(&[
            "100% Pure/01 - a.flac",
            "1000 Things/01 - b.flac",
            "a_b/01 - c.flac",
            "axb/01 - d.flac",
            "a_b/Sub_1/01 - e.flac",
            "a_b/Sub%1/02 - f.flac",
            "a_b/Sub!1/03 - g.flac",
            "a_b/SubX1/04 - h.flac",
        ]);
        let root = lib.catalogue_folder("", "").unwrap();
        assert_eq!(
            folder_names(&root),
            vec![
                ("100% Pure", "100% Pure/", 1),
                ("1000 Things", "1000 Things/", 1),
                ("a_b", "a_b/", 5),
                ("axb", "axb/", 1),
            ]
        );
        let pure = lib.catalogue_folder("", "100% Pure/").unwrap();
        assert_eq!(track_keys(&pure), vec!["100% Pure/01 - a.flac"]);

        let ab = lib.catalogue_folder("", "a_b/").unwrap();
        assert_eq!(
            folder_names(&ab),
            vec![
                ("Sub!1", "a_b/Sub!1/", 1),
                ("Sub%1", "a_b/Sub%1/", 1),
                ("Sub_1", "a_b/Sub_1/", 1),
                ("SubX1", "a_b/SubX1/", 1),
            ]
        );
        let sub = lib.catalogue_folder("", "a_b/Sub_1").unwrap();
        assert_eq!(track_keys(&sub), vec!["a_b/Sub_1/01 - e.flac"]);
        assert!(lib.catalogue_folder("", "a%b/").unwrap().folders.is_empty());
    }

    #[test]
    fn wildcard_characters_in_the_prefix_match_literally() {
        let (_dir, lib) = library_with(&["my_music/A/01 - a.flac", "myXmusic/A/01 - b.flac"]);
        let view = lib.catalogue_folder("my_music/", "").unwrap();
        assert_eq!(folder_names(&view), vec![("A", "A/", 1)]);
        assert_eq!(track_keys(&view), Vec::<&str>::new());
        let a = lib.catalogue_folder("my_music/", "A/").unwrap();
        assert_eq!(track_keys(&a), vec!["my_music/A/01 - a.flac"]);
    }

    #[test]
    fn rejects_absolute_and_dot_dot_paths() {
        let (_dir, lib) = library_with(&["Zed/01 - one.flac"]);
        for bad in [
            "/",
            "/Zed",
            "../Zed",
            "Zed/../Zed",
            "Zed/..",
            "./Zed",
            "Zed//x",
            "Zed/./x",
        ] {
            assert!(
                matches!(
                    lib.catalogue_folder("", bad),
                    Err(LibraryError::InvalidPath(_))
                ),
                "{bad:?} must be rejected"
            );
        }
        assert!(lib.catalogue_folder("", "Zed").is_ok());
        assert!(lib.catalogue_folder("", "Zed/").is_ok());
    }

    #[test]
    fn normalize_path_adds_trailing_slash_only_once() {
        assert_eq!(normalize_path("").unwrap(), "");
        assert_eq!(normalize_path("A").unwrap(), "A/");
        assert_eq!(normalize_path("A/").unwrap(), "A/");
        assert_eq!(normalize_path("A/B").unwrap(), "A/B/");
        assert!(normalize_path("/").is_err());
        assert!(normalize_path("A//").is_err());
    }
}
