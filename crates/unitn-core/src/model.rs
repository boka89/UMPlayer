use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A local audio file with its tags.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    pub id: i64,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub track_no: u32,
    pub disc_no: u32,
    pub year: u32,
    pub genre: String,
    pub duration_ms: u64,
    pub cover: Option<PathBuf>,
    /// For songs on a music server: where to stream them from. `path` then
    /// holds a stable key like `jellyfin://server/item`.
    pub stream_url: Option<String>,
    /// File type of the stream (e.g. "flac"), if known.
    pub stream_ext: Option<String>,
}

impl Track {
    /// What to give the player to play this song.
    pub fn source(&self) -> crate::audio::Source {
        match &self.stream_url {
            Some(url) => crate::audio::Source::Remote { url: url.clone(), ext: self.stream_ext.clone() },
            None => crate::audio::Source::File(self.path.clone()),
        }
    }

    /// True for songs streamed from a music server.
    pub fn is_remote(&self) -> bool {
        self.stream_url.is_some()
    }

    /// The artist an album is grouped under (album artist if present).
    pub fn grouping_artist(&self) -> &str {
        if self.album_artist.is_empty() {
            &self.artist
        } else {
            &self.album_artist
        }
    }

    pub fn duration_secs(&self) -> f64 {
        self.duration_ms as f64 / 1000.0
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Album {
    pub title: String,
    pub artist: String,
    pub year: u32,
    pub track_count: u32,
    pub duration_ms: u64,
    pub cover: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Artist {
    pub name: String,
    pub album_count: u32,
    pub track_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub track_count: u32,
}

/// An internet radio station (from radio-browser.info or a favorite).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Station {
    pub uuid: String,
    pub name: String,
    pub url: String,
    pub favicon: String,
    pub country: String,
    pub tags: String,
    pub codec: String,
    pub bitrate: u32,
}

impl Station {
    /// True if one of the station's tags contains `tag` (ignoring case), the
    /// way Radio Browser matches a genre: "pop" also finds "pop rock".
    pub fn has_tag(&self, tag: &str) -> bool {
        let tag = tag.to_lowercase();
        self.tags.split(',').any(|t| t.trim().to_lowercase().contains(&tag))
    }

    /// Short human readable description, e.g. "Germany • jazz, swing • MP3 128 kbps".
    pub fn info(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.country.is_empty() {
            parts.push(self.country.clone());
        }
        let tags: Vec<&str> = self
            .tags
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .take(3)
            .collect();
        if !tags.is_empty() {
            parts.push(tags.join(", "));
        }
        let mut fmt = self.codec.to_uppercase();
        if self.bitrate > 0 {
            if !fmt.is_empty() {
                fmt.push(' ');
            }
            fmt.push_str(&format!("{} kbps", self.bitrate));
        }
        if !fmt.is_empty() {
            parts.push(fmt);
        }
        parts.join(" • ")
    }
}
