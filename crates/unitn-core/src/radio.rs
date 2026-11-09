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

    fn stations(&self, path: &str, query: &[(&str, &str)]) -> Result<Vec<Station>> {
        let mut last_error = String::from("no mirror available");
        for mirror in MIRRORS {
            let mut req = self.agent.get(&format!("{mirror}{path}"));
            for (k, v) in query {
                req = req.query(k, v);
            }
            match req.call() {
                Ok(resp) => {
                    let list: Vec<StationDto> = resp.into_json()?;
                    return Ok(list
                        .into_iter()
                        .map(Station::from)
                        .filter(|s| !s.url.is_empty() && !s.name.is_empty())
                        .collect());
                }
                Err(e) => last_error = e.to_string(),
            }
        }
        Err(anyhow!("Radio Browser is unreachable ({last_error})"))
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
}
