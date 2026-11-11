//! The startup animation: the Unitn logo sting (rendered with HyperFrames,
//! see tools/make_intro.py) plays in its own fixed-size splash window, holds
//! the logo for a moment, then the main window takes over. A click or a key
//! skips it. Its sound plays on a separate, short-lived player (setting
//! "Startup sound").

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Image, Rgb8Pixel, SharedPixelBuffer};
use unitn_core::audio::{Player, Source};
use unitn_core::log::log;

use crate::SplashWindow;

static FRAMES: &[u8] = include_bytes!("../assets/intro/frames.bin");
static SOUND: &[u8] = include_bytes!("../assets/intro/sound.m4a");
const FPS: f64 = 30.0;
/// How long the finished logo stays before the fade.
const HOLD: Duration = Duration::from_millis(900);
/// Must match `animate opacity` in SplashWindow (app.slint).
const FADE: Duration = Duration::from_millis(350);

/// Splits frames.bin into the JPEG frames.
fn frames() -> Vec<&'static [u8]> {
    let word = |i: usize| -> Option<usize> {
        FRAMES.get(i * 4..i * 4 + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let Some(count) = word(0) else { return Vec::new() };
    let mut offset = 4 * (count + 1);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let Some(len) = word(i + 1) else { break };
        let Some(frame) = FRAMES.get(offset..offset + len) else { break };
        out.push(frame);
        offset += len;
    }
    out
}

fn decode(jpeg: &[u8]) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
    let img = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?.to_rgb8();
    Some(SharedPixelBuffer::clone_from_slice(img.as_raw(), img.width(), img.height()))
}

/// Starts the sound on its own player; dropping the player stops it.
fn start_sound(volume: f32) -> Option<Player> {
    let path = std::env::temp_dir().join("unitn-intro.m4a");
    if let Err(e) = std::fs::write(&path, SOUND) {
        log(format!("intro: cannot write the sound: {e}"));
        return None;
    }
    let (player, _events) = Player::new();
    player.set_volume(volume);
    player.load(Source::File(path));
    Some(player)
}

/// Lets the app skip the animation (click or key).
#[derive(Clone)]
pub struct Intro {
    skip: Arc<AtomicBool>,
}

impl Intro {
    pub fn skip(&self) {
        self.skip.store(true, Ordering::Relaxed);
    }
}

/// Shows the splash window and plays the animation; `sound` is the volume
/// for the startup sound, or `None` for silence. `done` runs on the UI thread
/// when the animation is over (or skipped), after which the splash closes.
pub fn start(splash: &SplashWindow, sound: Option<f32>, done: impl FnOnce() + 'static) -> Intro {
    let intro = Intro { skip: Arc::new(AtomicBool::new(false)) };
    let frames = frames();
    let done = std::rc::Rc::new(std::cell::RefCell::new(Some(Box::new(done) as Box<dyn FnOnce()>)));
    let finish = {
        let done = done.clone();
        let weak = splash.as_weak();
        move || {
            if let Some(done) = done.borrow_mut().take() {
                done();
            }
            if let Some(splash) = weak.upgrade() {
                let _ = splash.hide();
            }
        }
    };
    if frames.is_empty() || splash.show().is_err() {
        finish();
        return intro;
    }
    center(splash);
    {
        let skip = intro.skip.clone();
        splash.on_skip(move || skip.store(true, Ordering::Relaxed));
    }
    // The thread can't hold `finish` (not Send): it asks the UI thread to run it.
    let finish = std::rc::Rc::new(std::cell::RefCell::new(Some(finish)));
    let finish_slot = finish.clone();
    let weak = splash.as_weak();
    splash.on_finished(move || {
        if let Some(f) = finish_slot.borrow_mut().take() {
            f();
        }
    });
    let skip = intro.skip.clone();
    let spawned = std::thread::Builder::new().name("unitn-intro".into()).spawn(move || {
        let player = sound.and_then(start_sound);
        let started = Instant::now();
        let count = frames.len();
        let mut shown = 0;
        for (i, jpeg) in frames.into_iter().enumerate() {
            if skip.load(Ordering::Relaxed) {
                break;
            }
            let due = started + Duration::from_secs_f64(i as f64 / FPS);
            // Running late (slow machine): drop frames, never the timing.
            if i + 1 < count && Instant::now() > due + Duration::from_secs_f64(1.0 / FPS) {
                continue;
            }
            let Some(buffer) = decode(jpeg) else { continue };
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            shown += 1;
            let _ = weak.upgrade_in_event_loop(move |s| s.set_frame(Image::from_rgb8(buffer)));
        }
        let skipped = skip.load(Ordering::Relaxed);
        if !skipped {
            let until = Instant::now() + HOLD;
            while Instant::now() < until && !skip.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        log(format!("intro: {shown} of {count} frames shown{}", if skipped { ", skipped" } else { "" }));
        let _ = weak.upgrade_in_event_loop(|s| s.set_fading(true));
        std::thread::sleep(FADE);
        let _ = weak.upgrade_in_event_loop(|s| s.invoke_finished());
        if let Some(player) = player {
            if skip.load(Ordering::Relaxed) {
                player.stop();
            } else {
                // The sound ends before the animation; leave a little margin.
                std::thread::sleep(Duration::from_millis(300));
            }
            drop(player);
        }
    });
    if let Err(e) = spawned {
        log(format!("intro: cannot start: {e}"));
        if let Some(f) = finish.borrow_mut().take() {
            f();
        }
    }
    intro
}

/// Puts the splash in the middle of the screen it opened on.
fn center(splash: &SplashWindow) {
    use slint::winit_030::WinitWindowAccessor;
    splash.window().with_winit_window(|w| {
        let Some(monitor) = w.current_monitor().or_else(|| w.primary_monitor()) else { return };
        let (area, size) = (monitor.size(), w.outer_size());
        let pos = monitor.position();
        let x = pos.x + (area.width as i32 - size.width as i32) / 2;
        let y = pos.y + (area.height as i32 - size.height as i32) / 2;
        w.set_outer_position(slint::winit_030::winit::dpi::PhysicalPosition::new(x, y));
    });
}
