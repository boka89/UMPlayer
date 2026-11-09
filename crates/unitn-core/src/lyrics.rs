//! Song lyrics: from a `.lrc` file next to the song, from the song's own
//! tags, or online from LRCLIB (lrclib.net, free and open). Synced (LRC)
//! lyrics carry a time per line, so the current line can be highlighted.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use crate::util::url_encode;

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Start time in seconds (synced lyrics only).
    pub time: Option<f64>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    /// Every line has a time.
    pub synced: bool,
    /// The song has no words (LRCLIB marks these).
    pub instrumental: bool,
    /// Where they came from, for the UI ("LRCLIB", "Lyrics file", "Song tags").
    pub source: String,
}

impl Lyrics {
    pub fn instrumental(source: &str) -> Self {
        Self { lines: Vec::new(), synced: false, instrumental: true, source: source.into() }
    }

    /// Index of the line being sung at `position` seconds.
    pub fn current_line(&self, position: f64) -> Option<usize> {
        if !self.synced {
            return None;
        }
        let n = self.lines.partition_point(|l| l.time.unwrap_or(0.0) <= position);
        n.checked_sub(1)
    }

    /// The lyrics as text again (LRC for synced lyrics), for the cache.
    pub fn to_text(&self) -> String {
        self.lines
            .iter()
            .map(|l| match l.time {
                Some(t) if self.synced => {
                    let cs = (t * 100.0).round().max(0.0) as u64;
                    format!("[{:02}:{:02}.{:02}]{}", cs / 6000, cs / 100 % 60, cs % 100, l.text)
                }
                _ => l.text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Parses LRC (`[mm:ss.xx]text`, several stamps per line, `[offset:±ms]`)
/// or plain text. Returns `None` for empty lyrics.
pub fn parse(text: &str, source: &str) -> Option<Lyrics> {
    let mut timed: Vec<Line> = Vec::new();
    let mut plain: Vec<Line> = Vec::new();
    let mut offset = 0.0;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let mut rest = line.trim_start();
        let mut stamps = Vec::new();
        while let Some(body) = rest.strip_prefix('[') {
            let Some(end) = body.find(']') else { break };
            let tag = &body[..end];
            if let Some(t) = parse_stamp(tag) {
                stamps.push(t);
            } else if let Some(v) = tag.strip_prefix("offset:") {
                // Positive offset: lyrics appear earlier.
                offset = v.trim().parse::<f64>().unwrap_or(0.0) / 1000.0;
            } else if stamps.is_empty()
                && tag
                    .split_once(':')
                    .is_some_and(|(k, _)| !k.is_empty() && k.len() <= 8 && k.chars().all(|c| c.is_ascii_alphabetic()))
            {
                // ID tags like [ar:…], [ti:…]: not lyrics.
                rest = "";
                break;
            } else {
                break;
            }
            rest = &body[end + 1..];
        }
        let words = rest.trim().to_string();
        if stamps.is_empty() {
            if !(words.is_empty() && plain.is_empty()) {
                plain.push(Line { time: None, text: words });
            }
        } else {
            for t in stamps {
                timed.push(Line { time: Some(t), text: words.clone() });
            }
        }
    }
    if !timed.is_empty() {
        for l in &mut timed {
            l.time = l.time.map(|t| (t - offset).max(0.0));
        }
        timed.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
        return Some(Lyrics { lines: timed, synced: true, instrumental: false, source: source.into() });
    }
    while plain.last().is_some_and(|l| l.text.is_empty()) {
        plain.pop();
    }
    (!plain.is_empty()).then(|| Lyrics { lines: plain, synced: false, instrumental: false, source: source.into() })
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss:xx` → seconds.
fn parse_stamp(tag: &str) -> Option<f64> {
    let (min, rest) = tag.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    let rest = rest.replacen(':', ".", 1);
    let secs: f64 = rest.trim().parse().ok()?;
    (secs < 60.0 && secs >= 0.0).then_some(min as f64 * 60.0 + secs)
}

/// Lyrics stored with a local song: `Song.lrc` next to `Song.mp3`, or a
/// lyrics tag inside the file.
pub fn local(path: &Path) -> Option<Lyrics> {
    let lrc = path.with_extension("lrc");
    if let Ok(text) = std::fs::read_to_string(&lrc) {
        if let Some(l) = parse(&text, "Lyrics file") {
            return Some(l);
        }
    }
    use lofty::prelude::*;
    let tagged = lofty::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let text = tag.get_string(&lofty::tag::ItemKey::Lyrics)?;
    parse(text, "Song tags")
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(crate::USER_AGENT)
        .timeout_connect(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .build()
}

/// Looks the song up on LRCLIB. `Ok(None)`: LRCLIB doesn't know it.
pub fn lrclib(artist: &str, title: &str, album: &str, duration_secs: f64) -> Result<Option<Lyrics>> {
    if artist.trim().is_empty() || title.trim().is_empty() {
        return Ok(None);
    }
    let mut url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}",
        url_encode(artist.trim()),
        url_encode(title.trim())
    );
    if !album.trim().is_empty() {
        url.push_str(&format!("&album_name={}", url_encode(album.trim())));
    }
    if duration_secs > 0.0 {
        url.push_str(&format!("&duration={}", duration_secs.round() as u64));
    }
    match agent().get(&url).call() {
        Ok(resp) => {
            let json: Value = resp.into_json()?;
            return Ok(from_lrclib(&json));
        }
        Err(ureq::Error::Status(404, _)) => {}
        Err(e) => return Err(e.into()),
    }
    // Not found exactly (album or length differ): search and take the best match.
    let url = format!(
        "https://lrclib.net/api/search?artist_name={}&track_name={}",
        url_encode(artist.trim()),
        url_encode(title.trim())
    );
    let json: Value = agent().get(&url).call()?.into_json()?;
    Ok(best_match(&json, duration_secs).and_then(from_lrclib))
}

/// From search results: closest length (within 10 s when known), synced first.
pub(crate) fn best_match(results: &Value, duration_secs: f64) -> Option<&Value> {
    let list = results.as_array()?;
    list.iter()
        .filter(|r| {
            duration_secs <= 0.0
                || r["duration"].as_f64().map(|d| (d - duration_secs).abs() <= 10.0).unwrap_or(true)
        })
        .min_by_key(|r| {
            let synced = r["syncedLyrics"].as_str().is_some_and(|s| !s.is_empty());
            let diff = r["duration"].as_f64().map(|d| (d - duration_secs).abs()).unwrap_or(30.0);
            (!synced, (diff * 10.0) as i64)
        })
}

pub(crate) fn from_lrclib(json: &Value) -> Option<Lyrics> {
    if let Some(synced) = json["syncedLyrics"].as_str().and_then(|t| parse(t, "LRCLIB")) {
        return Some(synced);
    }
    if let Some(plain) = json["plainLyrics"].as_str().and_then(|t| parse(t, "LRCLIB")) {
        return Some(plain);
    }
    json["instrumental"].as_bool().filter(|i| *i).map(|_| Lyrics::instrumental("LRCLIB"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synced_lrc() {
        let text = "[ar:Band]\n[ti:Song]\n[offset:+500]\n[00:01.00]First line\n[00:05.50][01:05.50]Chorus\n[00:03.20]Second\n\n";
        let l = parse(text, "test").unwrap();
        assert!(l.synced);
        let times: Vec<f64> = l.lines.iter().map(|x| x.time.unwrap()).collect();
        assert_eq!(times, vec![0.5, 2.7, 5.0, 65.0]);
        assert_eq!(l.lines[1].text, "Second");
        assert_eq!(l.lines[3].text, "Chorus");
        assert_eq!(l.current_line(0.0), None);
        assert_eq!(l.current_line(3.0), Some(1));
        assert_eq!(l.current_line(100.0), Some(3));
        // Round trip through the cache format.
        assert_eq!(parse(&l.to_text(), "cache").unwrap().lines, l.lines);
    }

    #[test]
    fn parses_plain_lyrics() {
        let l = parse("\nHello\n\nWorld\n\n", "test").unwrap();
        assert!(!l.synced);
        assert_eq!(l.lines.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), vec!["Hello", "", "World"]);
        assert_eq!(l.current_line(10.0), None);
        assert!(parse("  \n\n", "test").is_none());
        assert_eq!(parse_stamp("02:03:50"), Some(123.5));
        assert_eq!(parse_stamp("ar:x"), None);
    }

    #[test]
    fn picks_lrclib_results() {
        let json: Value = serde_json::from_str(
            r#"[{"duration":200,"plainLyrics":"plain far"},
                {"duration":181,"plainLyrics":"plain close"},
                {"duration":185,"syncedLyrics":"[00:01.00]synced"},
                {"duration":400,"syncedLyrics":"[00:01.00]wrong song"}]"#,
        )
        .unwrap();
        let best = from_lrclib(best_match(&json, 180.0).unwrap()).unwrap();
        assert!(best.synced);
        assert_eq!(best.lines[0].text, "synced");
        let inst: Value = serde_json::from_str(r#"{"instrumental":true,"plainLyrics":null,"syncedLyrics":null}"#).unwrap();
        assert!(from_lrclib(&inst).unwrap().instrumental);
    }

    #[test]
    fn reads_lrc_next_to_the_song() {
        let dir = std::env::temp_dir().join(format!("unitn-lyrics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("Song.mp3");
        std::fs::write(&song, b"not really audio").unwrap();
        std::fs::write(dir.join("Song.lrc"), "[00:10.00]Hi").unwrap();
        let l = local(&song).unwrap();
        assert_eq!(l.source, "Lyrics file");
        assert_eq!(l.lines[0].time, Some(10.0));
        std::fs::remove_dir_all(&dir).ok();
    }
}
