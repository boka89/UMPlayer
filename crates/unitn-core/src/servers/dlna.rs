//! DLNA / UPnP AV media servers: the media server built into most NAS boxes
//! (Twonky on older WD and Synology boxes, MiniDLNA / ReadyMedia, Serviio,
//! Plex and Jellyfin's DLNA mode, Windows media sharing).
//!
//! Servers are found with SSDP and browsed through their ContentDirectory
//! service (SOAP over HTTP, results as DIDL-Lite XML). There is no sign-in.
//! Songs are plain HTTP files and play like any other server song.
//!
//! Servers differ a lot (and old ones are not always standard), so albums
//! are found in three ways, from best to most general:
//! 1. a `Search` for album containers (MiniDLNA, newer Twonky, Serviio),
//! 2. the "Music → Album" view most servers have,
//! 3. walking the folders: a folder holding songs counts as an album.

use std::collections::{HashSet, VecDeque};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use super::{agent, explain, host_of, track_key, DlnaServer, ServerAlbum, ServerConfig, ServerKind};
use crate::model::Track;

const SSDP_ADDR: &str = "239.255.255.250:1900";
const MEDIA_SERVER: &str = "urn:schemas-upnp-org:device:MediaServer:1";
const CONTENT_DIRECTORY: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
/// Items asked for per Browse/Search call (some old servers cap lower).
const PAGE: usize = 100;
/// Most Browse calls spent walking a server's folders for albums.
const CRAWL_BUDGET: usize = 1500;
const MAX_DEPTH: usize = 10;
/// Where common servers keep their description, for addresses typed in by
/// hand when the server does not answer a direct search.
const KNOWN_DESCRIPTIONS: &[&str] = &[
    ":8200/rootDesc.xml",              // MiniDLNA / ReadyMedia
    ":9000/TMSDeviceDescription.xml",  // Twonky 5+
    ":9000/DeviceDescription.xml",     // older Twonky
    ":9000/desc/device.xml",           // older Twonky
    ":8895/deviceDescription/1",       // Serviio
    ":50001/desc/device.xml",          // some NAS media servers
];

// ---------------------------------------------------------------------------
// Discovery

fn search_message(target: &str) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_ADDR}\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {target}\r\nUSER-AGENT: {}\r\n\r\n",
        crate::USER_AGENT
    )
}

/// The LOCATION header of an SSDP answer.
fn location_of(answer: &str) -> Option<String> {
    let mut lines = answer.lines();
    let status = lines.next()?;
    if !status.starts_with("HTTP/1.") || !status.contains("200") {
        return None;
    }
    lines.find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("location").then(|| v.trim().to_string()).filter(|v| v.starts_with("http"))
    })
}

/// Sends an SSDP search to `to` and collects description addresses until
/// `wait` has passed (or, for a single host, until it answered).
fn ssdp_search(to: SocketAddr, wait: Duration, first_only: bool) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else { return found };
    let _ = socket.set_multicast_ttl_v4(2);
    for target in [MEDIA_SERVER, "ssdp:all"] {
        let _ = socket.send_to(search_message(target).as_bytes(), to);
    }
    let end = Instant::now() + wait;
    let mut buf = [0u8; 4096];
    while let Some(left) = end.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) {
        let _ = socket.set_read_timeout(Some(left));
        let Ok((n, _)) = socket.recv_from(&mut buf) else { break };
        let answer = String::from_utf8_lossy(&buf[..n]);
        if let Some(loc) = location_of(&answer) {
            if !found.contains(&loc) {
                found.push(loc);
                if first_only {
                    // Give a multi-device box a moment to name its media server.
                    let _ = socket.set_read_timeout(Some(Duration::from_millis(300)));
                    while let Ok((n, _)) = socket.recv_from(&mut buf) {
                        if let Some(l) = location_of(&String::from_utf8_lossy(&buf[..n])) {
                            if !found.contains(&l) {
                                found.push(l);
                            }
                        }
                    }
                    break;
                }
            }
        }
    }
    found
}

/// A server's description: its name and ContentDirectory address.
#[derive(Debug, Clone, PartialEq)]
struct Description {
    name: String,
    udn: String,
    control_url: String,
}

/// Resolves `href` (absolute, or relative to `base`).
fn resolve(base: &str, href: &str) -> String {
    let href = href.trim();
    if href.contains("://") {
        return href.to_string();
    }
    let origin = origin_of(base);
    if let Some(path) = href.strip_prefix('/') {
        return format!("{origin}/{path}");
    }
    // Relative to the base's folder.
    let after = &base[origin.len()..];
    let dir = after.rfind('/').map(|i| &after[..=i]).unwrap_or("/");
    format!("{origin}{dir}{href}")
}

/// `http://host:port` of an address.
fn origin_of(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let host = rest.split('/').next().unwrap_or(rest);
    format!("{scheme}://{host}")
}

fn parse_description(location: &str, xml: &str) -> Result<Description> {
    let doc = roxmltree::Document::parse(xml).context("the server's description is not valid XML")?;
    let base = doc
        .descendants()
        .find(|n| n.has_tag_name("URLBase"))
        .and_then(|n| n.text())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(location)
        .to_string();
    // The media server device may be the root device or an embedded one.
    let devices: Vec<_> = doc.descendants().filter(|n| n.has_tag_name("device")).collect();
    for device in &devices {
        let service = device
            .descendants()
            .filter(|n| n.has_tag_name("service"))
            .find(|s| child_text(*s, "serviceType").contains(":ContentDirectory:"));
        let Some(service) = service else { continue };
        let control = child_text(service, "controlURL");
        if control.is_empty() {
            continue;
        }
        let name = child_text(*device, "friendlyName");
        return Ok(Description {
            name: if name.is_empty() { "Media server".into() } else { name },
            udn: child_text(*device, "UDN"),
            control_url: resolve(&base, &control),
        });
    }
    bail!("this device is not a media server (it has no music browsing service)")
}

fn child_text(node: roxmltree::Node, name: &str) -> String {
    node.children().find(|c| c.has_tag_name(name)).and_then(|c| c.text()).unwrap_or_default().trim().to_string()
}

fn describe(location: &str) -> Result<Description> {
    let xml = agent()
        .get(location)
        .timeout(Duration::from_secs(8))
        .call()
        .map_err(|e| explain(e, "DLNA"))?
        .into_string()?;
    parse_description(location, &xml)
}

pub fn discover(wait: Duration) -> Vec<DlnaServer> {
    let Ok(to) = SSDP_ADDR.parse::<SocketAddr>() else { return Vec::new() };
    let locations = ssdp_search(to, wait, false);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for location in locations {
        if let Ok(d) = describe(&location) {
            if seen.insert(if d.udn.is_empty() { d.control_url.clone() } else { d.udn.clone() }) {
                out.push(DlnaServer { name: d.name, location });
            }
        }
    }
    out.sort_by_cached_key(|s| s.name.to_lowercase());
    out
}

/// Connects to a server: `input` is a description address (from
/// [`discover`]) or an address typed by the user (host, host:port or URL).
pub fn connect(input: &str) -> Result<ServerConfig> {
    let input = input.trim().trim_end_matches('/');
    if input.is_empty() {
        bail!("Pick a server from the list, or enter its address.");
    }
    let mut candidates: Vec<String> = Vec::new();
    let full = if input.contains("://") { input.to_string() } else { format!("http://{input}") };
    let host = host_of(&full).split('/').next().unwrap_or_default().to_string();
    let bare_host = host.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or_else(|| host.clone());
    // A full description address (has a path) is tried first.
    if full[origin_of(&full).len()..].len() > 1 {
        candidates.push(full.clone());
    }
    // Ask that host directly where its description is.
    if let Some(addr) = (bare_host.as_str(), 1900).to_socket_addrs().ok().and_then(|mut a| a.next()) {
        candidates.extend(ssdp_search(addr, Duration::from_millis(1500), true));
    }
    for known in KNOWN_DESCRIPTIONS {
        let (port, path) = known.split_at(known.find('/').unwrap_or(known.len()));
        if host.contains(':') {
            // A port was typed: try the known paths on that port.
            candidates.push(format!("http://{host}{path}"));
        } else {
            candidates.push(format!("http://{bare_host}{port}{path}"));
        }
    }
    let mut first_err: Option<anyhow::Error> = None;
    let mut tried = HashSet::new();
    for location in candidates {
        if !tried.insert(location.clone()) {
            continue;
        }
        match describe(&location) {
            Ok(d) => {
                let cfg = ServerConfig {
                    kind: ServerKind::Dlna,
                    name: d.name,
                    url: origin_of(&location),
                    user: String::new(),
                    token: String::new(),
                    user_id: d.udn,
                    device_id: location,
                    control_url: d.control_url,
                };
                // Make sure browsing works before saving it.
                browse(&cfg, "0", 0, 1).context("the server did not let Unitn browse its music")?;
                return Ok(cfg);
            }
            Err(e) => {
                if first_err.is_none() && e.to_string().contains("not a media server") {
                    first_err = Some(e);
                }
            }
        }
    }
    Err(first_err.unwrap_or_else(|| {
        anyhow!("No DLNA media server found at {input}. Check that the NAS's media server (e.g. Twonky) is switched on.")
    }))
}

// ---------------------------------------------------------------------------
// ContentDirectory

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// One page of objects and the total the server reports (0 = unknown).
struct Page {
    objects: Vec<Object>,
    returned: usize,
    total: usize,
}

fn soap(cfg: &ServerConfig, action: &str, args: &str) -> Result<Page> {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
<s:Body><u:{action} xmlns:u=\"{CONTENT_DIRECTORY}\">{args}</u:{action}></s:Body></s:Envelope>"
    );
    let resp = agent()
        .post(&cfg.control_url)
        .set("Content-Type", "text/xml; charset=\"utf-8\"")
        .set("SOAPAction", &format!("\"{CONTENT_DIRECTORY}#{action}\""))
        .send_string(&body);
    let text = match resp {
        Ok(r) => r.into_string()?,
        Err(ureq::Error::Status(500, r)) => {
            let text = r.into_string().unwrap_or_default();
            let code = between(&text, "errorCode>", "<").unwrap_or_default();
            bail!("DLNA: the server refused the request ({action}, error {code})");
        }
        Err(e) => return Err(explain(e, "DLNA")),
    };
    parse_response(&text)
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = text.find(start)? + start.len();
    let j = text[i..].find(end)? + i;
    Some(text[i..j].trim())
}

fn parse_response(text: &str) -> Result<Page> {
    let doc = roxmltree::Document::parse(text).context("DLNA: the server's answer is not valid XML")?;
    let field = |name: &str| doc.descendants().find(|n| n.has_tag_name(name)).and_then(|n| n.text()).unwrap_or_default();
    let number = |name: &str| field(name).trim().parse::<usize>().unwrap_or(0);
    let didl = field("Result");
    let objects = if didl.trim().is_empty() { Vec::new() } else { parse_didl(didl)? };
    let returned = match number("NumberReturned") {
        0 => objects.len(),
        n => n,
    };
    Ok(Page { objects, returned, total: number("TotalMatches") })
}

fn browse(cfg: &ServerConfig, id: &str, start: usize, count: usize) -> Result<Page> {
    soap(
        cfg,
        "Browse",
        &format!(
            "<ObjectID>{}</ObjectID><BrowseFlag>BrowseDirectChildren</BrowseFlag><Filter>*</Filter>\
<StartingIndex>{start}</StartingIndex><RequestedCount>{count}</RequestedCount><SortCriteria></SortCriteria>",
            xml_escape(id)
        ),
    )
}

fn search_page(cfg: &ServerConfig, criteria: &str, start: usize, count: usize) -> Result<Page> {
    soap(
        cfg,
        "Search",
        &format!(
            "<ContainerID>0</ContainerID><SearchCriteria>{}</SearchCriteria><Filter>*</Filter>\
<StartingIndex>{start}</StartingIndex><RequestedCount>{count}</RequestedCount><SortCriteria></SortCriteria>",
            xml_escape(criteria)
        ),
    )
}

/// Pages through a Browse or Search until everything (or `limit`) is read.
fn all_pages(mut call: impl FnMut(usize) -> Result<Page>, limit: usize) -> Result<Vec<Object>> {
    let mut out = Vec::new();
    loop {
        let page = call(out.len())?;
        let got = page.objects.len();
        out.extend(page.objects);
        if page.returned == 0 || got == 0 || out.len() >= limit || (page.total > 0 && out.len() >= page.total) {
            break;
        }
    }
    out.truncate(limit);
    Ok(out)
}

fn children(cfg: &ServerConfig, id: &str) -> Result<Vec<Object>> {
    all_pages(|start| browse(cfg, id, start, PAGE), 20_000)
}

// ---------------------------------------------------------------------------
// DIDL-Lite

/// A container (folder, album, …) or an item (song, …) on the server.
#[derive(Debug, Clone, Default, PartialEq)]
struct Object {
    id: String,
    container: bool,
    class: String,
    title: String,
    artist: String,
    album_artist: String,
    album: String,
    genre: String,
    track_no: u32,
    year: u32,
    art: Option<String>,
    /// The song file: address, duration, file type.
    url: Option<String>,
    duration_ms: u64,
    ext: Option<String>,
}

impl Object {
    fn is_audio(&self) -> bool {
        !self.container && (self.class.starts_with("object.item.audioItem") || (self.class.is_empty() && self.url.is_some()))
    }

    fn is_album(&self) -> bool {
        self.container && self.class.starts_with("object.container.album")
    }
}

/// Some old servers put a bare `&` in titles. Escapes those so the XML
/// parses; real entities are left alone.
fn fix_ampersands(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() + 16);
    let mut rest = xml;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let entity = after.find(';').filter(|&j| j > 0 && j <= 10).map(|j| &after[..j]).filter(|e| {
            e.chars().all(|c| c.is_ascii_alphanumeric() || c == '#')
        });
        out.push_str(if entity.is_some() { "&" } else { "&amp;" });
        rest = after;
    }
    out.push_str(rest);
    out
}

fn parse_didl(xml: &str) -> Result<Vec<Object>> {
    let fixed;
    let doc = match roxmltree::Document::parse(xml) {
        Ok(d) => d,
        Err(_) => {
            fixed = fix_ampersands(xml);
            roxmltree::Document::parse(&fixed).context("DLNA: the server sent a list Unitn can't read")?
        }
    };
    let root = doc.root_element();
    Ok(root
        .children()
        .filter(|n| n.has_tag_name("container") || n.has_tag_name("item"))
        .filter_map(|n| {
            let id = n.attribute("id")?.to_string();
            let mut o = Object { id, container: n.has_tag_name("container"), ..Default::default() };
            let mut creator = String::new();
            for c in n.children().filter(|c| c.is_element()) {
                let text = c.text().unwrap_or_default().trim();
                match c.tag_name().name() {
                    "title" => o.title = text.to_string(),
                    "class" => o.class = text.to_string(),
                    "creator" => creator = text.to_string(),
                    "artist" => match c.attribute("role").map(str::to_ascii_lowercase).as_deref() {
                        Some("albumartist") => o.album_artist = text.to_string(),
                        Some("performer") | None if o.artist.is_empty() => o.artist = text.to_string(),
                        _ => {}
                    },
                    "album" => o.album = text.to_string(),
                    "genre" if o.genre.is_empty() => o.genre = text.to_string(),
                    "originalTrackNumber" => o.track_no = text.parse().unwrap_or(0),
                    "date" => o.year = text.get(..4).and_then(|y| y.parse().ok()).unwrap_or(0),
                    "albumArtURI" if o.art.is_none() && text.starts_with("http") => o.art = Some(text.to_string()),
                    "res" => {
                        let info = c.attribute("protocolInfo").unwrap_or_default();
                        // The original file, not a converted copy, when a
                        // server offers both.
                        let converted = info.contains("DLNA.ORG_CI=1");
                        let audio = info.split(':').nth(2).map(|m| m.starts_with("audio/")).unwrap_or(true);
                        if text.starts_with("http") && audio && (o.url.is_none() || (!converted && o.ext.is_none())) {
                            o.url = Some(text.to_string());
                            o.ext = ext_of(info, text);
                            o.duration_ms = c.attribute("duration").map(parse_duration).unwrap_or(o.duration_ms);
                        }
                    }
                    _ => {}
                }
            }
            if o.artist.is_empty() {
                o.artist = creator;
            }
            Some(o)
        })
        .collect())
}

/// `H:MM:SS(.fff)` → milliseconds.
fn parse_duration(text: &str) -> u64 {
    let mut ms = 0f64;
    for part in text.trim().split(':') {
        ms = ms * 60.0 + part.parse::<f64>().unwrap_or(0.0);
    }
    (ms * 1000.0).round() as u64
}

/// The file type, from the MIME type in `protocolInfo` or the address.
fn ext_of(protocol_info: &str, url: &str) -> Option<String> {
    let mime = protocol_info.split(':').nth(2).unwrap_or_default().to_ascii_lowercase();
    let from_mime = match mime.split(';').next().unwrap_or_default() {
        "audio/mpeg" | "audio/mp3" | "audio/x-mpeg" => Some("mp3"),
        "audio/flac" | "audio/x-flac" => Some("flac"),
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" | "audio/x-mp4" => Some("m4a"),
        "audio/aac" | "audio/x-aac" | "audio/aacp" => Some("aac"),
        "audio/ogg" | "audio/x-ogg" | "application/ogg" | "audio/vorbis" => Some("ogg"),
        "audio/wav" | "audio/x-wav" | "audio/wave" => Some("wav"),
        "audio/aiff" | "audio/x-aiff" => Some("aiff"),
        "audio/x-matroska" => Some("mka"),
        _ => None,
    };
    from_mime.map(str::to_string).or_else(|| {
        let path = url.split(['?', '#']).next().unwrap_or_default();
        let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
        crate::library::scanner::AUDIO_EXTENSIONS.contains(&ext.as_str()).then_some(ext)
    })
}

// ---------------------------------------------------------------------------
// Albums and songs

fn album_from_container(o: &Object) -> ServerAlbum {
    ServerAlbum {
        id: o.id.clone(),
        title: non_empty(&o.title, "Unknown Album"),
        artist: non_empty(if o.album_artist.is_empty() { &o.artist } else { &o.album_artist }, "Unknown Artist"),
        year: o.year,
        image_url: o.art.clone(),
    }
}

/// A folder holding songs, as an album (named after its songs' tags).
fn album_from_folder(folder: &Object, songs: &[&Object]) -> ServerAlbum {
    let first = songs.first();
    let tag = |f: fn(&Object) -> &str| first.map(|s| f(s).to_string()).unwrap_or_default();
    let album = tag(|s| &s.album);
    let artist = {
        let aa = tag(|s| &s.album_artist);
        if !aa.is_empty() {
            aa
        } else if songs.iter().all(|s| Some(&s.artist) == first.map(|f| &f.artist)) {
            tag(|s| &s.artist)
        } else {
            "Various Artists".into()
        }
    };
    ServerAlbum {
        id: folder.id.clone(),
        title: non_empty(if album.is_empty() { &folder.title } else { &album }, "Unknown Album"),
        artist: non_empty(&artist, "Unknown Artist"),
        year: songs.iter().map(|s| s.year).max().unwrap_or(0),
        image_url: folder.art.clone().or_else(|| songs.iter().find_map(|s| s.art.clone())),
    }
}

fn non_empty(s: &str, fallback: &str) -> String {
    if s.trim().is_empty() { fallback.to_string() } else { s.trim().to_string() }
}

fn title_is(o: &Object, names: &[&str]) -> bool {
    let t = o.title.trim().to_lowercase();
    names.iter().any(|n| t == *n)
}

/// Views that list every song again (not albums), skipped while walking.
const SKIP_VIEWS: &[&str] = &[
    "all tracks", "all music", "all songs", "all audio", "tracks", "songs", "playlists", "recently added",
    "recently played", "most played", "last played", "rating", "my rating", "genre", "genres", "year",
    "years", "date", "decade", "composer", "conductor", "radio", "internet radio", "podcasts",
];

pub fn albums(cfg: &ServerConfig) -> Result<Vec<ServerAlbum>> {
    // 1. Ask the server for its albums.
    if let Ok(found) = all_pages(
        |start| search_page(cfg, "upnp:class derivedfrom \"object.container.album.musicAlbum\"", start, PAGE),
        50_000,
    ) {
        let list: Vec<ServerAlbum> = found.iter().filter(|o| o.is_album()).map(album_from_container).collect();
        if !list.is_empty() {
            return Ok(dedupe(list));
        }
    }
    // 2./3. Start at Music → Album (or the folders) and walk from there.
    let start = start_container(cfg)?;
    crawl(cfg, start)
}

/// The best place to look for albums: the Music view's Album list if there
/// is one, else its folder view, else the Music view, else the top.
fn start_container(cfg: &ServerConfig) -> Result<Object> {
    let top = Object { id: "0".into(), container: true, title: "Music".into(), ..Default::default() };
    let root = children(cfg, "0")?;
    let music = root
        .iter()
        .find(|o| o.container && title_is(o, &["music", "audio", "musik", "musique", "música", "musica", "zene", "muziek"]))
        .cloned();
    let Some(music) = music else { return Ok(top) };
    let views = children(cfg, &music.id)?;
    let pick = |names: &[&str]| views.iter().find(|o| o.container && title_is(o, names)).cloned();
    Ok(pick(&["album", "albums", "all albums", "albumok"])
        .or_else(|| pick(&["folders", "folder", "by folder", "browse folders", "music folders", "mappák"]))
        .unwrap_or(music))
}

fn crawl(cfg: &ServerConfig, start: Object) -> Result<Vec<ServerAlbum>> {
    let mut albums = Vec::new();
    let mut queue = VecDeque::from([(start, 0usize)]);
    let mut visited = HashSet::new();
    let mut calls = 0usize;
    while let Some((folder, depth)) = queue.pop_front() {
        if calls >= CRAWL_BUDGET || !visited.insert(folder.id.clone()) {
            continue;
        }
        calls += 1;
        let list = match children(cfg, &folder.id) {
            Ok(list) => list,
            Err(e) if calls == 1 => return Err(e),
            Err(_) => continue,
        };
        let songs: Vec<&Object> = list.iter().filter(|o| o.is_audio()).collect();
        if !songs.is_empty() {
            albums.push(album_from_folder(&folder, &songs));
        }
        for c in list.iter().filter(|o| o.container) {
            if c.is_album() {
                albums.push(album_from_container(c));
            } else if depth < MAX_DEPTH && !title_is(c, SKIP_VIEWS) {
                queue.push_back((c.clone(), depth + 1));
            }
        }
    }
    Ok(dedupe(albums))
}

/// The same album can show up in several views: keep one.
fn dedupe(list: Vec<ServerAlbum>) -> Vec<ServerAlbum> {
    let mut seen = HashSet::new();
    list.into_iter()
        .filter(|a| seen.insert((a.id.clone(), String::new())) && seen.insert((a.title.to_lowercase(), a.artist.to_lowercase())))
        .collect()
}

fn track_of(cfg: &ServerConfig, o: &Object, album: Option<&ServerAlbum>) -> Option<Track> {
    let url = o.url.clone()?;
    let album_title = match album {
        Some(a) if o.album.is_empty() => a.title.clone(),
        _ => non_empty(&o.album, "Unknown Album"),
    };
    let artist = non_empty(&o.artist, album.map(|a| a.artist.as_str()).unwrap_or("Unknown Artist"));
    let album_artist = if !o.album_artist.is_empty() {
        o.album_artist.clone()
    } else {
        album.map(|a| a.artist.clone()).unwrap_or_else(|| artist.clone())
    };
    Some(Track {
        path: track_key(cfg, &o.id),
        title: non_empty(&o.title, "Unknown"),
        artist,
        album: album_title,
        album_artist,
        track_no: o.track_no,
        year: o.year.max(album.map(|a| a.year).unwrap_or(0)),
        genre: o.genre.clone(),
        duration_ms: o.duration_ms,
        stream_url: Some(url),
        stream_ext: o.ext.clone(),
        ..Default::default()
    })
}

pub fn album_tracks(cfg: &ServerConfig, album: &ServerAlbum) -> Result<Vec<Track>> {
    let list = children(cfg, &album.id)?;
    Ok(list.iter().filter(|o| o.is_audio()).filter_map(|o| track_of(cfg, o, Some(album))).collect())
}

pub fn search(cfg: &ServerConfig, query: &str) -> Result<Vec<Track>> {
    let q = query.replace('\\', "\\\\").replace('"', "\\\"");
    let criteria = format!(
        "upnp:class derivedfrom \"object.item.audioItem\" and (dc:title contains \"{q}\" or upnp:artist contains \"{q}\" or upnp:album contains \"{q}\")"
    );
    // Servers without Search simply find nothing.
    let found = all_pages(|start| search_page(cfg, &criteria, start, 50), 50).unwrap_or_default();
    Ok(found.iter().filter(|o| o.is_audio()).filter_map(|o| track_of(cfg, o, None)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ServerConfig {
        ServerConfig {
            kind: ServerKind::Dlna,
            name: "MyBookWorld: Twonky".into(),
            url: "http://192.168.1.20:9000".into(),
            user: String::new(),
            token: String::new(),
            user_id: "uuid:1".into(),
            device_id: "http://192.168.1.20:9000/TMSDeviceDescription.xml".into(),
            control_url: "http://192.168.1.20:9000/TMSContentDirectory/Control".into(),
        }
    }

    #[test]
    fn reads_ssdp_answers() {
        let a = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nLocation: http://192.168.1.20:9000/TMSDeviceDescription.xml\r\nST: urn:schemas-upnp-org:device:MediaServer:1\r\n\r\n";
        assert_eq!(location_of(a).as_deref(), Some("http://192.168.1.20:9000/TMSDeviceDescription.xml"));
        assert_eq!(location_of("NOTIFY * HTTP/1.1\r\nLOCATION: http://x/\r\n"), None);
    }

    #[test]
    fn resolves_addresses() {
        assert_eq!(resolve("http://h:9000/desc/device.xml", "/ctl"), "http://h:9000/ctl");
        assert_eq!(resolve("http://h:9000/desc/device.xml", "ctl/cd"), "http://h:9000/desc/ctl/cd");
        assert_eq!(resolve("http://h:9000", "ctl"), "http://h:9000/ctl");
        assert_eq!(resolve("http://h:9000/x.xml", "http://o:1/c"), "http://o:1/c");
    }

    #[test]
    fn parses_descriptions() {
        let xml = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0"><specVersion><major>1</major></specVersion>
<device><deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>
<friendlyName>MyBookWorld: Twonky</friendlyName><UDN>uuid:55076f6e-6b79-4d65-6400-00904c3a3b2c</UDN>
<serviceList>
<service><serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType><controlURL>/cm</controlURL></service>
<service><serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>
<controlURL>/TMSContentDirectory/Control</controlURL></service></serviceList></device></root>"#;
        let d = parse_description("http://192.168.1.20:9000/TMSDeviceDescription.xml", xml).unwrap();
        assert_eq!(d.name, "MyBookWorld: Twonky");
        assert_eq!(d.control_url, "http://192.168.1.20:9000/TMSContentDirectory/Control");

        let router = r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><device><friendlyName>Router</friendlyName>
<serviceList><service><serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1</serviceType><controlURL>/l3</controlURL></service></serviceList></device></root>"#;
        assert!(parse_description("http://r/", router).is_err());
    }

    const BROWSE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>
<u:BrowseResponse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">
<Result>&lt;DIDL-Lite xmlns:dc=&quot;http://purl.org/dc/elements/1.1/&quot; xmlns:upnp=&quot;urn:schemas-upnp-org:metadata-1-0/upnp/&quot; xmlns=&quot;urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/&quot;&gt;
&lt;container id=&quot;0$1$12&quot; parentID=&quot;0$1&quot; childCount=&quot;9&quot;&gt;&lt;dc:title&gt;Ghosts I-IV&lt;/dc:title&gt;&lt;upnp:class&gt;object.container.album.musicAlbum&lt;/upnp:class&gt;&lt;upnp:artist&gt;Nine Inch Nails&lt;/upnp:artist&gt;&lt;upnp:albumArtURI&gt;http://192.168.1.20:9000/art/12.jpg&lt;/upnp:albumArtURI&gt;&lt;/container&gt;
&lt;item id=&quot;0$1$12$3&quot; parentID=&quot;0$1$12&quot;&gt;&lt;dc:title&gt;3 Ghosts I&lt;/dc:title&gt;&lt;dc:creator&gt;Nine Inch Nails&lt;/dc:creator&gt;&lt;upnp:album&gt;Ghosts I-IV&lt;/upnp:album&gt;&lt;upnp:originalTrackNumber&gt;3&lt;/upnp:originalTrackNumber&gt;&lt;dc:date&gt;2008-03-02&lt;/dc:date&gt;&lt;upnp:class&gt;object.item.audioItem.musicTrack&lt;/upnp:class&gt;
&lt;res protocolInfo=&quot;http-get:*:audio/x-flac:*&quot; duration=&quot;0:03:46.120&quot;&gt;http://192.168.1.20:9000/disk/DLNA-PNFLAC/O0$1$12$3.flac&lt;/res&gt;
&lt;res protocolInfo=&quot;http-get:*:audio/mpeg:DLNA.ORG_PN=MP3;DLNA.ORG_CI=1&quot;&gt;http://192.168.1.20:9000/disk/trans/O0$1$12$3.mp3&lt;/res&gt;&lt;/item&gt;
&lt;item id=&quot;v1&quot;&gt;&lt;dc:title&gt;A video&lt;/dc:title&gt;&lt;upnp:class&gt;object.item.videoItem&lt;/upnp:class&gt;&lt;res protocolInfo=&quot;http-get:*:video/mp4:*&quot;&gt;http://x/v.mp4&lt;/res&gt;&lt;/item&gt;
&lt;/DIDL-Lite&gt;</Result>
<NumberReturned>3</NumberReturned><TotalMatches>3</TotalMatches><UpdateID>7</UpdateID></u:BrowseResponse></s:Body></s:Envelope>"#;

    #[test]
    fn parses_browse_results() {
        let page = parse_response(BROWSE).unwrap();
        assert_eq!((page.returned, page.total), (3, 3));
        let album = &page.objects[0];
        assert!(album.is_album());
        let a = album_from_container(album);
        assert_eq!((a.title.as_str(), a.artist.as_str()), ("Ghosts I-IV", "Nine Inch Nails"));
        assert_eq!(a.image_url.as_deref(), Some("http://192.168.1.20:9000/art/12.jpg"));

        let song = &page.objects[1];
        assert!(song.is_audio());
        assert!(!page.objects[2].is_audio());
        let t = track_of(&cfg(), song, Some(&a)).unwrap();
        assert_eq!(t.title, "3 Ghosts I");
        assert_eq!(t.track_no, 3);
        assert_eq!(t.year, 2008);
        assert_eq!(t.duration_ms, 226_120);
        // The original FLAC, not the converted MP3.
        assert_eq!(t.stream_ext.as_deref(), Some("flac"));
        assert!(t.stream_url.as_deref().unwrap().ends_with(".flac"));
        assert_eq!(t.path, std::path::PathBuf::from("dlna://192.168.1.20:9000/0$1$12$3"));
    }

    #[test]
    fn folders_become_albums() {
        let folder = Object { id: "f1".into(), container: true, title: "Kind of Blue".into(), ..Default::default() };
        let s1 = Object { artist: "Miles Davis".into(), year: 1959, ..Default::default() };
        let s2 = Object { artist: "Miles Davis".into(), ..Default::default() };
        let a = album_from_folder(&folder, &[&s1, &s2]);
        assert_eq!((a.title.as_str(), a.artist.as_str(), a.year), ("Kind of Blue", "Miles Davis", 1959));
        let s3 = Object { artist: "Someone".into(), ..Default::default() };
        assert_eq!(album_from_folder(&folder, &[&s1, &s3]).artist, "Various Artists");
    }

    #[test]
    fn tolerates_bare_ampersands() {
        let didl = r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/"><item id="1"><dc:title>Simon & Garfunkel &amp; more</dc:title></item></DIDL-Lite>"#;
        assert_eq!(parse_didl(didl).unwrap()[0].title, "Simon & Garfunkel & more");
    }

    #[test]
    fn small_helpers() {
        assert_eq!(parse_duration("1:02:03.5"), 3_723_500);
        assert_eq!(parse_duration("0:00:07"), 7_000);
        assert_eq!(ext_of("http-get:*:audio/mpeg:*", "http://x/a"), Some("mp3".into()));
        assert_eq!(ext_of("http-get:*:*:*", "http://x/a.OGG?x=1"), Some("ogg".into()));
        assert_eq!(ext_of("", "http://x/stream"), None);
        assert_eq!(origin_of("http://h:9000/a/b.xml"), "http://h:9000");
    }
}
