//! HTTP audio streams for internet radio, including playlist resolution and
//! ICY ("Shoutcast") in-band metadata for the current song title.

use std::io::{self, Read};
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};

use super::PlayerEvent;

pub struct OpenedStream {
    pub reader: Box<dyn Read + Send + Sync>,
    pub content_type: String,
}

/// Connects to `url`, following .m3u/.pls playlists, and returns a reader with
/// ICY metadata stripped out (titles are sent as [`PlayerEvent::StreamTitle`]).
pub fn open(url: &str, events: Sender<PlayerEvent>) -> Result<OpenedStream> {
    let agent = ureq::AgentBuilder::new()
        .user_agent(crate::USER_AGENT)
        .timeout_connect(Duration::from_secs(10))
        // A station that stops sending data for this long counts as dropped.
        .timeout_read(Duration::from_secs(15))
        .build();
    let mut url = url.trim().to_string();
    for _ in 0..4 {
        let resp = agent.get(&url).set("Icy-MetaData", "1").call()?;
        let content_type = resp.content_type().to_ascii_lowercase();
        if is_playlist(&content_type, &url) {
            let body = resp.into_string()?;
            if body.contains("#EXT-X-") {
                bail!("HLS streams are not supported yet");
            }
            url = first_stream_url(&body).ok_or_else(|| anyhow!("the station's playlist is empty"))?;
            continue;
        }
        let metaint = resp
            .header("icy-metaint")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n > 0);
        let inner = resp.into_reader();
        let reader: Box<dyn Read + Send + Sync> = match metaint {
            Some(interval) => Box::new(IcyReader { inner, interval, until_meta: interval, events, last: String::new() }),
            None => inner,
        };
        return Ok(OpenedStream { reader, content_type });
    }
    bail!("too many playlist redirects")
}

fn is_playlist(content_type: &str, url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    content_type.contains("mpegurl")
        || content_type.contains("scpls")
        || path.ends_with(".m3u")
        || path.ends_with(".m3u8")
        || path.ends_with(".pls")
}

/// First http(s) URL in an M3U or PLS playlist.
pub fn first_stream_url(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.split_once('=') {
            // PLS: File1=http://...
            Some((k, v)) if k.to_ascii_lowercase().starts_with("file") => v.trim(),
            _ => l,
        })
        .find(|l| l.starts_with("http://") || l.starts_with("https://"))
        .map(str::to_string)
}

/// File extension hint for symphonia's probe from a Content-Type.
pub fn extension_for(content_type: &str) -> Option<&'static str> {
    let ct = content_type.split(';').next().unwrap_or("").trim();
    Some(match ct {
        "audio/mpeg" | "audio/mp3" | "audio/mpeg3" | "audio/x-mpeg" => "mp3",
        "audio/aac" | "audio/aacp" | "audio/x-aac" | "audio/x-aacp" => "aac",
        "audio/ogg" | "application/ogg" | "audio/vorbis" => "ogg",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/wav" | "audio/x-wav" => "wav",
        _ => return None,
    })
}

/// Extracts `StreamTitle='...'` from an ICY metadata block.
pub fn parse_stream_title(meta: &str) -> Option<String> {
    let start = meta.find("StreamTitle='")? + "StreamTitle='".len();
    let rest = &meta[start..];
    let end = rest.find("';").or_else(|| rest.rfind('\''))?;
    let title = rest[..end].trim();
    (!title.is_empty()).then(|| title.to_string())
}

struct IcyReader {
    inner: Box<dyn Read + Send + Sync>,
    interval: usize,
    until_meta: usize,
    events: Sender<PlayerEvent>,
    last: String,
}

impl IcyReader {
    fn read_metadata(&mut self) -> io::Result<()> {
        let mut len = [0u8; 1];
        self.inner.read_exact(&mut len)?;
        let size = len[0] as usize * 16;
        if size > 0 {
            let mut block = vec![0u8; size];
            self.inner.read_exact(&mut block)?;
            let text = String::from_utf8_lossy(&block);
            if let Some(title) = parse_stream_title(text.trim_end_matches('\0')) {
                if title != self.last {
                    self.last = title.clone();
                    let _ = self.events.send(PlayerEvent::StreamTitle(title));
                }
            }
        }
        self.until_meta = self.interval;
        Ok(())
    }
}

impl Read for IcyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.until_meta == 0 {
            self.read_metadata()?;
        }
        let n = buf.len().min(self.until_meta);
        let read = self.inner.read(&mut buf[..n])?;
        self.until_meta -= read;
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn parses_titles() {
        assert_eq!(
            parse_stream_title("StreamTitle='Daft Punk - One More Time';StreamUrl='';").as_deref(),
            Some("Daft Punk - One More Time")
        );
        assert_eq!(parse_stream_title("StreamTitle='';"), None);
        assert_eq!(parse_stream_title("nothing"), None);
    }

    #[test]
    fn resolves_playlists() {
        let pls = "[playlist]\nNumberOfEntries=1\nFile1=http://radio.example/stream\nTitle1=X\n";
        assert_eq!(first_stream_url(pls).as_deref(), Some("http://radio.example/stream"));
        let m3u = "#EXTM3U\n#EXTINF:-1,Radio\nhttps://radio.example/live.mp3\n";
        assert_eq!(first_stream_url(m3u).as_deref(), Some("https://radio.example/live.mp3"));
        assert!(is_playlist("audio/x-mpegurl", "http://x/y"));
        assert!(is_playlist("text/plain", "http://x/listen.pls?id=1"));
        assert!(!is_playlist("audio/mpeg", "http://x/stream"));
    }

    #[test]
    fn strips_icy_metadata() {
        let meta = b"StreamTitle='Song';";
        let blocks = (meta.len() + 15) / 16;
        let mut data = b"abcd".to_vec();
        data.push(blocks as u8);
        let mut m = meta.to_vec();
        m.resize(blocks * 16, 0);
        data.extend_from_slice(&m);
        data.extend_from_slice(b"efgh");
        data.push(0);

        let (tx, rx) = mpsc::channel();
        let mut r = IcyReader {
            inner: Box::new(io::Cursor::new(data)),
            interval: 4,
            until_meta: 4,
            events: tx,
            last: String::new(),
        };
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abcdefgh");
        assert_eq!(rx.try_recv().unwrap(), PlayerEvent::StreamTitle("Song".into()));
    }

    #[test]
    fn maps_content_types() {
        assert_eq!(extension_for("audio/mpeg"), Some("mp3"));
        assert_eq!(extension_for("audio/aacp; charset=x"), Some("aac"));
        assert_eq!(extension_for("text/html"), None);
    }
}
