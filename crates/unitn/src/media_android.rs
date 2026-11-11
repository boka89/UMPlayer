//! Android counterpart of `media.rs`: the playback notification with its
//! buttons, the lock screen, headset and Bluetooth controls. The Java side
//! (`PlaybackService`) does the work; this keeps it up to date.

use std::ffi::c_void;
use std::sync::mpsc::Sender;
use std::time::Instant;

use crate::android::{self, NowPlaying};

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

pub struct Media {
    enabled: bool,
    now: NowPlaying,
    /// Something is playing or paused (the notification is up).
    active: bool,
    /// When the Java side was last told, and with what.
    sent: Option<(Instant, NowPlaying)>,
}

impl Media {
    /// No OS integration (until [`Media::new`]).
    pub fn disabled() -> Self {
        Self { enabled: false, now: NowPlaying::default(), active: false, sent: None }
    }

    pub fn new(_hwnd: Option<*mut c_void>, tx: Sender<MediaCmd>) -> Self {
        android::set_media_sender(tx);
        Self { enabled: true, ..Self::disabled() }
    }

    pub fn set_metadata(&mut self, title: &str, artist: &str, album: &str, duration: Option<f64>) {
        self.now.title = title.to_string();
        self.now.artist = artist.to_string();
        self.now.album = album.to_string();
        self.now.duration = duration.filter(|d| d.is_finite() && *d > 0.0);
        self.push();
    }

    pub fn set_state(&mut self, playing: bool, paused: bool, position: f64) {
        self.now.playing = playing;
        self.now.position = position.max(0.0);
        self.active = playing || paused;
        self.push();
    }

    /// Called as playback moves on; only a jump (a seek) is passed on, the
    /// system counts the seconds itself in between.
    pub fn set_position(&mut self, position: f64) {
        self.now.position = position.max(0.0);
        let Some((at, sent)) = &self.sent else { return };
        let expected = sent.position + if sent.playing { at.elapsed().as_secs_f64() } else { 0.0 };
        if (self.now.position - expected).abs() > 2.5 {
            self.push();
        }
    }

    fn push(&mut self) {
        if !self.enabled {
            return;
        }
        let Some(platform) = android::platform() else { return };
        if !self.active {
            if self.sent.take().is_some() {
                platform.playback(None);
            }
            return;
        }
        if let Some((at, sent)) = &self.sent {
            // Nothing new (the position alone moving on is not news).
            let mut same = sent.clone();
            same.position = self.now.position;
            let expected = sent.position + if sent.playing { at.elapsed().as_secs_f64() } else { 0.0 };
            if same == self.now && (self.now.position - expected).abs() <= 2.5 {
                return;
            }
        }
        platform.playback(Some(&self.now));
        self.sent = Some((Instant::now(), self.now.clone()));
    }
}
