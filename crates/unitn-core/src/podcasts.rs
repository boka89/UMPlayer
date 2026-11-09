//! Podcasts: find shows in Apple's public podcast directory (no account or
//! key needed) or by RSS link, read their feeds and list the episodes.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::util::url_encode;

/// A show, from a search or a feed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Show {
    pub feed_url: String,
    pub title: String,
    pub author: String,
    pub description: String,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Episode {
    pub guid: String,
    pub title: String,
    /// Unix time (seconds), 0 if unknown.
    pub published: i64,
    /// Length in seconds, 0 if unknown.
    pub duration: u64,
    pub audio_url: String,
    pub mime: Option<String>,
    pub description: String,
}

impl Episode {
    /// File extension hint for the decoder.
    pub fn extension(&self) -> Option<String> {
        let from_mime = match self.mime.as_deref().unwrap_or("") {
            "audio/mpeg" | "audio/mp3" => Some("mp3"),
            "audio/mp4" | "audio/x-m4a" | "audio/m4a" | "audio/aac" => Some("m4a"),
            "audio/ogg" | "audio/vorbis" => Some("ogg"),
            "audio/flac" => Some("flac"),
            "audio/wav" | "audio/x-wav" => Some("wav"),
            _ => None,
        };
        from_mime.map(str::to_string).or_else(|| {
            let path = self.audio_url.split(['?', '#']).next().unwrap_or("");
            let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
            (ext.len() <= 4 && ext.chars().all(|c| c.is_ascii_alphanumeric())).then_some(ext)
        })
    }
}

/// A feed: the show and its episodes, newest first.
#[derive(Debug, Clone, Default)]
pub struct Feed {
    pub show: Show,
    pub episodes: Vec<Episode>,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(crate::USER_AGENT)
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .build()
}

/// Searches Apple's podcast directory.
pub fn search(term: &str) -> Result<Vec<Show>> {
    let url = format!(
        "https://itunes.apple.com/search?media=podcast&entity=podcast&limit=30&term={}",
        url_encode(term.trim())
    );
    let json: Value = agent().get(&url).call().context("podcast search")?.into_json()?;
    Ok(parse_search(&json))
}

pub(crate) fn parse_search(json: &Value) -> Vec<Show> {
    json["results"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|r| {
                    let feed_url = r["feedUrl"].as_str().filter(|u| u.starts_with("http"))?.to_string();
                    Some(Show {
                        feed_url,
                        title: r["collectionName"].as_str().unwrap_or("Untitled podcast").to_string(),
                        author: r["artistName"].as_str().unwrap_or_default().to_string(),
                        description: r["primaryGenreName"].as_str().unwrap_or_default().to_string(),
                        image_url: r["artworkUrl600"].as_str().or(r["artworkUrl100"].as_str()).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Downloads and reads a podcast feed.
pub fn fetch(feed_url: &str) -> Result<Feed> {
    let url = feed_url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("a podcast link starts with http:// or https://");
    }
    let body = agent().get(url).call().context("could not load the podcast feed")?.into_string()?;
    parse_feed(url, &body)
}

const ITUNES: &str = "http://www.itunes.com/dtds/podcast-1.0.dtd";

/// Most episodes kept per show.
const MAX_EPISODES: usize = 500;

pub fn parse_feed(feed_url: &str, xml: &str) -> Result<Feed> {
    let xml = xml.trim_start_matches('\u{feff}').trim_start();
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(xml, options)
        .map_err(|e| anyhow!("this is not a podcast feed ({e})"))?;
    let channel = doc
        .descendants()
        .find(|n| n.has_tag_name("channel"))
        .ok_or_else(|| anyhow!("this is not a podcast feed (no channel)"))?;
    let show = Show {
        feed_url: feed_url.to_string(),
        title: plain_child(channel, "title").unwrap_or_else(|| "Untitled podcast".into()),
        author: itunes_child(channel, "author").or_else(|| plain_child(channel, "managingEditor")).unwrap_or_default(),
        description: strip_html(
            &plain_child(channel, "description").or_else(|| itunes_child(channel, "summary")).unwrap_or_default(),
        ),
        image_url: channel
            .children()
            .find(|n| n.has_tag_name((ITUNES, "image")))
            .and_then(|n| n.attribute("href"))
            .map(str::to_string)
            .or_else(|| {
                channel
                    .children()
                    .find(|n| n.tag_name().name() == "image" && n.tag_name().namespace().is_none())
                    .and_then(|img| plain_child(img, "url"))
            }),
    };
    let mut episodes: Vec<Episode> = channel
        .children()
        .filter(|n| n.has_tag_name("item"))
        .filter_map(|item| {
            let enclosure = item.children().find(|n| n.has_tag_name("enclosure"))?;
            let audio_url = enclosure.attribute("url")?.trim().to_string();
            let mime = enclosure.attribute("type").map(|t| t.trim().to_ascii_lowercase());
            if mime.as_deref().is_some_and(|m| m.starts_with("video/")) {
                return None;
            }
            let title = plain_child(item, "title").or_else(|| itunes_child(item, "title")).unwrap_or_default();
            Some(Episode {
                guid: plain_child(item, "guid").unwrap_or_else(|| audio_url.clone()),
                published: plain_child(item, "pubDate").and_then(|d| parse_rfc2822(&d)).unwrap_or(0),
                duration: itunes_child(item, "duration").and_then(|d| parse_duration(&d)).unwrap_or(0),
                description: strip_html(
                    &plain_child(item, "description").or_else(|| itunes_child(item, "summary")).unwrap_or_default(),
                ),
                title: if title.is_empty() { "Untitled episode".into() } else { title },
                audio_url,
                mime,
            })
        })
        .collect();
    episodes.sort_by(|a, b| b.published.cmp(&a.published));
    episodes.truncate(MAX_EPISODES);
    Ok(Feed { show, episodes })
}

fn text_of(n: roxmltree::Node) -> String {
    n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect::<String>().trim().to_string()
}

/// Text of a child element without a namespace (RSS's own tags).
fn plain_child(n: roxmltree::Node, name: &str) -> Option<String> {
    n.children()
        .find(|c| c.is_element() && c.tag_name().name() == name && c.tag_name().namespace().is_none())
        .map(text_of)
        .filter(|t| !t.is_empty())
}

fn itunes_child(n: roxmltree::Node, name: &str) -> Option<String> {
    n.children().find(|c| c.has_tag_name((ITUNES, name))).map(text_of).filter(|t| !t.is_empty())
}

/// Removes HTML tags and collapses whitespace (show notes are often HTML).
pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `1:02:03`, `62:03` or `3723` → seconds.
pub fn parse_duration(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut total = 0f64;
    for p in &parts {
        total = total * 60.0 + p.trim().parse::<f64>().ok()?;
    }
    (total >= 0.0).then_some(total as u64)
}

/// RFC 2822 dates as used by RSS: `Wed, 05 Nov 2025 10:00:00 +0000`.
pub fn parse_rfc2822(s: &str) -> Option<i64> {
    let s = s.trim();
    let s = s.split_once(',').map(|(_, r)| r).unwrap_or(s);
    let mut it = s.split_whitespace();
    let day: i64 = it.next()?.parse().ok()?;
    let month = match it.next()?.get(..3)?.to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let mut year: i64 = it.next()?.parse().ok()?;
    if year < 100 {
        year += if year < 50 { 2000 } else { 1900 };
    }
    let mut hms = it.next().unwrap_or("00:00:00").split(':').map(|p| p.parse::<i64>().unwrap_or(0));
    let (h, m, sec) = (hms.next().unwrap_or(0), hms.next().unwrap_or(0), hms.next().unwrap_or(0));
    let zone = it.next().unwrap_or("+0000");
    let offset_min = match zone.to_ascii_uppercase().as_str() {
        "GMT" | "UT" | "UTC" | "Z" => 0,
        "EST" => -300,
        "EDT" => -240,
        "CST" => -360,
        "CDT" => -300,
        "MST" => -420,
        "MDT" => -360,
        "PST" => -480,
        "PDT" => -420,
        z => {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let digits: String = z.chars().filter(|c| c.is_ascii_digit()).collect();
            let v: i64 = digits.parse().unwrap_or(0);
            sign * (v / 100 * 60 + v % 100)
        }
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86400 + h * 3600 + m * 60 + sec - offset_min * 60)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `2025-11-05` from a Unix time (for the episode list).
pub fn format_date(unix: i64) -> String {
    if unix <= 0 {
        return String::new();
    }
    let days = unix.div_euclid(86400);
    // Inverse of days_from_civil.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd" xmlns:content="http://purl.org/rss/1.0/modules/content/">
<channel>
  <title>Danube Talks</title>
  <itunes:author>Duna Media</itunes:author>
  <description><![CDATA[<p>Stories &amp; music from <b>Budapest</b>.</p>]]></description>
  <itunes:image href="https://example.com/show.jpg"/>
  <item>
    <title>Episode 1: Buda</title>
    <itunes:title>Buda</itunes:title>
    <guid isPermaLink="false">ep-1</guid>
    <pubDate>Mon, 03 Nov 2025 10:00:00 +0000</pubDate>
    <itunes:duration>1:02:03</itunes:duration>
    <enclosure url="https://cdn.example.com/ep1.mp3?x=1" type="audio/mpeg" length="123"/>
  </item>
  <item>
    <title>Episode 2: Pest</title>
    <pubDate>Wed, 05 Nov 2025 08:30:00 GMT</pubDate>
    <itunes:duration>754</itunes:duration>
    <enclosure url="https://cdn.example.com/ep2.m4a" length="1"/>
  </item>
  <item><title>Video</title><enclosure url="https://cdn.example.com/v.mp4" type="video/mp4"/></item>
  <item><title>No audio</title></item>
</channel>
</rss>"#;

    #[test]
    fn reads_a_feed() {
        let feed = parse_feed("https://example.com/feed.xml", FEED).unwrap();
        assert_eq!(feed.show.title, "Danube Talks");
        assert_eq!(feed.show.author, "Duna Media");
        assert_eq!(feed.show.description, "Stories & music from Budapest .");
        assert_eq!(feed.show.image_url.as_deref(), Some("https://example.com/show.jpg"));
        assert_eq!(feed.episodes.len(), 2);
        // Newest first.
        assert_eq!(feed.episodes[0].title, "Episode 2: Pest");
        assert_eq!(feed.episodes[0].guid, "https://cdn.example.com/ep2.m4a");
        assert_eq!(feed.episodes[0].duration, 754);
        assert_eq!(feed.episodes[0].extension().as_deref(), Some("m4a"));
        assert_eq!(feed.episodes[1].title, "Episode 1: Buda");
        assert_eq!(feed.episodes[1].guid, "ep-1");
        assert_eq!(feed.episodes[1].duration, 3723);
        assert_eq!(feed.episodes[1].extension().as_deref(), Some("mp3"));
        assert_eq!(format_date(feed.episodes[1].published), "2025-11-03");
        assert!(parse_feed("x", "<html><body>nope</body></html>").is_err());
    }

    #[test]
    fn parses_dates_and_durations() {
        assert_eq!(parse_rfc2822("Thu, 01 Jan 1970 00:00:00 +0000"), Some(0));
        assert_eq!(parse_rfc2822("Mon, 03 Nov 2025 10:00:00 +0000"), Some(1762164000));
        assert_eq!(parse_rfc2822("Mon, 03 Nov 2025 05:00:00 -0500"), Some(1762164000));
        assert_eq!(parse_rfc2822("03 Nov 2025 10:00:00 GMT"), Some(1762164000));
        assert_eq!(parse_rfc2822("garbage"), None);
        assert_eq!(format_date(1762164000), "2025-11-03");
        assert_eq!(format_date(951782400), "2000-02-29");
        assert_eq!(parse_duration("45:10"), Some(2710));
        assert_eq!(parse_duration("90"), Some(90));
        assert_eq!(parse_duration("x"), None);
    }

    #[test]
    fn reads_search_results() {
        let json: Value = serde_json::from_str(
            r#"{"results":[{"collectionName":"A","artistName":"B","feedUrl":"https://f/1","artworkUrl600":"https://i/1"},
                           {"collectionName":"No feed"}]}"#,
        )
        .unwrap();
        let shows = parse_search(&json);
        assert_eq!(shows.len(), 1);
        assert_eq!(shows[0].feed_url, "https://f/1");
        assert_eq!(shows[0].image_url.as_deref(), Some("https://i/1"));
    }
}
