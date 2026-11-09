//! Music from your own media servers: Jellyfin, Plex and OpenSubsonic
//! servers (Navidrome, Gonic, Airsonic and other Subsonic-compatible ones).
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
    /// OpenSubsonic / Subsonic API (Navidrome, Gonic, Airsonic, …).
    Subsonic,
}

impl ServerKind {
    pub fn label(self) -> &'static str {
        match self {
            ServerKind::Jellyfin => "Jellyfin",
            ServerKind::Plex => "Plex",
            ServerKind::Subsonic => "Subsonic",
        }
    }

    fn scheme(self) -> &'static str {
        match self {
            ServerKind::Jellyfin => "jellyfin",
            ServerKind::Plex => "plex",
            ServerKind::Subsonic => "subsonic",
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
    /// Access token (never the password). For Subsonic: `token:salt`.
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
            ServerKind::Subsonic => ":4533",
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
        ServerKind::Subsonic => subsonic::connect(&url, user.trim(), secret, device_id),
    }
}

/// Up to 50 songs on the server matching `query` (title, artist or album).
pub fn search_tracks(cfg: &ServerConfig, query: &str) -> Result<Vec<Track>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    match cfg.kind {
        ServerKind::Jellyfin => jellyfin::search(cfg, query),
        ServerKind::Plex => plex::search(cfg, query),
        ServerKind::Subsonic => subsonic::search(cfg, query),
    }
}

/// The address a server song streams from, with the current credentials.
/// `stream_ref` is what [`stream_ref`] returned (a path without tokens).
pub fn stream_url(cfg: &ServerConfig, stream_ref: &str) -> String {
    let sep = if stream_ref.contains('?') { '&' } else { '?' };
    let auth = match cfg.kind {
        ServerKind::Jellyfin => format!("deviceId={}&api_key={}", cfg.device_id, cfg.token),
        ServerKind::Plex => format!("X-Plex-Token={}", cfg.token),
        ServerKind::Subsonic => subsonic::auth_query(cfg),
    };
    format!("{}{stream_ref}{sep}{auth}", cfg.url)
}

/// The part of a song's stream address that can be saved (in playlists):
/// without the server address and without any credentials.
pub fn stream_ref(cfg: &ServerConfig, stream_url: &str) -> Option<String> {
    let rest = stream_url.strip_prefix(cfg.url.as_str())?;
    let marker = match cfg.kind {
        ServerKind::Jellyfin => "deviceId=",
        ServerKind::Plex => "X-Plex-Token=",
        ServerKind::Subsonic => "u=",
    };
    let cut = ['?', '&'].iter().filter_map(|c| rest.find(&format!("{c}{marker}"))).min()?;
    Some(rest[..cut].to_string())
}

/// Whether a song key (`Track::path` of a server song) belongs to `cfg`.
pub fn owns_track(cfg: &ServerConfig, key: &std::path::Path) -> bool {
    let key = key.to_string_lossy();
    key.starts_with(&format!("{}://{}/", cfg.kind.scheme(), host_of(&cfg.url)))
}

/// A pending "Sign in with Plex" request: the user approves it on plex.tv
/// in the browser (where two-factor sign-in works), then the app picks up
/// the token with [`plex_pin_token`].
#[derive(Debug, Clone)]
pub struct PlexPin {
    pub id: u64,
    /// Short code, also usable on plex.tv/link.
    pub code: String,
    /// Page to open in the browser.
    pub auth_url: String,
    pub device_id: String,
}

/// Starts a browser sign-in with Plex.
pub fn plex_pin_start() -> Result<PlexPin> {
    plex::pin_start(new_device_id())
}

/// The account token once the user approved the sign-in in the browser,
/// `None` while still waiting.
pub fn plex_pin_token(pin: &PlexPin) -> Result<Option<String>> {
    plex::pin_token(pin)
}

/// Connects to a Plex server with the account token from the browser
/// sign-in. With an empty `url` the user's servers are looked up on plex.tv
/// and the first reachable one with a music library is used.
pub fn plex_connect_account(url: &str, account_token: &str, device_id: String) -> Result<ServerConfig> {
    let url = if url.trim().is_empty() { String::new() } else { normalize_url(url, ServerKind::Plex) };
    plex::connect_account(&url, account_token, device_id)
}

/// All albums on the server, sorted by artist and title.
pub fn albums(cfg: &ServerConfig) -> Result<Vec<ServerAlbum>> {
    let mut list = match cfg.kind {
        ServerKind::Jellyfin => jellyfin::albums(cfg)?,
        ServerKind::Plex => plex::albums(cfg)?,
        ServerKind::Subsonic => subsonic::albums(cfg)?,
    };
    list.sort_by_cached_key(|a| (a.artist.to_lowercase(), a.title.to_lowercase()));
    Ok(list)
}

/// The songs of an album, in disc and track order, ready to play.
pub fn album_tracks(cfg: &ServerConfig, album: &ServerAlbum) -> Result<Vec<Track>> {
    let mut tracks = match cfg.kind {
        ServerKind::Jellyfin => jellyfin::album_tracks(cfg, album)?,
        ServerKind::Plex => plex::album_tracks(cfg, album)?,
        ServerKind::Subsonic => subsonic::album_tracks(cfg, album)?,
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
        Ok(parse_tracks(cfg, Some(album), &json))
    }

    pub fn search(cfg: &ServerConfig, query: &str) -> Result<Vec<Track>> {
        let json = get(
            cfg,
            "/Items",
            &[
                ("userId", &cfg.user_id),
                ("searchTerm", query),
                ("IncludeItemTypes", "Audio"),
                ("Recursive", "true"),
                ("Limit", "50"),
                ("Fields", "ProductionYear,Container"),
            ],
        )?;
        Ok(parse_tracks(cfg, None, &json))
    }

    /// Songs from an `/Items` answer. Without `album`, the album comes from
    /// each song's own fields (search results).
    pub(super) fn parse_tracks(cfg: &ServerConfig, album: Option<&ServerAlbum>, json: &Value) -> Vec<Track> {
        let Some(items) = json["Items"].as_array() else { return Vec::new() };
        items
            .iter()
            .filter_map(|it| {
                let id = it["Id"].as_str()?.to_string();
                let (album_title, album_artist, album_year) = match album {
                    Some(a) => (a.title.clone(), a.artist.clone(), a.year),
                    None => (
                        str_of(&it["Album"]),
                        it["AlbumArtist"].as_str().filter(|a| !a.is_empty()).unwrap_or("Unknown Artist").to_string(),
                        0,
                    ),
                };
                let artist = it["Artists"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| album_artist.clone());
                // Container can be a list ("mov,mp4,m4a,…"); take the audio one.
                let ext = it["Container"].as_str().and_then(|c| {
                    let parts: Vec<&str> = c.split(',').collect();
                    parts.iter().find(|p| **p == "m4a").or(parts.first()).map(|p| p.to_string())
                });
                Some(Track {
                    path: track_key(cfg, &id),
                    title: str_of(&it["Name"]),
                    artist,
                    album: album_title,
                    album_artist,
                    track_no: u32_of(&it["IndexNumber"]),
                    disc_no: u32_of(&it["ParentIndexNumber"]),
                    year: u32_of(&it["ProductionYear"]).max(album_year),
                    duration_ms: it["RunTimeTicks"].as_u64().unwrap_or(0) / 10_000,
                    stream_url: Some(stream_url(cfg, &format!("/Audio/{id}/stream?static=true"))),
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
        finish(url, user, token, device_id)
    }

    fn plex_tv(method: &str, path: &str, device_id: &str) -> ureq::Request {
        agent()
            .request(method, &format!("https://plex.tv{path}"))
            .set("Accept", "application/json")
            .set("X-Plex-Client-Identifier", device_id)
            .set("X-Plex-Product", PRODUCT)
            .set("X-Plex-Version", env!("CARGO_PKG_VERSION"))
    }

    pub fn pin_start(device_id: String) -> Result<PlexPin> {
        let resp: Value = plex_tv("POST", "/api/v2/pins", &device_id)
            .send_form(&[("strong", "false")])
            .map_err(|e| explain(e, "Plex sign-in"))?
            .into_json()?;
        let id = resp["id"].as_u64().ok_or_else(|| anyhow!("Plex sign-in: unexpected answer from plex.tv"))?;
        let code = str_of(&resp["code"]);
        if code.is_empty() {
            bail!("Plex sign-in: unexpected answer from plex.tv");
        }
        let auth_url = format!(
            "https://app.plex.tv/auth#?clientID={}&code={}&context%5Bdevice%5D%5Bproduct%5D={}",
            percent_encode(&device_id),
            percent_encode(&code),
            percent_encode("Unitn player")
        );
        Ok(PlexPin { id, code, auth_url, device_id })
    }

    pub fn pin_token(pin: &PlexPin) -> Result<Option<String>> {
        let resp: Value = plex_tv("GET", &format!("/api/v2/pins/{}", pin.id), &pin.device_id)
            .call()
            .map_err(|e| match e {
                ureq::Error::Status(404, _) => anyhow!("The Plex sign-in expired. Please try again."),
                e => explain(e, "Plex sign-in"),
            })?
            .into_json()?;
        let token = str_of(&resp["authToken"]);
        Ok((!token.is_empty()).then_some(token))
    }

    /// A server from plex.tv's resource list.
    struct Resource {
        token: String,
        owned: bool,
        /// Connection addresses, best first (local, then remote, then relay).
        uris: Vec<String>,
    }

    fn resources(account_token: &str, device_id: &str) -> Result<Vec<Resource>> {
        let json: Value = plex_tv("GET", "/api/v2/resources", device_id)
            .query("includeHttps", "1")
            .query("includeRelay", "1")
            .set("X-Plex-Token", account_token)
            .call()
            .map_err(|e| explain(e, "Plex"))?
            .into_json()?;
        let mut out: Vec<Resource> = json
            .as_array()
            .map(|list| {
                list.iter()
                    .filter(|r| str_of(&r["provides"]).split(',').any(|p| p == "server"))
                    .map(|r| {
                        let mut conns: Vec<(u8, String)> = r["connections"]
                            .as_array()
                            .map(|cs| {
                                cs.iter()
                                    .filter_map(|c| {
                                        let uri = str_of(&c["uri"]);
                                        let rank = if c["relay"].as_bool() == Some(true) {
                                            2
                                        } else if c["local"].as_bool() == Some(true) {
                                            0
                                        } else {
                                            1
                                        };
                                        (!uri.is_empty()).then_some((rank, uri.trim_end_matches('/').to_string()))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        conns.sort_by_key(|c| c.0);
                        Resource {
                            token: str_of(&r["accessToken"]),
                            owned: r["owned"].as_bool().unwrap_or(false),
                            uris: conns.into_iter().map(|c| c.1).collect(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|r| !r.owned);
        Ok(out)
    }

    /// `host:port` of an address, for comparing what the user typed with
    /// plex.tv's connection list.
    pub(super) fn host_port(url: &str) -> String {
        let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
        let host = rest.split('/').next().unwrap_or(rest).to_ascii_lowercase();
        if host.contains(':') {
            host
        } else if url.starts_with("https://") {
            format!("{host}:443")
        } else {
            format!("{host}:80")
        }
    }

    pub fn connect_account(url: &str, account_token: &str, device_id: String) -> Result<ServerConfig> {
        let found = resources(account_token, &device_id).unwrap_or_else(|e| {
            crate::log::log(format!("servers: plex.tv resource list failed: {e:#}"));
            Vec::new()
        });
        if !url.is_empty() {
            // Shared servers need their own access token; an owned server
            // also accepts the account token.
            let want = host_port(url);
            let token = found
                .iter()
                .find(|r| r.uris.iter().any(|u| host_port(u) == want))
                .map(|r| r.token.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| account_token.to_string());
            return finish(url, "", token, device_id);
        }
        if found.is_empty() {
            bail!("No Plex server found on your account. Enter the server's address.");
        }
        let mut last_err = None;
        for res in &found {
            let token = if res.token.is_empty() { account_token.to_string() } else { res.token.clone() };
            for uri in &res.uris {
                let probe = agent()
                    .get(&format!("{uri}/identity"))
                    .timeout(Duration::from_secs(4))
                    .set("Accept", "application/json")
                    .set("X-Plex-Token", &token)
                    .call();
                if probe.is_err() {
                    continue;
                }
                match finish(uri, "", token.clone(), device_id.clone()) {
                    Ok(cfg) => return Ok(cfg),
                    Err(e) => {
                        last_err = Some(e);
                        break; // reachable, but no music library: try the next server
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("Couldn't reach your Plex server. Enter its address.")))
    }

    fn finish(url: &str, user: &str, token: String, device_id: String) -> Result<ServerConfig> {
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
        Ok(parse_tracks(cfg, Some(album), &json["MediaContainer"]["Metadata"]))
    }

    pub fn search(cfg: &ServerConfig, query: &str) -> Result<Vec<Track>> {
        let json = get(cfg, "/hubs/search", &[("query", query), ("limit", "50")])?;
        Ok(parse_search(cfg, &json))
    }

    pub(super) fn parse_search(cfg: &ServerConfig, json: &Value) -> Vec<Track> {
        json["MediaContainer"]["Hub"]
            .as_array()
            .map(|hubs| {
                hubs.iter()
                    .filter(|h| h["type"].as_str() == Some("track"))
                    .flat_map(|h| parse_tracks(cfg, None, &h["Metadata"]))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Songs from a `Metadata` list. Without `album`, the album comes from
    /// each song's own fields (search results).
    pub(super) fn parse_tracks(cfg: &ServerConfig, album: Option<&ServerAlbum>, items: &Value) -> Vec<Track> {
        let Some(items) = items.as_array() else { return Vec::new() };
        items
            .iter()
            .filter_map(|it| {
                let id = it["ratingKey"].as_str()?.to_string();
                let media = &it["Media"][0];
                let part = media["Part"][0]["key"].as_str()?;
                let (album_title, album_artist, album_year) = match album {
                    Some(a) => (a.title.clone(), a.artist.clone(), a.year),
                    None => (
                        str_of(&it["parentTitle"]),
                        it["grandparentTitle"].as_str().filter(|a| !a.is_empty()).unwrap_or("Unknown Artist").to_string(),
                        u32_of(&it["parentYear"]).max(u32_of(&it["year"])),
                    ),
                };
                let artist = it["originalTitle"]
                    .as_str()
                    .or(it["grandparentTitle"].as_str())
                    .filter(|a| !a.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| album_artist.clone());
                Some(Track {
                    path: track_key(cfg, &id),
                    title: str_of(&it["title"]),
                    artist,
                    album: album_title,
                    album_artist,
                    track_no: u32_of(&it["index"]),
                    disc_no: u32_of(&it["parentIndex"]),
                    year: album_year,
                    duration_ms: it["duration"].as_u64().unwrap_or(0),
                    stream_url: Some(stream_url(cfg, part)),
                    stream_ext: media["container"].as_str().map(str::to_string),
                    ..Default::default()
                })
            })
            .collect()
    }
}

mod subsonic {
    use super::*;

    const API_VERSION: &str = "1.16.1";

    /// Credentials for every request: user, token = md5(password + salt), salt.
    pub fn auth_query(cfg: &ServerConfig) -> String {
        let (token, salt) = cfg.token.split_once(':').unwrap_or((&cfg.token, ""));
        format!("u={}&t={token}&s={salt}&v={API_VERSION}&c=Unitn", percent_encode(&cfg.user))
    }

    fn get(cfg: &ServerConfig, method: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut url = format!("{}/rest/{method}.view?{}&f=json", cfg.url, auth_query(cfg));
        for (k, v) in query {
            url.push_str(&format!("&{k}={}", percent_encode(v)));
        }
        let json: Value = agent()
            .get(&url)
            .set("Accept", "application/json")
            .call()
            .map_err(|e| explain(e, "Subsonic"))?
            .into_json()
            .context("this doesn't look like a Subsonic / Navidrome server")?;
        let resp = &json["subsonic-response"];
        if resp["status"].as_str() != Some("ok") {
            let code = resp["error"]["code"].as_u64().unwrap_or(0);
            let msg = resp["error"]["message"].as_str().unwrap_or("unknown error");
            if code == 40 || code == 41 {
                bail!("Subsonic: wrong user name or password");
            }
            if resp.is_null() {
                bail!("this doesn't look like a Subsonic / Navidrome server");
            }
            bail!("Subsonic: {msg}");
        }
        Ok(resp.clone())
    }

    pub fn connect(url: &str, user: &str, password: &str, device_id: String) -> Result<ServerConfig> {
        if user.is_empty() {
            bail!("Enter your user name");
        }
        if password.is_empty() {
            bail!("Enter your password");
        }
        // The password itself is never stored: only md5(password + salt)
        // with a random salt, which the server checks on every request.
        let salt = new_device_id().trim_start_matches("unitn-").to_string();
        let token = super::md5_hex(format!("{password}{salt}").as_bytes());
        let mut cfg = ServerConfig {
            kind: ServerKind::Subsonic,
            name: "Subsonic".into(),
            url: url.to_string(),
            user: user.to_string(),
            token: format!("{token}:{salt}"),
            user_id: String::new(),
            device_id,
        };
        let ping = get(&cfg, "ping", &[])?;
        if let Some(kind) = ping["type"].as_str().filter(|t| !t.is_empty()) {
            let mut name: Vec<char> = kind.chars().collect();
            name[0] = name[0].to_ascii_uppercase();
            cfg.name = name.into_iter().collect();
        }
        Ok(cfg)
    }

    pub fn albums(cfg: &ServerConfig) -> Result<Vec<ServerAlbum>> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let off = offset.to_string();
            let json = get(cfg, "getAlbumList2", &[("type", "alphabeticalByArtist"), ("size", "500"), ("offset", &off)])?;
            let page = parse_albums(cfg, &json["albumList2"]["album"]);
            let n = page.len();
            out.extend(page);
            offset += n;
            if n < 500 || offset > 100_000 {
                break;
            }
        }
        Ok(out)
    }

    pub(super) fn parse_albums(cfg: &ServerConfig, list: &Value) -> Vec<ServerAlbum> {
        let Some(items) = list.as_array() else { return Vec::new() };
        items
            .iter()
            .filter_map(|it| {
                let id = id_of(&it["id"])?;
                let image_url = id_of(&it["coverArt"]).map(|art| {
                    format!("{}/rest/getCoverArt.view?id={}&size=400&{}", cfg.url, percent_encode(&art), auth_query(cfg))
                });
                Some(ServerAlbum {
                    title: it["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("Unknown Album").to_string(),
                    artist: it["artist"].as_str().filter(|n| !n.is_empty()).unwrap_or("Unknown Artist").to_string(),
                    year: u32_of(&it["year"]),
                    image_url,
                    id,
                })
            })
            .collect()
    }

    pub fn album_tracks(cfg: &ServerConfig, album: &ServerAlbum) -> Result<Vec<Track>> {
        let json = get(cfg, "getAlbum", &[("id", &album.id)])?;
        Ok(parse_songs(cfg, Some(album), &json["album"]["song"]))
    }

    pub fn search(cfg: &ServerConfig, query: &str) -> Result<Vec<Track>> {
        let json = get(cfg, "search3", &[("query", query), ("songCount", "50"), ("albumCount", "0"), ("artistCount", "0")])?;
        Ok(parse_songs(cfg, None, &json["searchResult3"]["song"]))
    }

    /// Ids are strings on most servers, numbers on some.
    fn id_of(v: &Value) -> Option<String> {
        match v {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }

    pub(super) fn parse_songs(cfg: &ServerConfig, album: Option<&ServerAlbum>, list: &Value) -> Vec<Track> {
        let Some(items) = list.as_array() else { return Vec::new() };
        items
            .iter()
            .filter(|it| it["isVideo"].as_bool() != Some(true))
            .filter_map(|it| {
                let id = id_of(&it["id"])?;
                let artist = it["artist"].as_str().filter(|a| !a.is_empty()).map(str::to_string);
                let (album_title, album_artist, album_year) = match album {
                    Some(a) => (a.title.clone(), a.artist.clone(), a.year),
                    None => (
                        str_of(&it["album"]),
                        artist.clone().unwrap_or_else(|| "Unknown Artist".into()),
                        0,
                    ),
                };
                Some(Track {
                    path: track_key(cfg, &id),
                    title: str_of(&it["title"]),
                    artist: artist.unwrap_or_else(|| album_artist.clone()),
                    album: album_title,
                    album_artist,
                    track_no: u32_of(&it["track"]),
                    disc_no: u32_of(&it["discNumber"]),
                    year: u32_of(&it["year"]).max(album_year),
                    duration_ms: it["duration"].as_u64().unwrap_or(0) * 1000,
                    stream_url: Some(stream_url(cfg, &format!("/rest/stream.view?id={}&format=raw", percent_encode(&id)))),
                    stream_ext: it["suffix"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
                    ..Default::default()
                })
            })
            .collect()
    }
}

/// MD5 as lowercase hex (for Subsonic's token authentication).
pub(crate) fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14,
        20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6,
        10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0].iter().flat_map(|w| w.to_le_bytes()).map(|b| format!("{b:02x}")).collect()
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
        let t = jellyfin::parse_tracks(&c, Some(&a[0]), &tracks);
        assert_eq!(t[0].duration_ms, 123_456);
        assert_eq!(t[0].path, std::path::PathBuf::from("jellyfin://nas:8096/T1"));
        assert_eq!(t[0].stream_ext.as_deref(), Some("flac"));
        assert!(t[0].stream_url.as_deref().unwrap().starts_with("http://nas:8096/Audio/T1/stream?static=true"));
        assert_eq!(t[0].year, 2008);
    }

    #[test]
    fn plex_host_port() {
        assert_eq!(plex::host_port("http://192.168.1.5:32400"), "192.168.1.5:32400");
        assert_eq!(plex::host_port("https://10-0-0-2.abc.plex.direct:32400/"), "10-0-0-2.abc.plex.direct:32400");
        assert_eq!(plex::host_port("https://plex.example.com"), "plex.example.com:443");
        assert_eq!(plex::host_port("http://NAS/plex"), "nas:80");
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
        let t = plex::parse_tracks(&c, Some(&a[0]), &tracks["MediaContainer"]["Metadata"]);
        assert_eq!(t[0].track_no, 8);
        assert_eq!(t[0].stream_url.as_deref(), Some("http://nas:8096/library/parts/5/123/file.flac?X-Plex-Token=TOK"));
        assert_eq!(t[0].source(), crate::audio::Source::Remote {
            url: "http://nas:8096/library/parts/5/123/file.flac?X-Plex-Token=TOK".into(),
            ext: Some("flac".into())
        });
    }

    #[test]
    fn md5_matches_reference() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"The quick brown fox jumps over the lazy dog"), "9e107d9d372bb6826bd81d3542a419d6");
        // Example from the Subsonic API documentation: password "sesame", salt "c19b2d".
        assert_eq!(md5_hex(b"sesamec19b2d"), "26719a1196d2a940705a59634eb18eab");
        let long = vec![b'a'; 1000];
        assert_eq!(md5_hex(&long), "cabe45dcc9ae5b66ba86600cca6b8ba8");
    }

    #[test]
    fn stream_refs_drop_credentials() {
        for kind in [ServerKind::Jellyfin, ServerKind::Plex, ServerKind::Subsonic] {
            let mut c = cfg(kind);
            if kind == ServerKind::Subsonic {
                c.token = "abc:salt1".into();
            }
            for r in ["/Audio/T1/stream?static=true", "/library/parts/5/123/file.flac", "/rest/stream.view?id=s%201&format=raw"] {
                let url = stream_url(&c, r);
                assert!(!stream_ref(&c, &url).unwrap().contains("TOK"));
                assert_eq!(stream_ref(&c, &url).as_deref(), Some(r), "{kind:?} {r}");
            }
        }
        let c = cfg(ServerKind::Subsonic);
        assert!(owns_track(&c, std::path::Path::new("subsonic://nas:8096/42")));
        assert!(!owns_track(&c, std::path::Path::new("plex://nas:8096/42")));
    }

    #[test]
    fn parses_subsonic() {
        let mut c = cfg(ServerKind::Subsonic);
        c.token = "26719a1196d2a940705a59634eb18eab:c19b2d".into();
        let albums: Value = serde_json::from_str(
            r#"[{"id":"al-1","name":"Blue Train","artist":"John Coltrane","year":1957,"coverArt":"al-1"},{"id":7,"name":""}]"#,
        )
        .unwrap();
        let a = subsonic::parse_albums(&c, &albums);
        assert_eq!(a.len(), 2);
        assert_eq!(a[1].id, "7");
        assert_eq!(a[1].title, "Unknown Album");
        let img = a[0].image_url.as_deref().unwrap();
        assert!(img.starts_with("http://nas:8096/rest/getCoverArt.view?id=al-1&size=400&u=me&t=26719a1196d2a940705a59634eb18eab&s=c19b2d"));
        let songs: Value = serde_json::from_str(
            r#"[{"id":"s1","title":"Blue Train","artist":"John Coltrane","album":"Blue Train","track":1,"discNumber":1,
                 "year":1957,"duration":643,"suffix":"flac"},{"id":"v1","title":"Clip","isVideo":true}]"#,
        )
        .unwrap();
        let t = subsonic::parse_songs(&c, Some(&a[0]), &songs);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].duration_ms, 643_000);
        assert_eq!(t[0].path, std::path::PathBuf::from("subsonic://nas:8096/s1"));
        assert_eq!(t[0].stream_ext.as_deref(), Some("flac"));
        assert!(t[0].stream_url.as_deref().unwrap().starts_with("http://nas:8096/rest/stream.view?id=s1&format=raw&u=me&t="));
        let found = subsonic::parse_songs(&c, None, &songs);
        assert_eq!(found[0].album, "Blue Train");
        assert_eq!(found[0].album_artist, "John Coltrane");
    }

    #[test]
    fn parses_search_results() {
        let c = cfg(ServerKind::Jellyfin);
        let json: Value = serde_json::from_str(
            r#"{"Items":[{"Id":"T9","Name":"Song","Artists":["A"],"Album":"Record","AlbumArtist":"Band","ProductionYear":1999}]}"#,
        )
        .unwrap();
        let t = jellyfin::parse_tracks(&c, None, &json);
        assert_eq!((t[0].album.as_str(), t[0].album_artist.as_str(), t[0].year), ("Record", "Band", 1999));

        let c = cfg(ServerKind::Plex);
        let json: Value = serde_json::from_str(
            r#"{"MediaContainer":{"Hub":[{"type":"album","Metadata":[{"ratingKey":"1"}]},
                {"type":"track","Metadata":[{"ratingKey":"12","title":"Airbag","grandparentTitle":"Radiohead",
                 "parentTitle":"OK Computer","parentYear":1997,"index":1,"duration":284000,
                 "Media":[{"container":"mp3","Part":[{"key":"/library/parts/9/1/file.mp3"}]}]}]}]}}"#,
        )
        .unwrap();
        let t = plex::parse_search(&c, &json);
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].album.as_str(), t[0].artist.as_str(), t[0].year), ("OK Computer", "Radiohead", 1997));
    }
}
