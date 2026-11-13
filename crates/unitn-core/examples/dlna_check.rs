//! Tries the DLNA support against real servers on the network:
//! `cargo run -p unitn-core --example dlna_check [address]`
//! Without an address, servers are discovered and the first one is used.
use std::time::Duration;

use unitn_core::servers::{self, ServerKind};

fn main() -> anyhow::Result<()> {
    let found = servers::dlna_discover(Duration::from_secs(3));
    for s in &found {
        println!("found: {} at {}", s.name, s.location);
    }
    let target = std::env::args()
        .nth(1)
        .or_else(|| found.first().map(|s| s.location.clone()));
    let Some(target) = target else {
        println!("no DLNA server found");
        return Ok(());
    };
    let cfg = servers::connect(ServerKind::Dlna, &target, "", "")?;
    println!(
        "connected: {} ({}), control {}",
        cfg.name, cfg.url, cfg.control_url
    );
    let albums = servers::albums(&cfg)?;
    println!("{} albums", albums.len());
    for a in &albums {
        println!(
            "  {} — {} ({}) art={}",
            a.artist,
            a.title,
            a.year,
            a.image_url.is_some()
        );
        let tracks = servers::album_tracks(&cfg, a)?;
        if let Some(t) = tracks.first() {
            println!(
                "    decode: {}",
                unitn_core::audio::check_source(&t.source(), 3.0)?
            );
        }
        for t in tracks {
            println!(
                "    {:>2}. {} [{} ms, {:?}] {}",
                t.track_no,
                t.title,
                t.duration_ms,
                t.stream_ext,
                t.stream_url.unwrap_or_default()
            );
        }
    }
    for q in ["ghost", "so"] {
        let hits = servers::search_tracks(&cfg, q)?;
        println!(
            "search {q:?}: {:?}",
            hits.iter().map(|t| &t.title).collect::<Vec<_>>()
        );
    }
    Ok(())
}
