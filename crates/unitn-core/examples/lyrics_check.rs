//! Looks up lyrics on LRCLIB and checks that synced lines come back.
//!
//!     cargo run -p unitn-core --example lyrics_check -- "Artist" "Title" [album] [seconds]

use unitn_core::lyrics;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: lyrics_check <artist> <title> [album] [seconds]");
        std::process::exit(2);
    }
    let album = args.get(2).cloned().unwrap_or_default();
    let secs = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    match lyrics::lrclib(&args[0], &args[1], &album, secs) {
        Ok(Some(l)) if !l.lines.is_empty() => {
            println!("{} lines, synced: {}, source: {}", l.lines.len(), l.synced, l.source);
            for line in l.lines.iter().take(5) {
                println!("  {:>7} {}", line.time.map(|t| format!("{t:.2}")).unwrap_or_default(), line.text);
            }
            let mid = l.lines.get(l.lines.len() / 2).and_then(|x| x.time).unwrap_or(0.0);
            println!("line at {mid:.1}s: {:?}", l.current_line(mid + 0.01));
            println!("RESULT lyrics: OK ({} lines, synced {})", l.lines.len(), l.synced);
        }
        Ok(_) => {
            println!("RESULT lyrics: FAILED: no lyrics found");
            std::process::exit(1);
        }
        Err(e) => {
            println!("RESULT lyrics: FAILED: {e:#}");
            std::process::exit(1);
        }
    }
}
