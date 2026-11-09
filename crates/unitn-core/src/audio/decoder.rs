use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder as SymDecoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, Packet, SeekMode, SeekTo};
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use super::adts::AdtsFramer;
use super::prefetch;
use super::{stream, PlayerEvent, Source};

/// A live stream must reveal its format within this many bytes; otherwise we
/// give up instead of scanning an endless stream forever.
const PROBE_BUDGET: u64 = 512 * 1024;

/// Limits how much of a stream format detection may read.
struct ProbeBudget {
    inner: Box<dyn Read + Send + Sync>,
    limit: Arc<AtomicU64>,
    read: u64,
}

impl Read for ProbeBudget {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.read > self.limit.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "no recognisable audio in the first 512 KB of the stream",
            ));
        }
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        Ok(n)
    }
}

/// A chunk of decoded, interleaved f32 audio.
pub struct Chunk {
    pub samples: Vec<f32>,
    pub channels: usize,
    pub rate: u32,
}

/// Where packets come from.
enum Demux {
    /// Any container symphonia understands.
    Format(Box<dyn FormatReader>),
    /// Raw AAC (ADTS) radio stream, framed by our own tolerant reader.
    Adts { framer: AdtsFramer<ProbeBudget>, pending: VecDeque<Vec<u8>>, ts: u64 },
}

/// Demuxer + decoder for one source.
pub struct Decoder {
    demux: Demux,
    decoder: Box<dyn SymDecoder>,
    track_id: u32,
    /// Frames still to drop after an accurate seek (seek lands on a packet boundary).
    skip_frames: u64,
    /// Total length in seconds, if known (not for live streams).
    pub duration: Option<f64>,
}

fn probe(hint: &Hint, mss: MediaSourceStream, options: &FormatOptions) -> Result<Box<dyn FormatReader>> {
    let probed = symphonia::default::get_probe()
        .format(hint, mss, options, &MetadataOptions::default())
        .context("unsupported or corrupt audio format")?;
    Ok(probed.format)
}

/// Hides access tokens in URLs before they reach the log.
fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for key in ["api_key=", "X-Plex-Token="] {
        while let Some(start) = out.find(key) {
            let from = start + key.len();
            let end = out[from..].find(|c: char| c == '&' || c.is_whitespace()).map(|e| from + e).unwrap_or(out.len());
            if out[from..end].starts_with("***") {
                break;
            }
            out.replace_range(from..end, "***");
        }
    }
    out
}

impl Decoder {
    pub fn open(source: &Source, events: &Sender<PlayerEvent>) -> Result<Self> {
        let mut hint = Hint::new();
        let format_options = FormatOptions { enable_gapless: true, ..Default::default() };
        let demux = match source {
            Source::File(path) => {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    hint.with_extension(ext);
                }
                let size = std::fs::metadata(path)
                    .with_context(|| format!("cannot open {}", path.display()))?
                    .len();
                // Load the whole file into memory in the background so slow
                // disks and network shares can't cause dropouts.
                let source: Box<dyn symphonia::core::io::MediaSource> = if size <= prefetch::MAX_PREFETCH {
                    crate::log::log(format!("audio: preloading {:.1} MB", size as f64 / 1_048_576.0));
                    Box::new(
                        prefetch::PrefetchReader::open(path)
                            .with_context(|| format!("cannot open {}", path.display()))?,
                    )
                } else {
                    Box::new(File::open(path).with_context(|| format!("cannot open {}", path.display()))?)
                };
                let mss = MediaSourceStream::new(source, Default::default());
                Demux::Format(probe(&hint, mss, &format_options)?)
            }
            Source::Remote { url, ext } => {
                let agent = ureq::AgentBuilder::new()
                    .user_agent(crate::USER_AGENT)
                    .timeout_connect(std::time::Duration::from_secs(10))
                    .timeout_read(std::time::Duration::from_secs(30))
                    .build();
                let resp = agent.get(url).call().map_err(|e| anyhow::anyhow!("server: {}", redact(&e.to_string())))?;
                let content_type = resp.content_type().to_string();
                let len = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok());
                crate::log::log(format!(
                    "audio: server file {content_type:?}, {:.1} MB",
                    len.unwrap_or(0) as f64 / 1_048_576.0
                ));
                match ext.as_deref().or_else(|| stream::extension_for(&content_type)) {
                    Some(e) => {
                        hint.with_extension(e);
                    }
                    None => {
                        hint.mime_type(&content_type);
                    }
                }
                let reader = prefetch::PrefetchReader::from_reader(resp.into_reader(), len)?;
                let mss = MediaSourceStream::new(Box::new(reader), Default::default());
                Demux::Format(probe(&hint, mss, &format_options)?)
            }
            Source::Url(url) => {
                let opened = stream::open(url, events.clone())?;
                crate::log::log(format!("audio: stream content type {:?}", opened.content_type));
                let ext = stream::extension_for(&opened.content_type);
                if let Some(ext) = ext {
                    hint.with_extension(ext);
                }
                if !opened.content_type.is_empty() {
                    hint.mime_type(&opened.content_type);
                }
                let limit = Arc::new(AtomicU64::new(PROBE_BUDGET));
                let reader = ProbeBudget { inner: opened.reader, limit: limit.clone(), read: 0 };
                let demux = if ext == Some("aac") {
                    let mut framer = AdtsFramer::new(reader);
                    let header = framer.lock().context("no AAC audio found in the stream")?;
                    crate::log::log(format!("audio: ADTS stream {header:?}"));
                    let _ = header;
                    Demux::Adts { framer, pending: VecDeque::new(), ts: 0 }
                } else {
                    let mss = MediaSourceStream::new(Box::new(ReadOnlySource::new(reader)), Default::default());
                    let live_options = FormatOptions { enable_gapless: false, ..Default::default() };
                    Demux::Format(probe(&hint, mss, &live_options)?)
                };
                limit.store(u64::MAX, Ordering::Relaxed);
                demux
            }
        };

        let (track_id, params) = match &demux {
            Demux::Format(format) => {
                let track = format
                    .tracks()
                    .iter()
                    .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
                    .ok_or_else(|| anyhow!("no playable audio track"))?;
                (track.id, track.codec_params.clone())
            }
            Demux::Adts { framer, .. } => {
                let header = framer.header().ok_or_else(|| anyhow!("no AAC audio found in the stream"))?;
                (0, header.codec_params())
            }
        };
        let decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .context("unsupported codec")?;

        let duration = match (params.n_frames, params.time_base, params.sample_rate) {
            (Some(n), Some(tb), _) => {
                let t = tb.calc_time(n);
                Some(t.seconds as f64 + t.frac)
            }
            (Some(n), None, Some(rate)) if rate > 0 => Some(n as f64 / rate as f64),
            _ => None,
        };

        Ok(Self { demux, decoder, track_id, skip_frames: 0, duration })
    }

    fn next_packet(&mut self) -> Result<Option<Packet>, SymError> {
        match &mut self.demux {
            Demux::Format(format) => match format.next_packet() {
                Ok(p) => Ok(Some(p)),
                Err(SymError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => Ok(None),
                Err(e) => Err(e),
            },
            Demux::Adts { framer, pending, ts } => loop {
                if let Some(block) = pending.pop_front() {
                    let packet = Packet::new_from_boxed_slice(0, *ts, 1024, block.into_boxed_slice());
                    *ts += 1024;
                    return Ok(Some(packet));
                }
                match framer.next_frame() {
                    Ok(Some(blocks)) => pending.extend(blocks),
                    Ok(None) => return Ok(None),
                    Err(e) => return Err(SymError::IoError(e)),
                }
            },
        }
    }

    /// Decodes the next chunk. `Ok(None)` means end of stream.
    pub fn next_chunk(&mut self) -> Result<Option<Chunk>> {
        loop {
            let packet = match self.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(None),
                Err(SymError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    if decoded.frames() == 0 {
                        continue;
                    }
                    let spec = *decoded.spec();
                    let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                    buf.copy_interleaved_ref(decoded);
                    let channels = spec.channels.count().max(1);
                    let mut samples = buf.samples();
                    if self.skip_frames > 0 {
                        let frames = (samples.len() / channels) as u64;
                        let drop = self.skip_frames.min(frames);
                        self.skip_frames -= drop;
                        samples = &samples[drop as usize * channels..];
                        if samples.is_empty() {
                            continue;
                        }
                    }
                    return Ok(Some(Chunk {
                        samples: samples.to_vec(),
                        channels,
                        rate: spec.rate,
                    }));
                }
                // A corrupt frame: skip it, like most players do.
                Err(SymError::DecodeError(_)) => continue,
                Err(SymError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Seeks to `seconds`; returns the position actually reached.
    pub fn seek(&mut self, seconds: f64) -> Result<f64> {
        let Demux::Format(format) = &mut self.demux else {
            anyhow::bail!("live streams cannot seek");
        };
        let seeked = format.seek(
            SeekMode::Accurate,
            SeekTo::Time { time: Time::from(seconds), track_id: Some(self.track_id) },
        )?;
        self.decoder.reset();
        self.skip_frames = seeked.required_ts.saturating_sub(seeked.actual_ts);
        let Demux::Format(format) = &self.demux else { unreachable!() };
        let reached = format
            .tracks()
            .iter()
            .find(|t| t.id == self.track_id)
            .and_then(|t| t.codec_params.time_base)
            .map(|tb| {
                let t = tb.calc_time(seeked.required_ts);
                t.seconds as f64 + t.frac
            })
            .unwrap_or(seconds);
        Ok(reached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn decodes_and_seeks_a_wav_file() {
        let path = std::env::temp_dir().join(format!("unitn-dec-{}.wav", std::process::id()));
        std::fs::write(&path, crate::audio::test_wav(8000, 1.0)).unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut dec = Decoder::open(&Source::File(path.clone()), &tx).unwrap();
        let dur = dec.duration.unwrap();
        assert!((dur - 1.0).abs() < 0.01, "duration {dur}");

        let mut frames = 0;
        while let Some(chunk) = dec.next_chunk().unwrap() {
            assert_eq!(chunk.channels, 1);
            assert_eq!(chunk.rate, 8000);
            frames += chunk.samples.len();
        }
        assert_eq!(frames, 8000);

        let reached = dec.seek(0.5).unwrap();
        assert!((reached - 0.5).abs() < 0.05, "reached {reached}");
        assert!(dec.next_chunk().unwrap().is_some());
        let _ = std::fs::remove_file(path);
    }
}
