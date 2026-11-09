use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{anyhow, bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use super::eq::{EqSettings, Equalizer};

/// State shared between the engine thread and the device callback.
pub struct Shared {
    pub buffer: Mutex<VecDeque<f32>>,
    pub paused: AtomicBool,
    volume: AtomicU32,
    /// Frames handed to the device since the last reset (drives the position).
    pub frames_played: AtomicU64,
    /// Mono copy of the most recently played audio (pre-volume), for the visualizer.
    pub tap: Mutex<VecDeque<f32>>,
    /// Total samples ever written to `tap` (lets readers detect fresh audio).
    pub tap_written: AtomicU64,
    /// Output sample rate in Hz (0 until the device is opened).
    pub rate: AtomicU32,
    eq_settings: Mutex<EqSettings>,
    /// Bumped on every equalizer change so the callback picks it up.
    eq_version: AtomicU64,
}

/// Samples kept in the visualizer tap.
pub const TAP_CAPACITY: usize = 8192;

impl Shared {
    pub fn new(volume: f32) -> Self {
        Self {
            buffer: Mutex::new(VecDeque::new()),
            paused: AtomicBool::new(false),
            volume: AtomicU32::new(volume.to_bits()),
            frames_played: AtomicU64::new(0),
            tap: Mutex::new(VecDeque::with_capacity(TAP_CAPACITY + 4096)),
            tap_written: AtomicU64::new(0),
            rate: AtomicU32::new(0),
            eq_settings: Mutex::new(EqSettings::default()),
            eq_version: AtomicU64::new(1),
        }
    }

    pub fn set_eq(&self, settings: EqSettings) {
        if let Ok(mut s) = self.eq_settings.lock() {
            *s = settings;
        }
        self.eq_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Appends mono samples to the visualizer tap (used by tests and the device callback).
    #[cfg(test)]
    pub fn push_tap(&self, samples: impl IntoIterator<Item = f32>) {
        if let Ok(mut tap) = self.tap.try_lock() {
            let mut n = 0u64;
            for s in samples {
                tap.push_back(s);
                n += 1;
            }
            while tap.len() > TAP_CAPACITY {
                tap.pop_front();
            }
            self.tap_written.fetch_add(n, Ordering::Relaxed);
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, VecDeque<f32>> {
        self.buffer.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.to_bits(), Ordering::Relaxed);
    }

    fn gain(&self) -> f32 {
        let v = f32::from_bits(self.volume.load(Ordering::Relaxed));
        v * v // perceptual curve
    }
}

/// An open output stream. Must stay on the thread that created it.
pub struct Output {
    _stream: cpal::Stream,
    pub rate: u32,
    pub channels: usize,
}

impl Output {
    pub fn open(shared: Arc<Shared>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("no audio output device found"))?;
        let supported = device.default_output_config()?;
        let format = supported.sample_format();
        crate::log::log(format!(
            "audio: output device {:?}, {:?}, {} ch, {} Hz",
            device.name().unwrap_or_default(),
            format,
            supported.channels(),
            supported.sample_rate().0
        ));
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels as usize;
        let rate = config.sample_rate.0;
        let s = shared.clone();
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, s)?,
            SampleFormat::I16 => build::<i16>(&device, &config, s)?,
            SampleFormat::U16 => build::<u16>(&device, &config, s)?,
            SampleFormat::I32 => build::<i32>(&device, &config, s)?,
            SampleFormat::F64 => build::<f64>(&device, &config, s)?,
            other => bail!("unsupported output sample format {other:?}"),
        };
        stream.play()?;
        shared.rate.store(rate, Ordering::Relaxed);
        Ok(Self { _stream: stream, rate, channels })
    }
}

fn build<T>(device: &cpal::Device, config: &cpal::StreamConfig, shared: Arc<Shared>) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut eq = Equalizer::new(config.sample_rate.0, channels);
    let mut eq_seen = 0u64;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let version = shared.eq_version.load(Ordering::Relaxed);
            if version != eq_seen {
                if let Ok(settings) = shared.eq_settings.try_lock() {
                    eq.configure(*settings);
                    eq_seen = version;
                }
            }
            let silence = T::from_sample(0.0f32);
            if shared.paused.load(Ordering::Relaxed) {
                data.fill(silence);
                return;
            }
            let gain = shared.gain();
            let mut frames = 0u64;
            let mut buf = shared.lock();
            // try_lock: never block the audio thread for the visualizer.
            let mut tap = shared.tap.try_lock().ok();
            for frame in data.chunks_mut(channels) {
                if buf.len() >= frame.len() {
                    let mut tmp = [0.0f32; 8];
                    let n = frame.len().min(tmp.len());
                    for k in 0..frame.len() {
                        let v = buf.pop_front().unwrap_or(0.0);
                        if k < n {
                            tmp[k] = v;
                        }
                    }
                    eq.process_frame(&mut tmp[..n]);
                    let mut mono = 0.0f32;
                    for (k, s) in frame.iter_mut().enumerate() {
                        let v = if k < n { tmp[k] } else { 0.0 };
                        mono += v;
                        *s = T::from_sample(v * gain);
                    }
                    if let Some(t) = tap.as_mut() {
                        t.push_back(mono / channels as f32);
                    }
                    frames += 1;
                } else {
                    frame.fill(silence);
                }
            }
            drop(buf);
            if let Some(mut t) = tap {
                while t.len() > TAP_CAPACITY {
                    t.pop_front();
                }
                drop(t);
                shared.tap_written.fetch_add(frames, Ordering::Relaxed);
            }
            shared.frames_played.fetch_add(frames, Ordering::Relaxed);
        },
        |err| crate::log::log(format!("audio: stream error: {err}")),
        None,
    )?;
    Ok(stream)
}
