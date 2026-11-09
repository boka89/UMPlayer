//! Real-time spectrum analyzer for the visualizer.
//!
//! Takes the most recent mono samples from the output tap, runs a windowed
//! FFT, groups the bins into logarithmically spaced bars and animates them
//! like a classic hi-fi analyzer: bars jump up quickly and fall with a
//! constant speed, and peak caps hold briefly before dropping with gravity.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::output::Shared;

const FFT_SIZE: usize = 4096;
const MIN_FREQ: f32 = 40.0;
const MAX_FREQ: f32 = 16_000.0;
/// Level range mapped to bar height 0..1, in dBFS.
const FLOOR_DB: f32 = -72.0;
const CEIL_DB: f32 = -12.0;
/// Spectral tilt (dB per octave around 1 kHz) so music looks balanced.
const TILT_DB_PER_OCTAVE: f32 = 3.0;
/// How fast bars fall, in full heights per second.
const FALL_SPEED: f32 = 1.6;
const PEAK_HOLD: f32 = 0.35;
const PEAK_GRAVITY: f32 = 2.8;
/// Without new audio for this long, the bars fall to zero.
const SILENCE_AFTER: Duration = Duration::from_millis(150);

pub struct Spectrum {
    shared: Arc<Shared>,
    fft: Fft,
    window: Vec<f32>,
    window_gain: f32,
    re: Vec<f32>,
    im: Vec<f32>,
    targets: Vec<f32>,
    bars: Vec<f32>,
    peaks: Vec<f32>,
    peak_hold: Vec<f32>,
    peak_speed: Vec<f32>,
    /// FFT bin range per bar, computed for `bands_rate`.
    bands: Vec<(usize, usize, f32)>,
    bands_rate: u32,
    last_written: u64,
    last_audio: Instant,
    last_update: Instant,
}

impl Spectrum {
    pub(crate) fn new(shared: Arc<Shared>, bars: usize) -> Self {
        let bars = bars.max(1);
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (FFT_SIZE - 1) as f32).cos())
            .collect();
        let window_gain = window.iter().sum::<f32>();
        let now = Instant::now();
        Self {
            shared,
            fft: Fft::new(FFT_SIZE),
            window,
            window_gain,
            re: vec![0.0; FFT_SIZE],
            im: vec![0.0; FFT_SIZE],
            targets: vec![0.0; bars],
            bars: vec![0.0; bars],
            peaks: vec![0.0; bars],
            peak_hold: vec![0.0; bars],
            peak_speed: vec![0.0; bars],
            bands: Vec::new(),
            bands_rate: 0,
            last_written: 0,
            last_audio: now - SILENCE_AFTER * 2,
            last_update: now,
        }
    }

    /// Bar heights, 0..1.
    pub fn bars(&self) -> &[f32] {
        &self.bars
    }

    /// Peak cap positions, 0..1.
    pub fn peaks(&self) -> &[f32] {
        &self.peaks
    }

    /// Recent waveform (`points` samples, -1..1) for an oscilloscope view,
    /// aligned on a rising zero crossing so the picture stays steady.
    pub fn waveform(&self, points: usize) -> Vec<f32> {
        let points = points.max(2);
        if Instant::now() - self.last_audio > SILENCE_AFTER {
            return vec![0.0; points];
        }
        let samples: Vec<f32> = {
            let tap = match self.shared.tap.lock() {
                Ok(t) => t,
                Err(e) => e.into_inner(),
            };
            let n = tap.len().min(2048);
            tap.range(tap.len() - n..).copied().collect()
        };
        if samples.len() < 8 {
            return vec![0.0; points];
        }
        let half = samples.len() / 2;
        let start = (1..half).find(|&i| samples[i - 1] <= 0.0 && samples[i] > 0.0).unwrap_or(0);
        let window = &samples[start..start + half];
        (0..points).map(|k| window[k * window.len() / points]).collect()
    }

    /// Advances the animation; call it once per UI frame. Returns `false` when
    /// everything is at rest at zero (nothing to redraw).
    pub fn update(&mut self) -> bool {
        let now = Instant::now();
        let dt = (now - self.last_update).as_secs_f32().clamp(0.0, 0.1);
        self.last_update = now;

        let written = self.shared.tap_written.load(Ordering::Relaxed);
        let paused = self.shared.paused.load(Ordering::Relaxed);
        if written != self.last_written && !paused {
            self.last_written = written;
            self.last_audio = now;
        }

        if now - self.last_audio < SILENCE_AFTER {
            self.analyze();
        } else {
            self.targets.iter_mut().for_each(|t| *t = 0.0);
        }
        self.animate(dt)
    }

    fn analyze(&mut self) {
        let rate = self.shared.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return;
        }
        if rate != self.bands_rate {
            self.bands = band_layout(self.bars.len(), rate, FFT_SIZE);
            self.bands_rate = rate;
        }

        {
            let tap = match self.shared.tap.lock() {
                Ok(t) => t,
                Err(e) => e.into_inner(),
            };
            let available = tap.len().min(FFT_SIZE);
            let pad = FFT_SIZE - available;
            for i in 0..pad {
                self.re[i] = 0.0;
            }
            for (i, s) in tap.iter().skip(tap.len() - available).enumerate() {
                self.re[pad + i] = *s;
            }
        }
        for (r, w) in self.re.iter_mut().zip(&self.window) {
            *r *= *w;
        }
        self.im.iter_mut().for_each(|v| *v = 0.0);
        self.fft.process(&mut self.re, &mut self.im);

        let scale = 2.0 / self.window_gain;
        for (bar, &(lo, hi, center)) in self.bands.iter().enumerate() {
            let mut peak = 0.0f32;
            for k in lo..hi {
                let m = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt();
                peak = peak.max(m);
            }
            let db = 20.0 * (peak * scale + 1e-9).log10() + TILT_DB_PER_OCTAVE * (center / 1000.0).log2();
            self.targets[bar] = ((db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)).clamp(0.0, 1.0);
        }
    }

    fn animate(&mut self, dt: f32) -> bool {
        let mut visible = false;
        for i in 0..self.bars.len() {
            let target = self.targets[i];
            let bar = &mut self.bars[i];
            if target > *bar {
                // Rise fast but not instantly, so bars look smooth.
                *bar += (target - *bar) * (dt * 30.0).min(1.0);
            } else {
                *bar = (*bar - FALL_SPEED * dt).max(target);
            }

            let bar = *bar;
            let peak = &mut self.peaks[i];
            if bar >= *peak {
                *peak = bar;
                self.peak_hold[i] = PEAK_HOLD;
                self.peak_speed[i] = 0.0;
            } else if self.peak_hold[i] > 0.0 {
                self.peak_hold[i] -= dt;
            } else {
                self.peak_speed[i] += PEAK_GRAVITY * dt;
                *peak = (*peak - self.peak_speed[i] * dt).max(bar);
            }
            if bar > 0.001 || *peak > 0.001 {
                visible = true;
            }
        }
        visible
    }
}

/// Logarithmically spaced FFT bin ranges `(lo, hi, center_hz)` for `bars` bars.
fn band_layout(bars: usize, rate: u32, size: usize) -> Vec<(usize, usize, f32)> {
    let bin_hz = rate as f32 / size as f32;
    let max_freq = MAX_FREQ.min(rate as f32 / 2.0 * 0.95);
    let ratio = max_freq / MIN_FREQ;
    let edge = |i: usize| MIN_FREQ * ratio.powf(i as f32 / bars as f32);
    (0..bars)
        .map(|i| {
            let (f0, f1) = (edge(i), edge(i + 1));
            let lo = ((f0 / bin_hz).floor() as usize).clamp(1, size / 2 - 1);
            let hi = ((f1 / bin_hz).ceil() as usize).clamp(lo + 1, size / 2);
            (lo, hi, (f0 * f1).sqrt())
        })
        .collect()
}

/// Minimal iterative radix-2 complex FFT (in place).
struct Fft {
    size: usize,
    cos: Vec<f32>,
    sin: Vec<f32>,
    reversed: Vec<usize>,
}

impl Fft {
    fn new(size: usize) -> Self {
        assert!(size.is_power_of_two());
        let bits = size.trailing_zeros();
        let reversed = (0..size).map(|i| i.reverse_bits() >> (usize::BITS - bits)).collect();
        let (cos, sin) = (0..size / 2)
            .map(|k| {
                let a = -std::f32::consts::TAU * k as f32 / size as f32;
                (a.cos(), a.sin())
            })
            .unzip();
        Self { size, cos, sin, reversed }
    }

    fn process(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.size;
        for i in 0..n {
            let j = self.reversed[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let (wr, wi) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_finds_a_pure_tone() {
        let n = 1024;
        let fft = Fft::new(n);
        let mut re: Vec<f32> = (0..n).map(|i| (std::f32::consts::TAU * 50.0 * i as f32 / n as f32).sin()).collect();
        let mut im = vec![0.0; n];
        fft.process(&mut re, &mut im);
        let mags: Vec<f32> = (0..n / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
        let max = mags.iter().cloned().enumerate().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        assert_eq!(max.0, 50);
        assert!((max.1 - n as f32 / 2.0).abs() < 1.0);
    }

    #[test]
    fn bands_cover_the_spectrum_in_order() {
        let bands = band_layout(64, 44_100, FFT_SIZE);
        assert_eq!(bands.len(), 64);
        for w in bands.windows(2) {
            assert!(w[0].0 <= w[1].0 && w[0].2 < w[1].2);
        }
        assert!(bands.iter().all(|(lo, hi, _)| lo < hi && *hi <= FFT_SIZE / 2));
    }

    #[test]
    fn analyzer_shows_a_tone_and_falls_silent() {
        let shared = Arc::new(Shared::new(1.0));
        shared.rate.store(48_000, Ordering::Relaxed);
        shared.push_tap((0..FFT_SIZE).map(|i| 0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin()));
        let mut spec = Spectrum::new(shared.clone(), 32);
        for _ in 0..12 {
            std::thread::sleep(Duration::from_millis(5));
            assert!(spec.update());
        }
        let bars = spec.bars().to_vec();
        let loudest = bars.iter().cloned().enumerate().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap().0;
        let (lo, hi, _) = band_layout(32, 48_000, FFT_SIZE)[loudest];
        let bin_hz = 48_000.0 / FFT_SIZE as f32;
        assert!((lo as f32 * bin_hz) <= 1100.0 && (hi as f32 * bin_hz) >= 900.0, "loudest bar {loudest}");
        assert!(bars[loudest] > 0.5);
        assert!(bars[0] < 0.2, "low bars should stay quiet: {}", bars[0]);

        // No new audio: everything falls back to zero.
        std::thread::sleep(SILENCE_AFTER + Duration::from_millis(20));
        let mut frames = 0;
        while spec.update() {
            std::thread::sleep(Duration::from_millis(20));
            frames += 1;
            assert!(frames < 200, "bars never settled");
        }
        assert!(spec.peaks().iter().all(|p| *p <= 0.001));
    }
}
