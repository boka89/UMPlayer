//! Album art for albums without any local cover, from the free and open
//! MusicBrainz database and the Cover Art Archive (no API key needed).
//!
//! MusicBrainz asks clients to send a descriptive User-Agent and to make at
//! most one request per second; [`ArtFetcher`] does both.

use std::io::Read;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Deserialize;

/// Covers larger than this are ignored.
const MAX_IMAGE: u64 = 12 * 1024 * 1024;
/// Minimum MusicBrainz search score to accept a match.
const MIN_SCORE: u32 = 90;

#[derive(Deserialize)]
struct Search {
    #[serde(rename = "release-groups", default)]
    release_groups: Vec<ReleaseGroup>,
}

#[derive(Deserialize)]
struct ReleaseGroup {
    id: String,
    #[serde(default)]
    score: u32,
}

pub struct ArtFetcher {
    agent: ureq::Agent,
    last_request: Option<Instant>,
}

impl Default for ArtFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtFetcher {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .user_agent(crate::USER_AGENT)
            .timeout(Duration::from_secs(20))
            .build();
        Self { agent, last_request: None }
    }

    fn throttle(&mut self) {
        if let Some(t) = self.last_request {
            let wait = Duration::from_millis(1100).saturating_sub(t.elapsed());
            std::thread::sleep(wait);
        }
        self.last_request = Some(Instant::now());
    }

    /// Finds the front cover of `album` by `artist`. `Ok(None)` means no match
    /// or no artwork available; errors are network problems.
    pub fn find_cover(&mut self, artist: &str, album: &str) -> Result<Option<Vec<u8>>> {
        let query = format!("releasegroup:{} AND artist:{}", lucene_quote(album), lucene_quote(artist));
        self.throttle();
        let search: Search = self
            .agent
            .get("https://musicbrainz.org/ws/2/release-group/")
            .query("query", &query)
            .query("fmt", "json")
            .query("limit", "5")
            .call()?
            .into_json()?;

        for group in search.release_groups.iter().filter(|g| g.score >= MIN_SCORE).take(3) {
            let url = format!("https://coverartarchive.org/release-group/{}/front-500", group.id);
            match self.agent.get(&url).call() {
                Ok(resp) => {
                    let mut data = Vec::new();
                    resp.into_reader().take(MAX_IMAGE).read_to_end(&mut data)?;
                    if is_image(&data) {
                        return Ok(Some(data));
                    }
                }
                Err(ureq::Error::Status(404, _)) => continue, // no art for this one
                Err(e) => return Err(e.into()),
            }
        }
        Ok(None)
    }
}

/// One album found by a manual search ("Fix album info").
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumCandidate {
    /// MusicBrainz release-group id.
    pub id: String,
    pub title: String,
    pub artist: String,
    pub year: u32,
    /// "Album", "Single", "EP", "Compilation", …
    pub kind: String,
}

fn parse_candidates(json: &serde_json::Value) -> Vec<AlbumCandidate> {
    let Some(groups) = json["release-groups"].as_array() else { return Vec::new() };
    groups
        .iter()
        .filter_map(|g| {
            let id = g["id"].as_str()?.to_string();
            let title = g["title"].as_str().unwrap_or_default().to_string();
            let artist = g["artist-credit"]
                .as_array()
                .map(|credits| {
                    credits
                        .iter()
                        .map(|c| {
                            format!("{}{}", c["name"].as_str().unwrap_or_default(), c["joinphrase"].as_str().unwrap_or_default())
                        })
                        .collect::<String>()
                })
                .unwrap_or_default();
            let year = g["first-release-date"].as_str().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok()).unwrap_or(0);
            let mut kind = g["primary-type"].as_str().unwrap_or_default().to_string();
            if let Some(secondary) = g["secondary-types"].as_array().and_then(|t| t.first()).and_then(|t| t.as_str()) {
                kind = if kind.is_empty() { secondary.to_string() } else { format!("{kind} · {secondary}") };
            }
            Some(AlbumCandidate { id, title, artist, year, kind })
        })
        .collect()
}

impl ArtFetcher {
    /// Searches MusicBrainz for albums matching the (possibly misspelled)
    /// artist and album names, best matches first. Either may be empty.
    pub fn search_albums(&mut self, artist: &str, album: &str) -> Result<Vec<AlbumCandidate>> {
        let (artist, album) = (artist.trim(), album.trim());
        // Unquoted terms tolerate typos better than exact phrases.
        let terms = |field: &str, v: &str| {
            let words: Vec<String> = v.split_whitespace().map(|w| lucene_escape(w)).filter(|w| !w.is_empty()).collect();
            if words.is_empty() { String::new() } else { format!("{field}:({})", words.join(" ")) }
        };
        let query = [terms("releasegroup", album), terms("artist", artist)]
            .into_iter()
            .filter(|q| !q.is_empty())
            .collect::<Vec<_>>()
            .join(" AND ");
        if query.is_empty() {
            return Ok(Vec::new());
        }
        self.throttle();
        let json: serde_json::Value = self
            .agent
            .get("https://musicbrainz.org/ws/2/release-group/")
            .query("query", &query)
            .query("fmt", "json")
            .query("limit", "15")
            .call()?
            .into_json()?;
        Ok(parse_candidates(&json))
    }

    /// Front cover of a release group (`size`: 250, 500 or 1200 pixels).
    /// `Ok(None)` if the Cover Art Archive has none.
    pub fn cover_for(&self, release_group: &str, size: u32) -> Result<Option<Vec<u8>>> {
        let url = format!("https://coverartarchive.org/release-group/{release_group}/front-{size}");
        match self.agent.get(&url).call() {
            Ok(resp) => {
                let mut data = Vec::new();
                resp.into_reader().take(MAX_IMAGE).read_to_end(&mut data)?;
                Ok(is_image(&data).then_some(data))
            }
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// Escapes Lucene special characters in a single search term.
fn lucene_escape(word: &str) -> String {
    let mut out = String::new();
    for c in word.chars() {
        if "+-&|!(){}[]^\"~*?:\\/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl ArtFetcher {
    /// Finds a photo of an artist: MusicBrainz → Wikidata → Wikimedia Commons
    /// (all free and open). `Ok(None)` means no photo is available.
    pub fn find_artist_image(&mut self, artist: &str) -> Result<Option<Vec<u8>>> {
        #[derive(Deserialize)]
        struct ArtistSearch {
            #[serde(default)]
            artists: Vec<ReleaseGroup>, // same shape: id + score
        }
        self.throttle();
        let found: ArtistSearch = self
            .agent
            .get("https://musicbrainz.org/ws/2/artist/")
            .query("query", &format!("artist:{}", lucene_quote(artist)))
            .query("fmt", "json")
            .query("limit", "3")
            .call()?
            .into_json()?;
        let Some(mbid) = found.artists.iter().find(|a| a.score >= MIN_SCORE).map(|a| a.id.clone()) else {
            return Ok(None);
        };

        self.throttle();
        let details: serde_json::Value = self
            .agent
            .get(&format!("https://musicbrainz.org/ws/2/artist/{mbid}"))
            .query("inc", "url-rels")
            .query("fmt", "json")
            .call()?
            .into_json()?;
        let Some(qid) = wikidata_id(&details) else { return Ok(None) };

        let entity: serde_json::Value = self
            .agent
            .get(&format!("https://www.wikidata.org/wiki/Special:EntityData/{qid}.json"))
            .call()?
            .into_json()?;
        let Some(file) = entity["entities"][&qid]["claims"]["P18"][0]["mainsnak"]["datavalue"]["value"].as_str() else {
            return Ok(None);
        };

        let url = format!(
            "https://commons.wikimedia.org/wiki/Special:FilePath/{}?width=1600",
            percent_encode(&file.replace(' ', "_"))
        );
        let mut data = Vec::new();
        self.agent.get(&url).call()?.into_reader().take(MAX_IMAGE).read_to_end(&mut data)?;
        Ok(is_image(&data).then_some(data))
    }
}

/// The Wikidata item (e.g. "Q42") linked from a MusicBrainz artist.
fn wikidata_id(artist: &serde_json::Value) -> Option<String> {
    artist["relations"].as_array()?.iter().find_map(|rel| {
        if rel["type"].as_str()? != "wikidata" {
            return None;
        }
        let url = rel["url"]["resource"].as_str()?;
        url.rsplit('/').next().filter(|q| q.starts_with('Q')).map(str::to_string)
    })
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn is_image(data: &[u8]) -> bool {
    data.starts_with(&[0xFF, 0xD8]) || data.starts_with(&[0x89, b'P', b'N', b'G'])
}

/// Quotes a value for a MusicBrainz (Lucene) search query.
fn lucene_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_queries() {
        assert_eq!(lucene_quote(r#"Say "Hi""#), r#""Say \"Hi\"""#);
        assert_eq!(lucene_quote("AC/DC"), "\"AC/DC\"");
    }

    #[test]
    fn parses_search_results() {
        let json = r#"{"created":"x","count":2,"offset":0,"release-groups":[{"id":"abc","score":100,"title":"X"},{"id":"def","score":40}]}"#;
        let s: Search = serde_json::from_str(json).unwrap();
        assert_eq!(s.release_groups.len(), 2);
        assert_eq!(s.release_groups[0].id, "abc");
        assert_eq!(s.release_groups[1].score, 40);
    }

    #[test]
    fn finds_wikidata_links() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"relations":[{"type":"official homepage","url":{"resource":"https://x"}},
                             {"type":"wikidata","url":{"resource":"https://www.wikidata.org/wiki/Q1299"}}]}"#,
        )
        .unwrap();
        assert_eq!(wikidata_id(&v).as_deref(), Some("Q1299"));
        assert_eq!(percent_encode("Bomfunk MC's_2001.jpg"), "Bomfunk%20MC%27s_2001.jpg");
    }

    #[test]
    fn parses_album_candidates() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"release-groups":[{"id":"abc","title":"Ghosts I–IV","primary-type":"Album",
               "first-release-date":"2008-03-02",
               "artist-credit":[{"name":"Nine Inch Nails","joinphrase":""}]},
              {"id":"def","title":"Duets","primary-type":"Album","secondary-types":["Live"],
               "artist-credit":[{"name":"A","joinphrase":" & "},{"name":"B"}]}]}"#,
        )
        .unwrap();
        let c = parse_candidates(&json);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].year, 2008);
        assert_eq!(c[0].artist, "Nine Inch Nails");
        assert_eq!(c[1].artist, "A & B");
        assert_eq!(c[1].kind, "Album · Live");
        assert_eq!(c[1].year, 0);
        assert_eq!(lucene_escape("AC/DC"), "AC\\/DC");
    }

    #[test]
    fn recognises_images() {
        assert!(is_image(&[0xFF, 0xD8, 0xFF]));
        assert!(!is_image(b"<html>"));
    }
}
