//! Reading and writing M3U playlists (the format every player understands).

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::model::Track;

/// Writes an extended M3U file with absolute paths.
pub fn write(path: &Path, tracks: &[Track]) -> Result<()> {
    let mut out = String::from("#EXTM3U\n");
    for t in tracks {
        out.push_str(&format!(
            "#EXTINF:{},{} - {}\n{}\n",
            (t.duration_ms / 1000).max(1),
            t.artist,
            t.title,
            t.path.display()
        ));
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// Reads an M3U/M3U8 file; relative entries are resolved against its folder.
/// Returns local file paths (URLs are skipped).
pub fn read(path: &Path) -> Result<Vec<PathBuf>> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(parse(&text, &base))
}

pub fn parse(text: &str, base: &Path) -> Vec<PathBuf> {
    text.lines()
        .map(|l| l.trim().trim_start_matches('\u{feff}'))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| !l.contains("://") || l.starts_with("file://"))
        .map(|l| {
            let l = l.strip_prefix("file://").unwrap_or(l);
            let p = PathBuf::from(l.replace('\\', std::path::MAIN_SEPARATOR_STR));
            if p.is_absolute() {
                p
            } else {
                base.join(p)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_resolves_entries() {
        let text = "#EXTM3U\n#EXTINF:120,Band - One\nOne.mp3\n\nhttp://radio.example/live\n/abs/Two.flac\n";
        let paths = parse(text, Path::new("/music/lists"));
        assert_eq!(paths, vec![PathBuf::from("/music/lists/One.mp3"), PathBuf::from("/abs/Two.flac")]);
    }

    #[test]
    fn writes_extended_m3u() {
        let file = std::env::temp_dir().join(format!("unitn-{}.m3u", std::process::id()));
        let song = std::env::temp_dir().join("One.mp3");
        let t = Track { path: song.clone(), title: "One".into(), artist: "Band".into(), duration_ms: 61_000, ..Default::default() };
        write(&file, &[t]).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("#EXTM3U\n#EXTINF:61,Band - One\n"));
        assert_eq!(read(&file).unwrap(), vec![song]);
        let _ = std::fs::remove_file(file);
    }
}
