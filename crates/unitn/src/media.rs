//! OS media integration: media keys, macOS "Now Playing", Windows media
//! overlay (SMTC) and Linux MPRIS, via the `souvlaki` crate.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::Sender;
use std::time::Duration;

use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};
use unitn_core::log::log;

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
    controls: Option<MediaControls>,
}

impl Media {
    /// No OS integration (until [`Media::new`] succeeds).
    pub fn disabled() -> Self {
        Self { controls: None }
    }

    /// `hwnd` is required on Windows (the main window handle), ignored elsewhere.
    pub fn new(hwnd: Option<*mut c_void>, tx: Sender<MediaCmd>) -> Self {
        if cfg!(target_os = "windows") && hwnd.is_none() {
            log("media controls unavailable (no window handle)");
            return Self { controls: None };
        }
        let config = PlatformConfig { dbus_name: "unitn", display_name: "Unitn", hwnd };
        // Never let the OS integration take the app down.
        let created = catch_unwind(AssertUnwindSafe(|| MediaControls::new(config)));
        let Ok(created) = created else {
            log("media controls crashed while starting; disabled");
            return Self { controls: None };
        };
        let controls = match created {
            Ok(mut c) => {
                let attached = c.attach(move |event: MediaControlEvent| {
                    let cmd = match event {
                        MediaControlEvent::Play => MediaCmd::Play,
                        MediaControlEvent::Pause => MediaCmd::Pause,
                        MediaControlEvent::Toggle => MediaCmd::Toggle,
                        MediaControlEvent::Next => MediaCmd::Next,
                        MediaControlEvent::Previous => MediaCmd::Previous,
                        MediaControlEvent::Stop => MediaCmd::Stop,
                        MediaControlEvent::SetPosition(MediaPosition(p)) => MediaCmd::SeekTo(p.as_secs_f64()),
                        _ => return,
                    };
                    let _ = tx.send(cmd);
                });
                if attached.is_ok() {
                    log("media controls attached");
                    Some(c)
                } else {
                    log("media controls unavailable (attach failed)");
                    None
                }
            }
            Err(e) => {
                log(format!("media controls unavailable: {e:?}"));
                None
            }
        };
        Self { controls }
    }

    /// Runs `f` on the controls; if the OS integration misbehaves it is
    /// switched off instead of breaking playback.
    fn with(&mut self, what: &str, f: impl FnOnce(&mut MediaControls)) {
        let Some(c) = self.controls.as_mut() else { return };
        if catch_unwind(AssertUnwindSafe(|| f(c))).is_err() {
            log(format!("media controls crashed in {what}; disabling them"));
            self.controls = None;
        }
    }

    pub fn set_metadata(&mut self, title: &str, artist: &str, album: &str, duration: Option<f64>) {
        self.with("set_metadata", |c| {
            if let Err(e) = c.set_metadata(MediaMetadata {
                title: Some(title),
                artist: (!artist.is_empty()).then_some(artist),
                album: (!album.is_empty()).then_some(album),
                cover_url: None,
                duration: duration.filter(|d| d.is_finite() && *d > 0.0).map(Duration::from_secs_f64),
            }) {
                log(format!("media controls: set_metadata failed: {e:?}"));
            }
        });
    }

    pub fn set_state(&mut self, playing: bool, paused: bool, position: f64) {
        self.with("set_playback", |c| {
            let progress = Some(MediaPosition(Duration::from_secs_f64(position.max(0.0))));
            let state = if playing {
                MediaPlayback::Playing { progress }
            } else if paused {
                MediaPlayback::Paused { progress }
            } else {
                MediaPlayback::Stopped
            };
            if let Err(e) = c.set_playback(state) {
                log(format!("media controls: set_playback failed: {e:?}"));
            }
        });
    }
}
