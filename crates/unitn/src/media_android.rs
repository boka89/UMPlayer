//! Android stand-in for `media.rs`: no lock-screen or headset controls yet.
//! Those need a MediaSession and a foreground service (Kotlin/Java), which
//! is the next step after the first test build.

use std::ffi::c_void;
use std::sync::mpsc::Sender;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum MediaCmd {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    SeekTo(f64),
}

pub struct Media;

impl Media {
    pub fn disabled() -> Self {
        Self
    }

    pub fn new(_hwnd: Option<*mut c_void>, _tx: Sender<MediaCmd>) -> Self {
        Self
    }

    pub fn set_metadata(&mut self, _title: &str, _artist: &str, _album: &str, _duration: Option<f64>) {}

    pub fn set_state(&mut self, _playing: bool, _paused: bool, _position: f64) {}
}
