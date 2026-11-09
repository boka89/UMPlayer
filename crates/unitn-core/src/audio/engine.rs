//! The audio thread.
//!
//! Design rule: nothing slow ever runs on this thread. Opening a source
//! (network, slow disks) happens on a worker thread, and live streams are
//! also *decoded* on their own thread, so a stalled station can never block
//! commands like "play this file" or "stop".

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use super::convert::{map_channels, Resampler};
use super::decoder::{Chunk, Decoder};
use super::output::{Output, Shared};
use super::{Command, PlayState, PlayerEvent, Source};
use crate::log::log;

/// Give up on a source that has not started within this time.
const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// Decoded chunks a live stream may queue up ahead of the engine.
const LIVE_QUEUE: usize = 64;

/// Where decoded audio comes from.
enum Feed {
    /// Local file: decoded on the engine thread (fast, seekable).
    File(Decoder),
    /// Live stream: decoded on its own thread.
    Live(Receiver<Result<Option<Chunk>>>),
}

/// Result of a background open.
struct Opened {
    generation: u64,
    /// `Some` when this is the prepared next track (gapless).
    next_gen: Option<u64>,
    result: Result<(Feed, Option<f64>)>,
}

/// Position bookkeeping of a track whose last samples are still playing
/// after a gapless switch.
struct Clock {
    start_frame: u64,
    base: f64,
    duration: Option<f64>,
}

struct Current {
    feed: Feed,
    duration: Option<f64>,
    resampler: Resampler,
    /// Position (s) at which `frames_played` was last reset.
    base: f64,
    /// Value of `frames_played` at which this track's audio begins.
    start_frame: u64,
    eof: bool,
    live: bool,
    logged_first_chunk: bool,
    /// Live stream ran dry and we told the UI we're buffering.
    stalled: bool,
    starved_since: Option<Instant>,
}

enum Pull {
    Chunk(Chunk),
    End,
    NotReady,
    Failed(anyhow::Error),
}

struct Engine {
    shared: Arc<Shared>,
    events: Sender<PlayerEvent>,
    output: Option<Output>,
    current: Option<Current>,
    /// Incremented on every load/stop; stale background results are ignored.
    generation: u64,
    pending: Option<(u64, Instant, bool)>,
    opened_tx: Sender<Opened>,
    opened_rx: Receiver<Opened>,
    last_position: Instant,
    /// Gapless: what comes next, and its decoder once prepared.
    next_source: Option<Source>,
    next_gen: u64,
    next_ready: Option<(Feed, Option<f64>)>,
    next_failed: bool,
    /// Frames pushed to the output buffer since the last reset.
    written_frames: u64,
    tail: Option<Clock>,
    /// Crossfade length in seconds (0 = plain gapless).
    crossfade: f64,
}

/// Body of the audio thread.
pub fn run(commands: Receiver<Command>, events: Sender<PlayerEvent>, shared: Arc<Shared>) {
    let (opened_tx, opened_rx) = mpsc::channel();
    let mut engine = Engine {
        shared,
        events,
        output: None,
        current: None,
        generation: 0,
        pending: None,
        opened_tx,
        opened_rx,
        last_position: Instant::now(),
        next_source: None,
        next_gen: 0,
        next_ready: None,
        next_failed: false,
        written_frames: 0,
        tail: None,
        crossfade: 0.0,
    };

    loop {
        let wait = if engine.current.is_some() {
            Duration::ZERO
        } else if engine.pending.is_some() {
            Duration::from_millis(20)
        } else {
            Duration::from_millis(500)
        };
        match commands.recv_timeout(wait) {
            Ok(cmd) => {
                let result = catch_unwind(AssertUnwindSafe(|| engine.handle(cmd)));
                if result.is_err() {
                    log("audio: command handler panicked; recovering");
                    engine.reset();
                    engine.emit(PlayerEvent::Error("Internal audio error (details in the log file)".into()));
                    engine.emit(PlayerEvent::State(PlayState::Stopped));
                }
                continue; // handle every pending command first
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        engine.check_opened();
        engine.step();
    }
}

impl Engine {
    fn emit(&self, e: PlayerEvent) {
        let _ = self.events.send(e);
    }

    fn reset(&mut self) {
        self.generation += 1;
        self.pending = None;
        self.current = None;
        self.next_source = None;
        self.next_ready = None;
        self.next_failed = false;
        self.tail = None;
        self.written_frames = 0;
        self.shared.lock().clear();
        self.shared.frames_played.store(0, Ordering::Relaxed);
    }

    fn set_next(&mut self, next: Option<Source>) {
        if next == self.next_source && (self.next_ready.is_some() || next.is_none()) {
            return; // already prepared
        }
        self.next_gen += 1;
        self.next_ready = None;
        self.next_failed = false;
        self.next_source = next.clone();
        let Some(source) = next else { return };
        if matches!(source, Source::Url(_)) {
            return; // live streams don't chain
        }
        let (generation, next_gen) = (self.generation, self.next_gen);
        let opened_tx = self.opened_tx.clone();
        let events = self.events.clone();
        let _ = std::thread::Builder::new().name("unitn-open-next".into()).spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| open_feed(&source, &events, false)))
                .unwrap_or_else(|_| Err(anyhow!("the decoder crashed while opening this source")));
            let _ = opened_tx.send(Opened { generation, next_gen: Some(next_gen), result });
        });
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Load(source) => self.load(source),
            Command::Play => {
                if self.current.is_some() {
                    self.shared.paused.store(false, Ordering::Relaxed);
                    self.emit(PlayerEvent::State(PlayState::Playing));
                }
            }
            Command::Pause => {
                if self.current.is_some() {
                    self.shared.paused.store(true, Ordering::Relaxed);
                    self.emit(PlayerEvent::State(PlayState::Paused));
                }
            }
            Command::Stop => {
                self.reset();
                self.emit(PlayerEvent::State(PlayState::Stopped));
            }
            Command::Seek(seconds) => self.seek(seconds),
            Command::Volume(v) => self.shared.set_volume(v),
            Command::SetNext(next) => self.set_next(next),
            Command::Crossfade(seconds) => self.crossfade = seconds,
        }
    }

    fn load(&mut self, source: Source) {
        log(format!("audio: load {source:?}"));
        self.reset();
        if self.output.is_none() {
            match Output::open(self.shared.clone()) {
                Ok(o) => self.output = Some(o),
                Err(e) => {
                    log(format!("audio: cannot open output: {e:#}"));
                    self.emit(PlayerEvent::Error(format!("Audio output unavailable: {e}")));
                    self.emit(PlayerEvent::State(PlayState::Stopped));
                    return;
                }
            }
        }
        self.emit(PlayerEvent::State(PlayState::Buffering));

        let generation = self.generation;
        let live = matches!(source, Source::Url(_));
        self.pending = Some((generation, Instant::now(), live));
        let opened_tx = self.opened_tx.clone();
        let events = self.events.clone();
        let spawned = std::thread::Builder::new().name("unitn-open".into()).spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| open_feed(&source, &events, live)))
                .unwrap_or_else(|_| Err(anyhow!("the decoder crashed while opening this source")));
            let _ = opened_tx.send(Opened { generation, next_gen: None, result });
        });
        if let Err(e) = spawned {
            self.pending = None;
            self.emit(PlayerEvent::Error(format!("Could not start playback: {e}")));
            self.emit(PlayerEvent::State(PlayState::Stopped));
        }
    }

    /// Picks up finished background opens and enforces the open timeout.
    fn check_opened(&mut self) {
        while let Ok(opened) = self.opened_rx.try_recv() {
            if let Some(next_gen) = opened.next_gen {
                if opened.generation == self.generation && next_gen == self.next_gen {
                    match opened.result {
                        Ok(feed) => {
                            log("audio: next track prepared");
                            self.next_ready = Some(feed);
                        }
                        Err(e) => {
                            log(format!("audio: cannot prepare next track: {e:#}"));
                            self.next_failed = true;
                        }
                    }
                }
                continue;
            }
            let Some((generation, _, live)) = self.pending else { continue };
            if opened.generation != generation {
                continue; // superseded by a newer load/stop; dropping it ends its threads
            }
            self.pending = None;
            let Some(out) = self.output.as_ref() else { continue };
            match opened.result {
                Ok((feed, duration)) => {
                    log(format!("audio: source ready, duration {duration:?}"));
                    self.emit(PlayerEvent::Position { position: 0.0, duration });
                    self.current = Some(Current {
                        feed,
                        duration,
                        resampler: Resampler::new(out.channels, out.rate),
                        base: 0.0,
                        start_frame: 0,
                        eof: false,
                        live,
                        logged_first_chunk: false,
                        stalled: false,
                        starved_since: None,
                    });
                    self.shared.frames_played.store(0, Ordering::Relaxed);
                    self.shared.paused.store(false, Ordering::Relaxed);
                    self.emit(PlayerEvent::State(PlayState::Playing));
                }
                Err(e) => {
                    log(format!("audio: cannot open source: {e:#}"));
                    self.emit(PlayerEvent::Error(format!("Could not play: {e:#}")));
                    self.emit(PlayerEvent::State(PlayState::Stopped));
                }
            }
        }
        if let Some((_, started, live)) = self.pending {
            if started.elapsed() > OPEN_TIMEOUT {
                log("audio: open timed out");
                self.reset();
                self.emit(PlayerEvent::Error(if live {
                    "The station is not responding or uses an unsupported format".into()
                } else {
                    "The file took too long to open".into()
                }));
                self.emit(PlayerEvent::State(PlayState::Stopped));
            }
        }
    }

    /// Decodes a bit more audio if the output buffer needs it.
    fn step(&mut self) {
        let (Some(cur), Some(out)) = (self.current.as_mut(), self.output.as_ref()) else {
            return;
        };

        // Decoded audio kept ready ahead of the speakers. Files are also fully
        // preloaded into memory (see prefetch.rs); streams get a longer cushion.
        // With crossfade, keep enough audio buffered to fade over.
        let ahead = if cur.live { 4.0 } else { (self.crossfade + 1.5).max(3.0) };
        let target = (out.rate as f64 * out.channels as f64 * ahead) as usize;
        let buffered = self.shared.lock().len();
        let mut idle = true;

        if !cur.eof && buffered < target {
            let pull = match &mut cur.feed {
                Feed::File(decoder) => match catch_unwind(AssertUnwindSafe(|| decoder.next_chunk())) {
                    Ok(Ok(Some(c))) => Pull::Chunk(c),
                    Ok(Ok(None)) => Pull::End,
                    Ok(Err(e)) => Pull::Failed(e),
                    Err(_) => Pull::Failed(anyhow!("decoder crashed")),
                },
                Feed::Live(rx) => match rx.try_recv() {
                    Ok(Ok(Some(c))) => Pull::Chunk(c),
                    Ok(Ok(None)) => Pull::End,
                    Ok(Err(e)) => Pull::Failed(e),
                    Err(TryRecvError::Empty) => Pull::NotReady,
                    Err(TryRecvError::Disconnected) => Pull::Failed(anyhow!("stream closed")),
                },
            };
            if !cur.logged_first_chunk && !matches!(pull, Pull::NotReady) {
                cur.logged_first_chunk = true;
                match &pull {
                    Pull::Chunk(c) => log(format!(
                        "audio: first chunk {} samples, {} ch, {} Hz -> device {} ch, {} Hz",
                        c.samples.len(),
                        c.channels,
                        c.rate,
                        out.channels,
                        out.rate
                    )),
                    Pull::End => log("audio: source ended before any audio"),
                    Pull::Failed(e) => log(format!("audio: first chunk failed: {e:#}")),
                    Pull::NotReady => {}
                }
            }
            match pull {
                Pull::Chunk(chunk) => {
                    idle = false;
                    let mapped = map_channels(&chunk.samples, chunk.channels, out.channels);
                    let samples = cur.resampler.process(mapped, chunk.rate);
                    self.written_frames += (samples.len() / out.channels.max(1)) as u64;
                    self.shared.lock().extend(samples);
                    cur.starved_since = None;
                    if cur.stalled {
                        cur.stalled = false;
                        let _ = self.events.send(PlayerEvent::State(PlayState::Playing));
                    }
                }
                Pull::End => cur.eof = true,
                Pull::NotReady => {
                    // A live stream that runs dry: tell the UI after a moment.
                    if buffered < out.channels {
                        let since = *cur.starved_since.get_or_insert_with(Instant::now);
                        if !cur.stalled && since.elapsed() > Duration::from_millis(800) {
                            cur.stalled = true;
                            let _ = self.events.send(PlayerEvent::State(PlayState::Buffering));
                        }
                    }
                }
                Pull::Failed(e) => {
                    log(format!("audio: decode error: {e:#}"));
                    let _ = self.events.send(PlayerEvent::Error(if cur.live {
                        format!("Stream interrupted: {e}")
                    } else {
                        format!("Playback error: {e}")
                    }));
                    cur.eof = true;
                }
            }
        }
        if idle {
            std::thread::sleep(Duration::from_millis(10));
        }

        let played = self.shared.frames_played.load(Ordering::Relaxed);
        let rate = out.rate as f64;

        // The previous track's last samples just finished: the next one is audible now.
        if self.tail.is_some() && played >= cur.start_frame {
            self.tail = None;
            let _ = self.events.send(PlayerEvent::Advanced);
            let _ = self.events.send(PlayerEvent::Position { position: cur.base, duration: cur.duration });
        }

        if self.last_position.elapsed() >= Duration::from_millis(250) {
            self.last_position = Instant::now();
            let (position, duration) = match &self.tail {
                Some(t) => (t.base + played.saturating_sub(t.start_frame) as f64 / rate, t.duration),
                None => (cur.base + played.saturating_sub(cur.start_frame) as f64 / rate, cur.duration),
            };
            let _ = self.events.send(PlayerEvent::Position { position, duration });
        }

        let eof = cur.eof;
        let channels = out.channels;

        // Gapless: chain the prepared next track right behind this one.
        if eof && self.next_ready.is_some() {
            if let (Some(old), Some((mut feed, duration))) = (self.current.take(), self.next_ready.take()) {
                let mut resampler = old.resampler;
                let mut start_frame = self.written_frames;
                let mut next_eof = false;
                let fade = (self.crossfade * rate) as usize;
                if fade > 0 {
                    // Crossfade: the old track's last seconds are still in the
                    // output buffer. Decode the start of the next track and
                    // blend it into them.
                    let buffered = self.shared.lock().len() / channels.max(1);
                    // Leave a little room for the audio the device is about to take.
                    let want = fade.min(buffered.saturating_sub(rate as usize / 20));
                    let mut fresh: Vec<f32> = Vec::with_capacity(want * channels);
                    if let Feed::File(decoder) = &mut feed {
                        while fresh.len() < want * channels {
                            match catch_unwind(AssertUnwindSafe(|| decoder.next_chunk())) {
                                Ok(Ok(Some(chunk))) => {
                                    let mapped = map_channels(&chunk.samples, chunk.channels, channels);
                                    fresh.extend(resampler.process(mapped, chunk.rate));
                                }
                                _ => {
                                    next_eof = true;
                                    break;
                                }
                            }
                        }
                    }
                    let mut buf = self.shared.lock();
                    let overlap = (fresh.len() / channels.max(1)).min(want).min(buf.len() / channels.max(1));
                    crossfade_into(&mut buf, &fresh[..overlap * channels], channels);
                    let rest = &fresh[overlap * channels..];
                    buf.extend(rest.iter().copied());
                    drop(buf);
                    start_frame = self.written_frames.saturating_sub(overlap as u64);
                    self.written_frames += (rest.len() / channels.max(1)) as u64;
                    log(format!("audio: crossfade over {:.1} s", overlap as f64 / rate));
                } else {
                    log("audio: gapless switch to the next track");
                }
                self.tail = Some(Clock { start_frame: old.start_frame, base: old.base, duration: old.duration });
                self.next_source = None;
                self.current = Some(Current {
                    feed,
                    duration,
                    resampler,
                    base: 0.0,
                    start_frame,
                    eof: next_eof,
                    live: false,
                    logged_first_chunk: false,
                    stalled: false,
                    starved_since: None,
                });
            }
            return;
        }

        // Still waiting for the next track to be prepared: don't end yet.
        let waiting_for_next = self.next_source.is_some() && self.next_ready.is_none() && !self.next_failed;
        if eof && !waiting_for_next && self.shared.lock().len() < channels {
            log("audio: ended");
            self.current = None;
            self.tail = None;
            self.emit(PlayerEvent::Ended);
        }
    }

    fn seek(&mut self, seconds: f64) {
        let Some(cur) = self.current.as_mut() else { return };
        let Feed::File(decoder) = &mut cur.feed else { return };
        match decoder.seek(seconds) {
            Ok(reached) => {
                self.shared.lock().clear();
                cur.resampler.reset();
                cur.base = reached;
                cur.start_frame = 0;
                cur.eof = false;
                self.tail = None;
                self.written_frames = 0;
                self.shared.frames_played.store(0, Ordering::Relaxed);
                let duration = cur.duration;
                self.emit(PlayerEvent::Position { position: reached, duration });
            }
            Err(e) => self.emit(PlayerEvent::Error(format!("Seek failed: {e}"))),
        }
    }
}

/// Blends `fresh` (the start of the next track) into the last frames of
/// `buf` (the end of the current one) with an equal-power fade.
pub(crate) fn crossfade_into(buf: &mut std::collections::VecDeque<f32>, fresh: &[f32], channels: usize) {
    let channels = channels.max(1);
    let frames = fresh.len() / channels;
    if frames == 0 || buf.len() < frames * channels {
        return;
    }
    let start = buf.len() - frames * channels;
    for f in 0..frames {
        let t = (f as f32 + 0.5) / frames as f32 * std::f32::consts::FRAC_PI_2;
        let (fade_in, fade_out) = (t.sin(), t.cos());
        for c in 0..channels {
            let i = start + f * channels + c;
            buf[i] = buf[i] * fade_out + fresh[f * channels + c] * fade_in;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::crossfade_into;
    use std::collections::VecDeque;

    #[test]
    fn crossfade_blends_the_overlap() {
        // 10 frames of the old track (value 1.0, stereo), 4 frames of the new (value 0.5).
        let mut buf: VecDeque<f32> = std::iter::repeat(1.0).take(20).collect();
        let fresh = vec![0.5; 8];
        crossfade_into(&mut buf, &fresh, 2);
        assert_eq!(buf.len(), 20);
        // Untouched before the overlap.
        assert!(buf.iter().take(12).all(|&v| v == 1.0));
        // Old fades out, new fades in: first overlap frame mostly old, last mostly new.
        assert!(buf[12] > 0.9 && buf[13] > 0.9);
        assert!((buf[18] - 0.5).abs() < 0.2);
        // Both channels get the same treatment.
        for f in 6..10 {
            assert_eq!(buf[f * 2], buf[f * 2 + 1]);
        }
        // Equal power: never louder than the louder input by much.
        assert!(buf.iter().all(|&v| v <= 1.15));
    }

    #[test]
    fn crossfade_ignores_too_little_buffer() {
        let mut buf: VecDeque<f32> = std::iter::repeat(1.0).take(4).collect();
        crossfade_into(&mut buf, &[0.0; 8], 2);
        assert!(buf.iter().all(|&v| v == 1.0));
    }
}

/// Runs on a worker thread: opens the source and, for live streams, keeps
/// decoding into a bounded queue until the engine drops the receiver.
fn open_feed(source: &Source, events: &Sender<PlayerEvent>, live: bool) -> Result<(Feed, Option<f64>)> {
    let decoder = Decoder::open(source, events)?;
    let duration = decoder.duration;
    if !live {
        return Ok((Feed::File(decoder), duration));
    }
    let (tx, rx): (SyncSender<Result<Option<Chunk>>>, _) = mpsc::sync_channel(LIVE_QUEUE);
    std::thread::Builder::new().name("unitn-stream".into()).spawn(move || {
        let mut decoder = decoder;
        loop {
            let next = catch_unwind(AssertUnwindSafe(|| decoder.next_chunk()))
                .unwrap_or_else(|_| Err(anyhow!("decoder crashed")));
            let done = !matches!(next, Ok(Some(_)));
            if tx.send(next).is_err() || done {
                break; // engine moved on, or the stream ended
            }
        }
        log("audio: stream reader finished");
    })?;
    Ok((Feed::Live(rx), duration))
}
