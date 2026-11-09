//! 10-band graphic equalizer (peaking biquads, RBJ cookbook), applied in
//! the output callback so changes are heard immediately.

/// Band centre frequencies in Hz.
pub const BANDS: [f32; 10] = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
/// Gain range of each band, in dB.
pub const MAX_GAIN_DB: f32 = 12.0;
const Q: f32 = 1.41; // about one octave per band
const MAX_CHANNELS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqSettings {
    pub enabled: bool,
    pub gains: [f32; 10],
    /// Overall gain before the bands, in dB (-12 … +12).
    pub preamp_db: f32,
}

impl Default for EqSettings {
    fn default() -> Self {
        Self { enabled: false, gains: [0.0; 10], preamp_db: 0.0 }
    }
}

#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    fn peaking(freq: f32, gain_db: f32, rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * freq / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * Q);
        let a0 = 1.0 + alpha / a;
        Self {
            b0: (1.0 + alpha * a) / a0,
            b1: (-2.0 * cos) / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha / a) / a0,
        }
    }
}

pub struct Equalizer {
    rate: f32,
    channels: usize,
    settings: EqSettings,
    filters: Vec<Biquad>,
    /// Per filter and channel: x1, x2, y1, y2.
    state: Vec<[f32; 4]>,
    preamp: f32,
    active: bool,
}

impl Equalizer {
    pub fn new(rate: u32, channels: usize) -> Self {
        let mut eq = Self {
            rate: rate as f32,
            channels: channels.min(MAX_CHANNELS),
            settings: EqSettings::default(),
            filters: Vec::new(),
            state: Vec::new(),
            preamp: 1.0,
            active: false,
        };
        eq.configure(EqSettings::default());
        eq
    }

    pub fn configure(&mut self, settings: EqSettings) {
        if settings == self.settings && !self.filters.is_empty() {
            return;
        }
        let nyquist = self.rate / 2.0;
        let old_len = self.filters.len();
        self.settings = settings;
        self.filters = BANDS
            .iter()
            .zip(settings.gains.iter())
            .filter(|(f, g)| g.abs() > 0.05 && **f < nyquist * 0.9)
            .map(|(f, g)| Biquad::peaking(*f, g.clamp(-MAX_GAIN_DB, MAX_GAIN_DB), self.rate))
            .collect();
        if self.filters.len() != old_len {
            self.state = vec![[0.0; 4]; self.filters.len() * self.channels];
        }
        let preamp_db = settings.preamp_db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB);
        self.active = settings.enabled && (!self.filters.is_empty() || preamp_db.abs() > 0.05);
        // Leave headroom for boosts so loud music doesn't clip.
        let max_boost = settings.gains.iter().cloned().fold(0.0f32, f32::max);
        self.preamp = 10f32.powf((preamp_db - max_boost) / 20.0);
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Processes one interleaved frame in place.
    pub fn process_frame(&mut self, frame: &mut [f32]) {
        if !self.active {
            return;
        }
        for (ch, sample) in frame.iter_mut().enumerate().take(self.channels) {
            let mut x = *sample * self.preamp;
            for (i, f) in self.filters.iter().enumerate() {
                let s = &mut self.state[i * self.channels + ch];
                let y = f.b0 * x + f.b1 * s[0] + f.b2 * s[1] - f.a1 * s[2] - f.a2 * s[3];
                s[1] = s[0];
                s[0] = x;
                s[3] = s[2];
                s[2] = y;
                x = y;
            }
            *sample = x.clamp(-1.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms_after(eq: &mut Equalizer, freq: f32) -> f32 {
        let rate = 48_000.0;
        let mut sum = 0.0;
        let n = 48_000;
        for i in 0..n {
            let v = 0.1 * (std::f32::consts::TAU * freq * i as f32 / rate).sin();
            let mut frame = [v, v];
            eq.process_frame(&mut frame);
            if i > n / 2 {
                sum += frame[0] * frame[0];
            }
        }
        (sum / (n / 2) as f32).sqrt()
    }

    #[test]
    fn flat_or_disabled_is_transparent() {
        let mut eq = Equalizer::new(48_000, 2);
        assert!(!eq.is_active());
        let mut frame = [0.3, -0.2];
        eq.process_frame(&mut frame);
        assert_eq!(frame, [0.3, -0.2]);
    }

    #[test]
    fn boosts_and_cuts_the_right_band() {
        let flat = 0.1 / 2f32.sqrt();
        let mut eq = Equalizer::new(48_000, 2);
        let mut gains = [0.0; 10];
        gains[5] = -12.0; // cut 1 kHz
        eq.configure(EqSettings { enabled: true, gains, preamp_db: 0.0 });
        let cut = rms_after(&mut eq, 1000.0);
        assert!(cut < flat * 0.4, "1 kHz should be cut: {cut} vs {flat}");

        let mut eq = Equalizer::new(48_000, 2);
        let mut gains = [0.0; 10];
        gains[0] = 9.0; // boost bass
        eq.configure(EqSettings { enabled: true, gains, preamp_db: 0.0 });
        let treble = rms_after(&mut eq, 8000.0);
        // Far away from the boosted band only the headroom preamp applies.
        assert!((treble - flat * 10f32.powf(-9.0 / 20.0)).abs() < flat * 0.1, "{treble}");
    }
}
