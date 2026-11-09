//! Checks that internet radio streams open and decode.
//!
//!     cargo run -p unitn-core --example radio_check -- [--top N] [URL...]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use unitn_core::audio::{check_source, Source};
use unitn_core::radio::RadioBrowser;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut targets: Vec<(String, String)> = Vec::new();
    if let Some(i) = args.iter().position(|a| a == "--top") {
        let n: u32 = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(20);
        args.drain(i..(i + 2).min(args.len()));
        match RadioBrowser::new().top(n) {
            Ok(list) => targets.extend(list.into_iter().map(|s| (s.name, s.url))),
            Err(e) => eprintln!("could not load top stations: {e}"),
        }
    }
    targets.extend(args.into_iter().map(|u| (u.clone(), u)));

    let (mut ok, mut failed, mut hung) = (0, 0, 0);
    for (name, url) in targets {
        let started = Instant::now();
        let (tx, rx) = mpsc::channel();
        let u = url.clone();
        std::thread::spawn(move || {
            let _ = tx.send(check_source(&Source::Url(u), 3.0));
        });
        match rx.recv_timeout(Duration::from_secs(25)) {
            Ok(Ok(info)) => {
                ok += 1;
                println!("OK    {:5.1}s  {name}  [{info}]", started.elapsed().as_secs_f64());
            }
            Ok(Err(e)) => {
                failed += 1;
                println!("ERROR {:5.1}s  {name}  {url}  -> {e:#}", started.elapsed().as_secs_f64());
            }
            Err(_) => {
                hung += 1;
                println!("HANG  >25s    {name}  {url}");
            }
        }
    }
    println!("\nSummary: {ok} ok, {failed} errors, {hung} hung");
    std::process::exit(if hung > 0 { 1 } else { 0 });
}
