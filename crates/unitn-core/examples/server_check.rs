//! Signs in to a Jellyfin or Plex server, lists albums and decodes a song.
//!
//!     cargo run -p unitn-core --example server_check -- jellyfin http://localhost:8096 USER PASSWORD
//!     cargo run -p unitn-core --example server_check -- plex http://localhost:32400 "" TOKEN

use unitn_core::audio::check_source;
use unitn_core::servers::{self, ServerKind};

fn run(kind: ServerKind, url: &str, user: &str, secret: &str) -> anyhow::Result<String> {
    let cfg = servers::connect(kind, url, user, secret)?;
    println!("connected to \"{}\" at {}", cfg.name, cfg.url);
    // Lets the screenshot run show this server (CI only).
    if let Ok(file) = std::env::var("UNITN_SAVE_SERVER") {
        std::fs::write(file, serde_json::to_string(&cfg)?)?;
    }
    let albums = servers::albums(&cfg)?;
    println!("{} albums", albums.len());
    for a in albums.iter().take(10) {
        println!("  {} — {} ({}) cover: {}", a.artist, a.title, a.year, a.image_url.is_some());
    }
    let album = albums.first().ok_or_else(|| anyhow::anyhow!("no albums"))?;
    if let Some(url) = &album.image_url {
        let data = servers::fetch_image(url)?;
        println!("cover: {} bytes", data.len());
    }
    let tracks = servers::album_tracks(&cfg, album)?;
    println!("{} tracks on \"{}\"", tracks.len(), album.title);
    for t in &tracks {
        println!("  {}.{} {} — {} ({} ms, {:?})", t.disc_no, t.track_no, t.artist, t.title, t.duration_ms, t.stream_ext);
    }
    let track = tracks.first().ok_or_else(|| anyhow::anyhow!("no tracks"))?;
    check_source(&track.source(), 3.0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("usage: server_check <jellyfin|plex> <url> <user> <password-or-token>");
        std::process::exit(2);
    }
    let kind = if args[0].eq_ignore_ascii_case("plex") { ServerKind::Plex } else { ServerKind::Jellyfin };
    match run(kind, &args[1], &args[2], &args[3]) {
        Ok(info) => println!("RESULT {}: OK ({info})", kind.label()),
        Err(e) => {
            println!("RESULT {}: FAILED: {e:#}", kind.label());
            std::process::exit(1);
        }
    }
}
