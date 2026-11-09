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
    let mut started = false;
    let mut first_len = None;
    // Position of the first song (as the player reports it) when the second one started.
    let mut last_pos = 0.0;
    let mut advanced_at = None;
    let mut second_pos = 0.0;
    let mut ended = false;
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline && !ended {
        let Ok(e) = events.recv_timeout(Duration::from_millis(200)) else { continue };
        match e {
            PlayerEvent::State(PlayState::Playing) if !started => {
                started = true;
                player.set_next(Some(Source::File(args[1].clone().into())));
            }
            PlayerEvent::Position { position, duration } => {
                if advanced_at.is_none() {
                    last_pos = position;
                    if first_len.is_none() && started {
                        first_len = duration;
                    }
                } else {
                    second_pos = position;
                }
            }
            PlayerEvent::Advanced => advanced_at = Some(last_pos),
            PlayerEvent::Ended => ended = true,
            PlayerEvent::Error(msg) => println!("error: {msg}"),
            _ => {}
        }
    }
    let first = first_len.unwrap_or(0.0);
    let expected = first - fade;
    println!("first track {first:.1} s, crossfade {fade:.1} s");
    println!("second track started when the first was at {advanced_at:?} s; second played to {second_pos:.1} s");
    match advanced_at {
        // Position events come every 250 ms: allow a little slack.
        Some(at) if (at - expected).abs() <= 0.6 && ended && second_pos > 12.0 => {
            println!("RESULT crossfade: OK (next song started at {at:.1} s of the first, expected {expected:.1} s)")
        }
        other => {
            println!("RESULT crossfade: FAILED (next song started at {other:?}, expected {expected:.1} s, ended: {ended})");
            std::process::exit(1);
        }
    }
}
