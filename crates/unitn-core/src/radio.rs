//! Internet radio directory via the free, open Radio Browser API
//! (<https://www.radio-browser.info>, ~50,000 stations, no API key).

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::model::Station;

/// Public mirrors; tried in order until one answers.
const MIRRORS: &[&str] = &[
    "https://de1.api.radio-browser.info",
    "https://nl1.api.radio-browser.info",
    "https://de2.api.radio-browser.info",
    "https://at1.api.radio-browser.info",
];

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StationDto {
    stationuuid: String,
    name: String,
    url: String,
    url_resolved: String,
    favicon: String,
    country: String,
    tags: String,
    codec: String,
    bitrate: u32,
    clickcount: u64,
}

impl From<StationDto> for Station {
    fn from(d: StationDto) -> Self {
        Station {
            uuid: d.stationuuid,
            name: d.name.trim().to_string(),
            url: if d.url_resolved.trim().is_empty() { d.url.trim().to_string() } else { d.url_resolved.trim().to_string() },
            favicon: d.favicon,
            country: d.country,
            tags: d.tags,
            codec: d.codec,
            bitrate: d.bitrate,
        }
    }
}

#[derive(Clone)]
pub struct RadioBrowser {
    agent: ureq::Agent,
}

impl Default for RadioBrowser {
    fn default() -> Self {
        Self::new()
    }
}

impl RadioBrowser {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .user_agent(crate::USER_AGENT)
            .timeout(Duration::from_secs(15))
            .build();
        Self { agent }
    }

    fn dtos(&self, path: &str, query: &[(&str, &str)]) -> Result<Vec<StationDto>> {
        let mut last_error = String::from("no mirror available");
        for mirror in MIRRORS {
            let mut req = self.agent.get(&format!("{mirror}{path}"));
            for (k, v) in query {
                req = req.query(k, v);
            }
            match req.call() {
                Ok(resp) => return Ok(resp.into_json()?),
                Err(e) => last_error = e.to_string(),
            }
        }
        Err(anyhow!("Radio Browser is unreachable ({last_error})"))
    }

    fn stations(&self, path: &str, query: &[(&str, &str)]) -> Result<Vec<Station>> {
        Ok(usable(self.dtos(path, query)?.into_iter().map(Station::from)))
    }

    /// Stations matching several filters at once, most popular first:
    /// any of `tags` (OR; no tags = every genre), and, when given, the
    /// country (ISO 3166 code) and a name search (both AND).
    pub fn filtered(&self, name: &str, tags: &[String], country_code: Option<&str>, limit: u32) -> Result<Vec<Station>> {
        let limit_text = limit.to_string();
        let country = country_code.map(str::to_uppercase);
        let mut base: Vec<(&str, &str)> =
            vec![("limit", &limit_text), ("hidebroken", "true"), ("order", "clickcount"), ("reverse", "true")];
        let name = name.trim();
        if !name.is_empty() {
            base.push(("name", name));
        }
        if let Some(c) = country.as_deref() {
            base.push(("countrycode", c));
        }
        let mut lists = Vec::new();
        if tags.is_empty() {
            lists.push(self.dtos("/json/stations/search", &base)?);
        } else {
            for tag in tags {
                let mut query = base.clone();
                query.push(("tag", tag));
                lists.push(self.dtos("/json/stations/search", &query)?);
            }
        }
        Ok(merge_ranked(lists, limit as usize))
    }

    /// Most popular stations worldwide.
    pub fn top(&self, limit: u32) -> Result<Vec<Station>> {
        self.stations(&format!("/json/stations/topclick/{limit}"), &[("hidebroken", "true")])
    }

    /// Stations whose name matches `name`, most popular first.
    pub fn search(&self, name: &str, limit: u32) -> Result<Vec<Station>> {
        let limit = limit.to_string();
        self.stations(
            "/json/stations/search",
            &[
                ("name", name),
                ("limit", &limit),
                ("hidebroken", "true"),
                ("order", "clickcount"),
                ("reverse", "true"),
            ],
        )
    }

    /// Stations with a tag (genre), e.g. "jazz".
    pub fn by_tag(&self, tag: &str, limit: u32) -> Result<Vec<Station>> {
        let limit = limit.to_string();
        self.stations(
            "/json/stations/search",
            &[("tag", tag), ("limit", &limit), ("hidebroken", "true"), ("order", "clickcount"), ("reverse", "true")],
        )
    }

    /// Most popular stations of a country (ISO 3166 code, e.g. "HU").
    pub fn by_country(&self, country_code: &str, limit: u32) -> Result<Vec<Station>> {
        let limit = limit.to_string();
        self.stations(
            &format!("/json/stations/bycountrycodeexact/{}", country_code.to_uppercase()),
            &[("limit", &limit), ("hidebroken", "true"), ("order", "clickcount"), ("reverse", "true")],
        )
    }

    /// Tells Radio Browser a station was played (keeps its popularity data useful).
    pub fn count_click(&self, uuid: &str) {
        for mirror in MIRRORS {
            if self.agent.get(&format!("{mirror}/json/url/{uuid}")).call().is_ok() {
                return;
            }
        }
    }
}

/// Drops stations that cannot be played or shown.
fn usable(list: impl Iterator<Item = Station>) -> Vec<Station> {
    list.filter(|s| !s.url.is_empty() && !s.name.is_empty()).collect()
}

/// Joins several result lists into one: each station once, most clicked first.
fn merge_ranked(lists: Vec<Vec<StationDto>>, limit: usize) -> Vec<Station> {
    let mut seen = std::collections::HashSet::new();
    let mut all: Vec<StationDto> = lists.into_iter().flatten().filter(|d| seen.insert(d.stationuuid.clone())).collect();
    all.sort_by(|a, b| b.clickcount.cmp(&a.clickcount));
    let mut list = usable(all.into_iter().map(Station::from));
    list.truncate(limit);
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_api_records() {
        let json = r#"[{"stationuuid":"abc","name":" Jazz FM ","url":"http://a/pls","url_resolved":"http://a/stream",
                        "favicon":"","country":"Germany","tags":"jazz,swing","codec":"MP3","bitrate":128,"votes":5}]"#;
        let list: Vec<StationDto> = serde_json::from_str(json).unwrap();
        let s: Station = list.into_iter().next().unwrap().into();
        assert_eq!(s.name, "Jazz FM");
        assert_eq!(s.url, "http://a/stream");
        assert_eq!(s.info(), "Germany • jazz, swing • MP3 128 kbps");
    }

    #[test]
    fn merges_results_of_several_genres() {
        let dto = |id: &str, clicks: u64| StationDto {
            stationuuid: id.into(),
            name: id.into(),
            url: format!("http://{id}"),
            clickcount: clicks,
            ..Default::default()
        };
        let jazz = vec![dto("a", 50), dto("b", 10)];
        let rock = vec![dto("c", 30), dto("a", 50)];
        let merged = merge_ranked(vec![jazz, rock], 10);
        let ids: Vec<&str> = merged.iter().map(|s| s.uuid.as_str()).collect();
        assert_eq!(ids, ["a", "c", "b"]);
        assert_eq!(merge_ranked(vec![vec![dto("a", 1), dto("b", 2)]], 1)[0].uuid, "b");
    }
}
