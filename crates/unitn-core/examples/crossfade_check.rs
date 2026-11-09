//! Plays two files with crossfade on the real audio output and checks when
//! the second one starts (needs a sound device, e.g. PulseAudio's null sink).
//!
//!     cargo run -p unitn-core --example crossfade_check -- first.wav second.wav SECONDS

use std::time::{Duration, Instant};

use unitn_core::audio::{PlayState, Player, PlayerEvent, Source};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: crossfade_check <first> <second> <seconds>");
        std::process::exit(2);
    }
    let fade: f64 = args[2].parse().unwrap_or(3.0);
    let (player, events) = Player::new();
    player.set_volume(0.2);
    player.set_crossfade(fade);
    player.load(Source::File(args[0].clone().into()));
    let mut started = None;
    let mut first_len = None;
    let mut advanced = None;
    let mut ended = None;
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline && ended.is_none() {
        let Ok(e) = events.recv_timeout(Duration::from_millis(200)) else { continue };
        match e {
            PlayerEvent::State(PlayState::Playing) if started.is_none() => {
                started = Some(Instant::now());
                player.set_next(Some(Source::File(args[1].clone().into())));
            }
            PlayerEvent::Position { duration: Some(d), .. } if first_len.is_none() && started.is_some() => {
                first_len = Some(d);
            }
            PlayerEvent::Advanced => advanced = started.map(|s| s.elapsed().as_secs_f64()),
            PlayerEvent::Ended => ended = started.map(|s| s.elapsed().as_secs_f64()),
            PlayerEvent::Error(msg) => println!("error: {msg}"),
            _ => {}
        }
    }
    let first = first_len.unwrap_or(0.0);
    println!("first track {first:.1} s, crossfade {fade:.1} s");
    println!("second track started after {advanced:?} s, everything ended after {ended:?} s");
    match advanced {
        Some(at) if (at - (first - fade)).abs() <= 1.0 => {
            println!("RESULT crossfade: OK (next song started {at:.1} s in, expected {:.1} s)", first - fade)
        }
        other => {
            println!("RESULT crossfade: FAILED (next song started at {other:?}, expected {:.1} s)", first - fade);
            std::process::exit(1);
        }
    }
}
