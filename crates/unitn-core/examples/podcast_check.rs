//! Searches the podcast directory, reads a feed and decodes the start of
//! the newest episode.
//!
//!     cargo run -p unitn-core --example podcast_check -- "search term"

use unitn_core::audio::{check_source, Source};
use unitn_core::podcasts;

fn run(term: &str) -> anyhow::Result<String> {
    let shows = podcasts::search(term)?;
    println!("{} shows for \"{term}\"", shows.len());
    for s in shows.iter().take(5) {
        println!("  {} — {} ({})", s.title, s.author, s.feed_url);
    }
    // Some feeds are down or need a browser: try a few.
    let mut last = anyhow::anyhow!("no shows found");
    for show in shows.iter().take(5) {
        let feed = match podcasts::fetch(&show.feed_url) {
            Ok(f) => f,
            Err(e) => {
                println!("  feed failed: {e:#}");
                last = e;
                continue;
            }
        };
        println!("feed \"{}\": {} episodes, image: {}", feed.show.title, feed.episodes.len(), feed.show.image_url.is_some());
        let Some(ep) = feed.episodes.first() else { continue };
        println!(
            "newest: \"{}\" ({}, {} s, {:?})",
            ep.title,
            podcasts::format_date(ep.published),
            ep.duration,
            ep.extension()
        );
        match check_source(&Source::Remote { url: ep.audio_url.clone(), ext: ep.extension() }, 3.0) {
            Ok(info) => return Ok(format!("\"{}\": {info}", feed.show.title)),
            Err(e) => {
                println!("  episode failed: {e:#}");
                last = e;
            }
        }
    }
    Err(last)
}

fn main() {
    let term = std::env::args().nth(1).unwrap_or_else(|| "NASA".into());
    match run(&term) {
        Ok(info) => println!("RESULT podcasts: OK ({info})"),
        Err(e) => {
            println!("RESULT podcasts: FAILED: {e:#}");
            std::process::exit(1);
        }
    }
}
