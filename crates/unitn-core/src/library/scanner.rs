use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::Result;
use lofty::prelude::*;
use lofty::tag::ItemKey;
use walkdir::WalkDir;

use super::Library;
use crate::model::Track;

/// File extensions the playback engine can decode.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "m4a", "mp4", "m4b", "aac", "wav", "aif", "aiff", "caf", "mka",
];

/// Image files commonly used as album art next to the music.
const FOLDER_COVERS: &[&str] = &[
    "cover.jpg", "cover.jpeg", "cover.png", "folder.jpg", "folder.jpeg", "folder.png", "front.jpg",
    "front.png", "Cover.jpg", "Cover.png", "Folder.jpg", "Folder.png", "Front.jpg", "AlbumArt.jpg",
];

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ScanStats {
    /// Audio files found.
    pub scanned: usize,
    /// Files that were new or changed and got (re)indexed.
    pub updated: usize,
    /// Index entries removed because the file is gone.
    pub removed: usize,
}

/// Key used for an album's cached cover.
pub fn album_key(artist: &str, album: &str) -> String {
    format!("{artist}\u{1f}{album}")
}

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// All audio files below `dir`, sorted by path.
pub fn audio_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && is_audio(e.path()))
        .map(|e| e.into_path())
        .collect();
    files.sort();
    files
}

/// Incrementally scans `folders`: unchanged files (same mtime) are skipped,
/// changed/new files are re-read, missing files are dropped from the index.
/// `progress` is called every few files with the number of files seen.
pub fn scan(
    lib: &Library,
    folders: &[PathBuf],
    cover_dir: &Path,
    mut progress: impl FnMut(usize),
) -> Result<ScanStats> {
    fs::create_dir_all(cover_dir)?;
    let known = lib.track_mtimes()?;
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut stats = ScanStats::default();

    lib.begin()?;
    let result = (|| -> Result<()> {
        for folder in folders {
            for entry in WalkDir::new(folder).follow_links(true).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() || !is_audio(entry.path()) {
                    continue;
                }
                let path = entry.path().to_path_buf();
                stats.scanned += 1;
                if stats.scanned % 25 == 0 {
                    progress(stats.scanned);
                }
                let mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let unchanged = known.get(&path) == Some(&mtime);
                seen.insert(path.clone());
                if unchanged {
                    continue;
                }
                let track = read_track(&path, cover_dir);
                lib.upsert_track(&track, mtime)?;
                stats.updated += 1;
                if stats.updated % 200 == 0 {
                    lib.commit()?;
                    lib.begin()?;
                }
            }
        }
        for path in known.keys() {
            if seen.contains(path) {
                continue;
            }
            let in_library = folders.iter().any(|f| path.starts_with(f));
            if !in_library || !path.exists() {
                lib.delete_track(path)?;
                stats.removed += 1;
            }
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            lib.commit()?;
            progress(stats.scanned);
            Ok(stats)
        }
        Err(e) => {
            lib.rollback();
            Err(e)
        }
    }
}

/// Reads tags (and embedded cover art) of one file. Never fails: files with
/// unreadable tags are still indexed using their file name.
pub fn read_track(path: &Path, cover_dir: &Path) -> Track {
    let mut t = Track { path: path.to_path_buf(), ..Default::default() };
    let mut picture: Option<Vec<u8>> = None;

    if let Ok(tagged) = lofty::read_from_path(path) {
        t.duration_ms = tagged.properties().duration().as_millis() as u64;
        if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
            fn s(v: Option<std::borrow::Cow<'_, str>>) -> String {
                v.map(|x| x.trim().to_string()).unwrap_or_default()
            }
            t.title = s(tag.title());
            t.artist = s(tag.artist());
            t.album = s(tag.album());
            t.genre = s(tag.genre());
            t.album_artist = tag
                .get_string(&ItemKey::AlbumArtist)
                .map(|x| x.trim().to_string())
                .unwrap_or_default();
            t.track_no = tag.track().unwrap_or(0);
            t.disc_no = tag.disk().unwrap_or(0);
            t.year = tag.year().unwrap_or(0);
            picture = tag.pictures().first().map(|p| p.data().to_vec());
        }
    }

    if t.title.is_empty() {
        t.title = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Unknown".into());
    }
    if t.artist.is_empty() {
        t.artist = "Unknown Artist".into();
    }
    if t.album.is_empty() {
        t.album = "Unknown Album".into();
    }

    let key = album_key(t.grouping_artist(), &t.album);
    t.cover = picture
        .and_then(|data| save_cover(&data, cover_dir, &key))
        .or_else(|| folder_cover(path));
    t
}

/// Stores cover image bytes in the cover cache; returns the file path.
pub fn save_cover(data: &[u8], cover_dir: &Path, key: &str) -> Option<PathBuf> {
    let ext = if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if data.starts_with(&[0xFF, 0xD8]) {
        "jpg"
    } else {
        return None;
    };
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    let file = cover_dir.join(format!("{:016x}.{ext}", h.finish()));
    if !file.exists() {
        fs::write(&file, data).ok()?;
    }
    Some(file)
}

/// A cover previously stored by [`save_cover`] under `key`, if any.
pub fn cached_cover(cover_dir: &Path, key: &str) -> Option<PathBuf> {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    let stem = format!("{:016x}", h.finish());
    ["jpg", "png"].iter().map(|ext| cover_dir.join(format!("{stem}.{ext}"))).find(|p| p.is_file())
}

fn folder_cover(track: &Path) -> Option<PathBuf> {
    let dir = track.parent()?;
    FOLDER_COVERS.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_audio_extensions() {
        assert!(is_audio(Path::new("/a/b.FLAC")));
        assert!(is_audio(Path::new("song.mp3")));
        assert!(!is_audio(Path::new("cover.jpg")));
        assert!(!is_audio(Path::new("noext")));
    }

    #[test]
    fn scans_a_folder() {
        let root = std::env::temp_dir().join(format!("unitn-scan-{}", std::process::id()));
        let music = root.join("music/Artist/Album");
        fs::create_dir_all(&music).unwrap();
        fs::write(music.join("01 Intro.wav"), crate::audio::test_wav(8000, 0.1)).unwrap();
        fs::write(music.join("notes.txt"), b"hi").unwrap();

        let lib = Library::open_in_memory().unwrap();
        let folders = vec![root.join("music")];
        let stats = scan(&lib, &folders, &root.join("covers"), |_| {}).unwrap();
        assert_eq!(stats.scanned, 1);
        assert_eq!(stats.updated, 1);
        let tracks = lib.tracks(None).unwrap();
        assert_eq!(tracks[0].title, "01 Intro");

        // Second scan: nothing changed.
        let stats = scan(&lib, &folders, &root.join("covers"), |_| {}).unwrap();
        assert_eq!(stats.updated, 0);

        // File removed: index entry is dropped.
        fs::remove_file(music.join("01 Intro.wav")).unwrap();
        let stats = scan(&lib, &folders, &root.join("covers"), |_| {}).unwrap();
        assert_eq!(stats.removed, 1);
        assert_eq!(lib.track_count().unwrap(), 0);
        let _ = fs::remove_dir_all(&root);
    }
}
