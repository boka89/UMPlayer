//! Channel mapping and sample-rate conversion to the output device format.
//!
//! Linear interpolation keeps this dependency-free and cheap; a windowed-sinc
//! resampler can replace it later without touching the rest of the engine.

/// Maps interleaved `src`-channel audio to `dst` channels.
pub fn map_channels(input: &[f32], src: usize, dst: usize) -> Vec<f32> {
    if src == dst || src == 0 {
        return input.to_vec();
    }
    let frames = input.len() / src;
    let mut out = Vec::with_capacity(frames * dst);
    for f in 0..frames {
        let fr = &input[f * src..f * src + src];
        for c in 0..dst {
            let v = if src == 1 {
                fr[0]
            } else if dst == 1 {
                (fr[0] + fr[1]) * 0.5
            } else if c < src {
                fr[c]
            } else {
                0.0
            };
            out.push(v);
        }
    }
    out
}

/// Stateful linear resampler for interleaved audio with a fixed channel count.
pub struct Resampler {
    channels: usize,
    dst_rate: u32,
    src_rate: u32,
    pos: f64,
    prev: Vec<f32>,
}

impl Resampler {
    pub fn new(channels: usize, dst_rate: u32) -> Self {
        Self { channels, dst_rate, src_rate: dst_rate, pos: 0.0, prev: Vec::new() }
    }

    /// Forgets buffered state (after seeking or loading a new source).
    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.prev.clear();
    }

    pub fn process(&mut self, input: Vec<f32>, src_rate: u32) -> Vec<f32> {
        if src_rate != self.src_rate {
            self.src_rate = src_rate;
            self.reset();
        }
        if src_rate == self.dst_rate || src_rate == 0 {
            return input;
        }
        let ch = self.channels;
        let step = src_rate as f64 / self.dst_rate as f64;
        let mut data = std::mem::take(&mut self.prev);
        data.extend_from_slice(&input);
        let n = data.len() / ch;
        if n < 2 {
            self.prev = data;
            return Vec::new();
        }
        let mut out = Vec::with_capacity(((n as f64 / step) as usize + 1) * ch);
        let mut t = self.pos;
        while t < (n - 1) as f64 {
            let i = t as usize;
            let frac = (t - i as f64) as f32;
            for c in 0..ch {
                let a = data[i * ch + c];
                let b = data[(i + 1) * ch + c];
                out.push(a + (b - a) * frac);
            }
            t += step;
        }
        self.pos = t - (n - 1) as f64;
        self.prev = data[(n - 1) * ch..n * ch].to_vec();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_mono_to_stereo_and_back() {
        assert_eq!(map_channels(&[1.0, 2.0], 1, 2), vec![1.0, 1.0, 2.0, 2.0]);
        assert_eq!(map_channels(&[1.0, 3.0], 2, 1), vec![2.0]);
        assert_eq!(map_channels(&[1.0, 2.0], 2, 4), vec![1.0, 2.0, 0.0, 0.0]);
    }

    #[test]
    fn resamples_with_correct_length() {
        let mut r = Resampler::new(1, 48_000);
        let mut total = 0;
        // 1 second of 44.1 kHz audio in uneven chunks.
        let mut remaining = 44_100;
        while remaining > 0 {
            let n = remaining.min(1_000);
            total += r.process(vec![0.5; n], 44_100).len();
            remaining -= n;
        }
        assert!((total as i64 - 48_000).abs() <= 2, "got {total}");
    }

    #[test]
    fn passthrough_when_rates_match() {
        let mut r = Resampler::new(2, 44_100);
        assert_eq!(r.process(vec![0.1, 0.2], 44_100), vec![0.1, 0.2]);
    }
}
