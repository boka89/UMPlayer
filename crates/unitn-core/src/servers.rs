//! Music from your own media servers: Jellyfin and Plex.
//!
//! Both are reached over their HTTP APIs with an access token. Songs are
//! streamed as the original files (see [`crate::audio::Source::Remote`]), so
//! they play, seek and transition exactly like local files.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Track;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerKind {
    Jellyfin,
    Plex,
}

impl ServerKind {
    pub fn label(self) -> &'static str {
        match self {
            ServerKind::Jellyfin => "Jellyfin",
            ServerKind::Plex => "Plex",
        }
    }

    fn scheme(self) -> &'static str {
        match self {
            ServerKind::Jellyfin => "jellyfin",
            ServerKind::Plex => "plex",
        }
    }
}

/// A connected server, as saved in the settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub kind: ServerKind,
    /// Name shown in the sidebar (the server's own name).
    pub name: String,
    /// Base address, e.g. `http://192.168.1.10:8096`.
    pub url: String,
    pub user: String,
    /// Access token (never the password).
    pub token: String,
    /// Jellyfin user id.
    #[serde(default)]
    pub user_id: String,
    /// Identifies this app installation to the server.
    pub device_id: String,
}

/// An album on a server.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerAlbum {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub year: u32,
    /// Cover image (already includes the token), if the server has one.
    pub image_url: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(crate::USER_AGENT)
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()
}

/// Cleans up what the user typed: adds `http://`, removes trailing slashes.
pub fn normalize_url(url: &str, kind: ServerKind) -> String {
    let url = url.trim().trim_end_matches('/').to_string();
    if url.contains("://") {
        return url; // typed in full: use as is (e.g. behind a reverse proxy)
    }
    // A bare host name or IP gets http:// and the server's default port.
    let port = if url.contains(':') || url.contains('/') {
        ""
    } else {
        match kind {
            ServerKind::Jellyfin => ":8096",
            ServerKind::Plex => ":32400",
        }
    };
    format!("http://{url}{port}")
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Turns HTTP errors into messages a person can act on.
fn explain(err: ureq::Error, what: &str) -> anyhow::Error {
    match err {
        ureq::Error::Status(401, _) | ureq::Error::Status(403, _) => anyhow!("{what}: wrong user name, password or token"),
        ureq::Error::Status(404, _) => anyhow!("{what}: not found — is this the right address and server type?"),
        ureq::Error::Status(code, _) => anyhow!("{what}: the server answered with error {code}"),
        ureq::Error::Transport(t) => {
            let msg = t.to_string();
            if msg.contains("certificate") || msg.contains("Certificate") {
                anyhow!("{what}: the server's HTTPS certificate isn't trusted (try its http:// address)")
            } else {
                anyhow!("{what}: can't reach the server ({})", t.kind())
            }
        }
    }
}

fn new_device_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut h);
    std::process::id().hash(&mut h);
    format!("unitn-{:016x}", h.finish())
}

/// Signs in and returns the configuration to save. For Plex, `user` may be
/// empty and `secret` an X-Plex-Token; otherwise `secret` is the password.
pub fn connect(kind: ServerKind, url: &str, user: &str, secret: &str) -> Result<ServerConfig> {
    let url = normalize_url(url, kind);
    let device_id = new_device_id();
    match kind {
        ServerKind::Jellyfin => jellyfin::connect(&url, user.trim(), secret, device_id),
        ServerKind::Plex => plex::connect(&url, user.trim(), secret, device_id),
    }
}

/// All albums on the server, sorted by artist and title.
pub fn albums(cfg: &ServerConfig) -> Result<Vec<ServerAlbum>> {
    let mut list = match cfg.kind {
        ServerKind::Jellyfin => jellyfin::albums(cfg)?,
        ServerKind::Plex => plex::albums(cfg)?,
    };
    list.sort_by_cached_key(|a| (a.artist.to_lowercase(), a.title.to_lowercase()));
    Ok(list)
}

/// The songs of an album, in disc and track order, ready to play.
pub fn album_tracks(cfg: &ServerConfig, album: &ServerAlbum) -> Result<Vec<Track>> {
    let mut tracks = match cfg.kind {
        ServerKind::Jellyfin => jellyfin::album_tracks(cfg, album)?,
        ServerKind::Plex => plex::album_tracks(cfg, album)?,
    };
    tracks.sort_by_key(|t| (t.disc_no, t.track_no));
    Ok(tracks)
}

/// Downloads a cover image (at most 12 MB).
pub fn fetch_image(url: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut data = Vec::new();
    agent()
        .get(url)
        .call()
        .map_err(|e| explain(e, "cover"))?
        .into_reader()
        .take(12 * 1024 * 1024)
        .read_to_end(&mut data)?;
    Ok(data)
}

fn host_of(url: &str) -> &str {
    url.split("://").nth(1).unwrap_or(url)
}

fn track_key(cfg: &ServerConfig, id: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}://{}/{}", cfg.kind.scheme(), host_of(&cfg.url), id))
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn u32_of(v: &Value) -> u32 {
    v.as_u64().unwrap_or(0).min(u32::MAX as u64) as u32
}

mod jellyfin {
    use super::*;

    fn auth_header(device_id: &str, token: Option<&str>) -> String {
        let mut h = format!(
            "MediaBrowser Client=\"Unitn\", Device=\"Unitn\", DeviceId=\"{device_id}\", Version=\"{}\"",
            env!("CARGO_PKG_VERSION")
        );
        if let Some(t) = token {
            h.push_str(&format!(", Token=\"{t}\""));
        }
        h
    }

    fn get(cfg: &ServerConfig, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut req = agent()
            .get(&format!("{}{path}", cfg.url))
            .set("Authorization", &auth_header(&cfg.device_id, Some(&cfg.token)))
            .set("Accept", "application/json");
        for (k, v) in query {
            req = req.query(k, v);
        }
        Ok(req.call().map_err(|e| explain(e, "Jellyfin"))?.into_json()?)
    }

    pub fn connect(url: &str, user: &str, password: &str, device_id: String) -> Result<ServerConfig> {
        if user.is_empty() {
            bail!("Enter your Jellyfin user name");
        }
        let info: Value = agent()
            .get(&format!("{url}/System/Info/Public"))
            .call()
            .map_err(|e| explain(e, "Jellyfin"))?
            .into_json()
            .context("this doesn't look like a Jellyfin server")?;
        let name = info["ServerName"].as_str().filter(|n| !n.is_empty()).unwrap_or("Jellyfin").to_string();
        let auth: Value = agent()
            .post(&format!("{url}/Users/AuthenticateByName"))
            .set("Authorization", &auth_header(&device_id, None))
            .set("Accept", "application/json")
            .send_json(serde_json::json!({ "Username": user, "Pw": password }))
            .map_err(|e| explain(e, "Jellyfin sign-in"))?
            .into_json()?;
        let token = str_of(&auth["AccessToken"]);
        let user_id = str_of(&auth["User"]["Id"]);
        if token.is_empty() || user_id.is_empty() {
            bail!("Jellyfin sign-in: unexpected answer from the server");
        }
        Ok(ServerConfig {
            kind: ServerKind::Jellyfin,
            name,
            url: url.to_string(),
            user: user.to_string(),
            token,
            user_id,
            device_id,
        })
    }

    pub fn albums(cfg: &ServerConfig) -> Result<Vec<ServerAlbum>> {
        let json = get(
            cfg,
            "/Items",
            &[
                ("userId", &cfg.user_id),
                ("IncludeItemTypes", "MusicAlbum"),
                ("Recursive", "true"),
                ("Fields", "ProductionYear"),
                ("EnableImageTypes", "Primary"),
                ("ImageTypeLimit", "1"),
            ],
        )?;
        Ok(parse_albums(cfg, &json))
    }

    pub(super) fn parse_albums(cfg: &ServerConfig, json: &Value) -> Vec<ServerAlbum> {
        let Some(items) = json["Items"].as_array() else { return Vec::new() };
        items
            .iter()
            .filter_map(|it| {
                let id = it["Id"].as_str()?.to_string();
                let artist = it["AlbumArtist"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| it["AlbumArtists"][0]["Name"].as_str().map(str::to_string))
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| "Unknown Artist".into());
                let image_url = it["ImageTags"]["Primary"].as_str().map(|tag| {
                    format!("{}/Items/{id}/Images/Primary?maxHeight=400&quality=90&tag={tag}&api_key={}", cfg.url, cfg.token)
                });
                Some(ServerAlbum {
                    title: it["Name"].as_str().filter(|n| !n.is_empty()).unwrap_or("Unknown Album").to_string(),
                    artist,
                    year: u32_of(&it["ProductionYear"]),
                    image_url,
                    id,
                })
            })
            .collect()
    }

    pub fn album_tracks(cfg: &ServerConfig, album: &ServerAlbum) -> Result<Vec<Track>> {
        let json = get(
            cfg,
            "/Items",
            &[
                ("userId", &cfg.user_id),
                ("ParentId", &album.id),
                ("IncludeItemTypes", "Audio"),
                ("Recursive", "true"),
                ("SortBy", "ParentIndexNumber,IndexNumber,SortName"),
                ("Fields", "ProductionYear,Container"),
            ],
        )?;
        Ok(parse_tracks(cfg, album, &json))
    }

    pub(super) fn parse_tracks(cfg: &ServerConfig, album: &ServerAlbum, json: &Value) -> Vec<Track> {
        let Some(items) = json["Items"].as_array() else { return Vec::new() };
        items
            .iter()
            .filter_map(|it| {
                let id = it["Id"].as_str()?.to_string();
                let artist = it["Artists"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| album.artist.clone());
                // Container can be a list ("mov,mp4,m4a,…"); take the audio one.
                let ext = it["Container"].as_str().and_then(|c| {
                    let parts: Vec<&str> = c.split(',').collect();
                    parts.iter().find(|p| **p == "m4a").or(parts.first()).map(|p| p.to_string())
                });
                Some(Track {
                    path: track_key(cfg, &id),
                    title: str_of(&it["Name"]),
                    artist,
                    album: album.title.clone(),
                    album_artist: album.artist.clone(),
                    track_no: u32_of(&it["IndexNumber"]),
                    disc_no: u32_of(&it["ParentIndexNumber"]),
                    year: u32_of(&it["ProductionYear"]).max(album.year),
                    duration_ms: it["RunTimeTicks"].as_u64().unwrap_or(0) / 10_000,
                    stream_url: Some(format!(
                        "{}/Audio/{id}/stream?static=true&deviceId={}&api_key={}",
                        cfg.url, cfg.device_id, cfg.token
                    )),
                    stream_ext: ext,
                    ..Default::default()
                })
            })
            .collect()
    }
}

mod plex {
    use super::*;

    const PRODUCT: &str = "Unitn";

    fn get(cfg: &ServerConfig, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut req = agent()
            .get(&format!("{}{path}", cfg.url))
            .set("Accept", "application/json")
            .set("X-Plex-Token", &cfg.token)
            .set("X-Plex-Client-Identifier", &cfg.device_id)
            .set("X-Plex-Product", PRODUCT);
        for (k, v) in query {
            req = req.query(k, v);
        }
        Ok(req.call().map_err(|e| explain(e, "Plex"))?.into_json()?)
    }

    pub fn connect(url: &str, user: &str, secret: &str, device_id: String) -> Result<ServerConfig> {
        let secret = secret.trim();
        if secret.is_empty() {
            bail!("Enter your Plex password (or a Plex token)");
        }
        // With a user name: sign in to plex.tv for a token. Otherwise the
        // secret is a token already.
        let token = if user.is_empty() {
            secret.to_string()
        } else {
            let resp: Value = agent()
                .post("https://plex.tv/users/sign_in.json")
                .set("Accept", "application/json")
                .set("X-Plex-Client-Identifier", &device_id)
                .set("X-Plex-Product", PRODUCT)
                .set("X-Plex-Version", env!("CARGO_PKG_VERSION"))
                .send_form(&[("user[login]", user), ("user[password]", secret)])
                .map_err(|e| explain(e, "Plex sign-in"))?
                .into_json()?;
            let token = str_of(&resp["user"]["authToken"]);
            if token.is_empty() {
                bail!("Plex sign-in failed (with two-factor sign-in, add the 6-digit code right after your password)");
            }
            token
        };
        let mut cfg = ServerConfig {
            kind: ServerKind::Plex,
            name: "Plex".into(),
            url: url.to_string(),
            user: user.to_string(),
            token,
            user_id: String::new(),
            device_id,
        };
        let root = get(&cfg, "/", &[])?;
        if let Some(name) = root["MediaContainer"]["friendlyName"].as_str().filter(|n| !n.is_empty()) {
            cfg.name = name.to_string();
        }
        if music_sections(&cfg)?.is_empty() {
            bail!("Connected, but this Plex server has no music library");
        }
        Ok(cfg)
    }

    fn music_sections(cfg: &ServerConfig) -> Result<Vec<String>> {
        let json = get(cfg, "/library/sections", &[])?;
        Ok(json["MediaContainer"]["Directory"]
            .as_array()
            .map(|dirs| {
                dirs.iter()
                    .filter(|d| d["type"].as_str() == Some("artist"))
                    .filter_map(|d| d["key"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn albums(cfg: &ServerConfig) -> Result<Vec<ServerAlbum>> {
        let mut out = Vec::new();
        for section in music_sections(cfg)? {
            let json = get(cfg, &format!("/library/sections/{section}/all"), &[("type", "9")])?;
            out.extend(parse_albums(cfg, &json));
        }
        Ok(out)
    }

    pub(super) fn parse_albums(cfg: &ServerConfig, json: &Value) -> Vec<ServerAlbum> {
        let Some(items) = json["MediaContainer"]["Metadata"].as_array() else { return Vec::new() };
        items
            .iter()
            .filter_map(|it| {
                let id = it["ratingKey"].as_str()?.to_string();
                let image_url = it["thumb"].as_str().map(|thumb| {
                    format!(
                        "{}/photo/:/transcode?width=400&height=400&minSize=1&upscale=1&url={}&X-Plex-Token={}",
                        cfg.url,
                        percent_encode(thumb),
                        cfg.token
                    )
                });
                Some(ServerAlbum {
                    title: it["title"].as_str().filter(|n| !n.is_empty()).unwrap_or("Unknown Album").to_string(),
                    artist: it["parentTitle"].as_str().filter(|n| !n.is_empty()).unwrap_or("Unknown Artist").to_string(),
                    year: u32_of(&it["year"]),
                    image_url,
                    id,
                })
            })
            .collect()
    }

    pub fn album_tracks(cfg: &ServerConfig, album: &ServerAlbum) -> Result<Vec<Track>> {
        let json = get(cfg, &format!("/library/metadata/{}/children", album.id), &[])?;
        Ok(parse_tracks(cfg, album, &json))
    }

    pub(super) fn parse_tracks(cfg: &ServerConfig, album: &ServerAlbum, json: &Value) -> Vec<Track> {
        let Some(items) = json["MediaContainer"]["Metadata"].as_array() else { return Vec::new() };
        items
            .iter()
            .filter_map(|it| {
                let id = it["ratingKey"].as_str()?.to_string();
                let media = &it["Media"][0];
                let part = media["Part"][0]["key"].as_str()?;
                let artist = it["originalTitle"]
                    .as_str()
                    .or(it["grandparentTitle"].as_str())
                    .filter(|a| !a.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| album.artist.clone());
                Some(Track {
                    path: track_key(cfg, &id),
                    title: str_of(&it["title"]),
                    artist,
                    album: album.title.clone(),
                    album_artist: album.artist.clone(),
                    track_no: u32_of(&it["index"]),
                    disc_no: u32_of(&it["parentIndex"]),
                    year: album.year,
                    duration_ms: it["duration"].as_u64().unwrap_or(0),
                    stream_url: Some(format!("{}{part}?X-Plex-Token={}", cfg.url, cfg.token)),
                    stream_ext: media["container"].as_str().map(str::to_string),
                    ..Default::default()
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(kind: ServerKind) -> ServerConfig {
        ServerConfig {
            kind,
            name: "Home".into(),
            url: "http://nas:8096".into(),
            user: "me".into(),
            token: "TOK".into(),
            user_id: "U1".into(),
            device_id: "dev".into(),
        }
    }

    #[test]
    fn normalizes_addresses() {
        assert_eq!(normalize_url("nas.local", ServerKind::Jellyfin), "http://nas.local:8096");
        assert_eq!(normalize_url("192.168.1.5", ServerKind::Plex), "http://192.168.1.5:32400");
        assert_eq!(normalize_url("https://music.example.com/", ServerKind::Jellyfin), "https://music.example.com");
        assert_eq!(normalize_url("nas:9001", ServerKind::Jellyfin), "http://nas:9001");
        assert_eq!(normalize_url("http://nas:9000/jellyfin/", ServerKind::Jellyfin), "http://nas:9000/jellyfin");
        assert_eq!(normalize_url("https://jf.example.com/jellyfin", ServerKind::Jellyfin), "https://jf.example.com/jellyfin");
    }

    #[test]
    fn parses_jellyfin() {
        let c = cfg(ServerKind::Jellyfin);
        let albums: Value = serde_json::from_str(
            r#"{"Items":[{"Id":"A1","Name":"Ghosts","AlbumArtist":"NIN","ProductionYear":2008,"ImageTags":{"Primary":"t1"}},
                         {"Id":"A2","Name":"","AlbumArtists":[{"Name":"X"}]}]}"#,
        )
        .unwrap();
        let a = jellyfin::parse_albums(&c, &albums);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].artist, "NIN");
        assert!(a[0].image_url.as_deref().unwrap().contains("/Items/A1/Images/Primary"));
        assert_eq!(a[1].title, "Unknown Album");
        assert_eq!(a[1].artist, "X");
        assert!(a[1].image_url.is_none());

        let tracks: Value = serde_json::from_str(
            r#"{"Items":[{"Id":"T1","Name":"One","Artists":["NIN"],"IndexNumber":1,"ParentIndexNumber":1,
                          "RunTimeTicks":1234560000,"Container":"flac"}]}"#,
        )
        .unwrap();
        let t = jellyfin::parse_tracks(&c, &a[0], &tracks);
        assert_eq!(t[0].duration_ms, 123_456);
        assert_eq!(t[0].path, std::path::PathBuf::from("jellyfin://nas:8096/T1"));
        assert_eq!(t[0].stream_ext.as_deref(), Some("flac"));
        assert!(t[0].stream_url.as_deref().unwrap().starts_with("http://nas:8096/Audio/T1/stream?static=true"));
        assert_eq!(t[0].year, 2008);
    }

    #[test]
    fn parses_plex() {
        let c = cfg(ServerKind::Plex);
        let albums: Value = serde_json::from_str(
            r#"{"MediaContainer":{"Metadata":[{"ratingKey":"10","title":"Kid A","parentTitle":"Radiohead","year":2000,
                "thumb":"/library/metadata/10/thumb/1"}]}}"#,
        )
        .unwrap();
        let a = plex::parse_albums(&c, &albums);
        assert_eq!(a[0].id, "10");
        assert!(a[0].image_url.as_deref().unwrap().contains("url=%2Flibrary%2Fmetadata%2F10%2Fthumb%2F1"));
        let tracks: Value = serde_json::from_str(
            r#"{"MediaContainer":{"Metadata":[{"ratingKey":"11","title":"Idioteque","grandparentTitle":"Radiohead",
                "index":8,"parentIndex":1,"duration":309000,
                "Media":[{"container":"flac","Part":[{"key":"/library/parts/5/123/file.flac"}]}]}]}}"#,
        )
        .unwrap();
        let t = plex::parse_tracks(&c, &a[0], &tracks);
        assert_eq!(t[0].track_no, 8);
        assert_eq!(t[0].stream_url.as_deref(), Some("http://nas:8096/library/parts/5/123/file.flac?X-Plex-Token=TOK"));
        assert_eq!(t[0].source(), crate::audio::Source::Remote {
            url: "http://nas:8096/library/parts/5/123/file.flac?X-Plex-Token=TOK".into(),
            ext: Some("flac".into())
        });
    }
}
