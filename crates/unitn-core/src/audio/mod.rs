//! Audio engine.
//!
//! A dedicated thread owns the output device (cpal) and the decoder
//! (symphonia). Decoded audio is converted to the device's channel count and
//! sample rate and pushed into a small shared buffer the device callback reads
//! from. The UI talks to it with [`Player`] and receives [`PlayerEvent`]s.

mod adts;
mod convert;
mod decoder;
mod engine;
pub mod eq;
mod output;
mod prefetch;
mod spectrum;
pub(crate) mod stream;

pub use spectrum::Spectrum;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use output::Shared;

/// What to play.
#[derive(Clone, PartialEq)]
pub enum Source {
    /// A local file.
    File(PathBuf),
    /// A network stream (internet radio). Playlists (.m3u/.pls) are resolved.
    Url(String),
    /// A complete file on a server (e.g. Jellyfin, Plex): downloaded into
    /// memory in the background and played like a local file, with seeking.
    Remote { url: String, ext: Option<String> },
}

// Server URLs carry access tokens: keep them out of logs.
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::File(p) => write!(f, "File({p:?})"),
            Source::Url(u) => write!(f, "Url({u:?})"),
            Source::Remote { url, ext } => {
                let base = url.split('?').next().unwrap_or_default();
                write!(f, "Remote({base:?}, {ext:?})")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Stopped,
    Buffering,
    Playing,
    Paused,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    State(PlayState),
    /// Sent about four times a second while playing.
    Position { position: f64, duration: Option<f64> },
    /// The current source finished playing.
    Ended,
    /// Playback moved on, without a gap, to the source given with
    /// [`Player::set_next`].
    Advanced,
    /// "Now playing" title from an internet radio stream (ICY metadata).
    StreamTitle(String),
    Error(String),
}

#[derive(Debug)]
pub(crate) enum Command {
    Load(Source),
    Play,
    Pause,
    Stop,
    Seek(f64),
    Volume(f32),
    SetNext(Option<Source>),
}

/// Handle to the audio thread. Cheap to use from the UI thread; all calls
/// are non-blocking.
pub struct Player {
    tx: Sender<Command>,
    shared: Arc<Shared>,
}

impl Player {
    /// Starts the audio thread. Events arrive on the returned receiver.
    pub fn new() -> (Self, Receiver<PlayerEvent>) {
        let (tx, rx) = mpsc::channel();
        let (etx, erx) = mpsc::channel();
        let shared = Arc::new(Shared::new(0.8));
        let engine_shared = shared.clone();
        std::thread::Builder::new()
            .name("unitn-audio".into())
            .spawn(move || engine::run(rx, etx, engine_shared))
            .expect("failed to start audio thread");
        (Self { tx, shared }, erx)
    }

    /// Applies equalizer settings (takes effect immediately).
    pub fn set_equalizer(&self, settings: eq::EqSettings) {
        self.shared.set_eq(settings);
    }

    /// A spectrum analyzer fed with the audio that is actually being played.
    pub fn spectrum(&self, bars: usize) -> Spectrum {
        Spectrum::new(self.shared.clone(), bars)
    }

    fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }

    /// Loads a source and starts playing it immediately.
    pub fn load(&self, source: Source) {
        self.send(Command::Load(source));
    }
    pub fn play(&self) {
        self.send(Command::Play);
    }
    pub fn pause(&self) {
        self.send(Command::Pause);
    }
    pub fn stop(&self) {
        self.send(Command::Stop);
    }
    /// What to play right after the current file, for gapless playback. The
    /// engine prepares it in advance and sends [`PlayerEvent::Advanced`] when
    /// it starts. Cleared by every `load`/`stop`.
    pub fn set_next(&self, next: Option<Source>) {
        self.send(Command::SetNext(next));
    }

    /// Seeks to an absolute position in seconds (ignored for live streams).
    pub fn seek(&self, seconds: f64) {
        self.send(Command::Seek(seconds.max(0.0)));
    }
    /// Volume from 0.0 to 1.0 (applied on a perceptual curve).
    pub fn set_volume(&self, volume: f32) {
        self.send(Command::Volume(volume.clamp(0.0, 1.0)));
    }
}

/// Diagnostics: opens `source`, decodes about `seconds` of audio without
/// playing it and returns a short description (used by `examples/radio_check`).
pub fn check_source(source: &Source, seconds: f64) -> anyhow::Result<String> {
    let (tx, rx) = mpsc::channel();
    let mut decoder = decoder::Decoder::open(source, &tx)?;
    let mut decoded = 0.0;
    let mut format = String::new();
    while decoded < seconds {
        match decoder.next_chunk()? {
            Some(chunk) => {
                if format.is_empty() {
                    format = format!("{} ch, {} Hz", chunk.channels, chunk.rate);
                }
                decoded += chunk.samples.len() as f64 / chunk.channels as f64 / chunk.rate.max(1) as f64;
            }
            None => break,
        }
    }
    let title = rx.try_iter().find_map(|e| match e {
        PlayerEvent::StreamTitle(t) => Some(t),
        _ => None,
    });
    Ok(format!(
        "{format}, decoded {decoded:.1} s{}",
        title.map(|t| format!(", now playing: {t}")).unwrap_or_default()
    ))
}

/// A tiny mono 16-bit PCM WAV file (a sine tone) for tests.
#[cfg(test)]
pub(crate) fn test_wav(rate: u32, seconds: f32) -> Vec<u8> {
    let n = (rate as f32 * seconds) as u32;
    let data_len = n * 2;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        let s = (i as f32 / rate as f32 * 440.0 * std::f32::consts::TAU).sin();
        v.extend_from_slice(&((s * 12000.0) as i16).to_le_bytes());
    }
    v
}
