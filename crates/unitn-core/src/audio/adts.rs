//! Robust ADTS (AAC) framing for internet radio.
//!
//! Symphonia's ADTS reader is strict: when joining a live stream mid-frame it
//! often locks onto a false sync word and gives up. This framer validates each
//! header (and the following one) before trusting it, resynchronises after
//! garbage, and splits packets that carry several raw data blocks.

use std::io::{self, Read};

use symphonia::core::audio::Channels;
use symphonia::core::codecs::{CodecParameters, CODEC_TYPE_AAC};
use symphonia::core::units::TimeBase;

const SAMPLE_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdtsHeader {
    /// MPEG-4 audio object type (2 = AAC LC).
    pub object_type: u8,
    pub rate_index: u8,
    pub channel_config: u8,
    /// Header size: 7 bytes, or 9 with CRC.
    pub header_len: usize,
    /// Whole frame including header.
    pub frame_len: usize,
    /// Raw data blocks in this frame (1..=4).
    pub blocks: usize,
}

impl AdtsHeader {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 7 || b[0] != 0xFF || (b[1] & 0xF6) != 0xF0 {
            return None; // sync 0xFFF and layer 00
        }
        let protection_absent = b[1] & 0x01 == 1;
        let profile = (b[2] >> 6) & 0x03;
        let rate_index = (b[2] >> 2) & 0x0F;
        let channel_config = ((b[2] & 0x01) << 2) | (b[3] >> 6);
        let frame_len = (((b[3] & 0x03) as usize) << 11) | ((b[4] as usize) << 3) | ((b[5] >> 5) as usize);
        let blocks = (b[6] & 0x03) as usize + 1;
        let header_len = if protection_absent { 7 } else { 9 };
        if rate_index as usize >= SAMPLE_RATES.len() || profile == 3 || frame_len <= header_len {
            return None;
        }
        Some(Self { object_type: profile + 1, rate_index, channel_config, header_len, frame_len, blocks })
    }

    pub fn sample_rate(&self) -> u32 {
        SAMPLE_RATES[self.rate_index as usize]
    }

    /// Same stream parameters (used to reject false syncs).
    fn compatible(&self, other: &Self) -> bool {
        self.object_type == other.object_type
            && self.rate_index == other.rate_index
            && self.channel_config == other.channel_config
    }

    /// Codec parameters for symphonia's AAC decoder, including the
    /// AudioSpecificConfig it expects as extra data.
    pub fn codec_params(&self) -> CodecParameters {
        let asc = vec![
            (self.object_type << 3) | (self.rate_index >> 1),
            ((self.rate_index & 1) << 7) | (self.channel_config << 3),
        ];
        let channels = if self.channel_config == 1 {
            Channels::FRONT_CENTRE
        } else {
            Channels::FRONT_LEFT | Channels::FRONT_RIGHT
        };
        let mut params = CodecParameters::new();
        params
            .for_codec(CODEC_TYPE_AAC)
            .with_sample_rate(self.sample_rate())
            .with_channels(channels)
            .with_time_base(TimeBase::new(1, self.sample_rate()))
            .with_extra_data(asc.into_boxed_slice());
        params
    }
}

pub struct AdtsFramer<R: Read> {
    reader: R,
    buf: Vec<u8>,
    start: usize,
    eof: bool,
    /// Parameters of the stream once locked.
    locked: Option<AdtsHeader>,
}

impl<R: Read> AdtsFramer<R> {
    pub fn new(reader: R) -> Self {
        Self { reader, buf: Vec::with_capacity(16 * 1024), start: 0, eof: false, locked: None }
    }

    /// Stream parameters, once [`lock`](Self::lock) succeeded.
    pub fn header(&self) -> Option<AdtsHeader> {
        self.locked
    }

    fn available(&self) -> usize {
        self.buf.len() - self.start
    }

    /// Makes sure at least `n` bytes are buffered (unless the stream ended).
    fn fill(&mut self, n: usize) -> io::Result<bool> {
        if self.start > 64 * 1024 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        while self.available() < n && !self.eof {
            let mut chunk = [0u8; 8192];
            let read = self.reader.read(&mut chunk)?;
            if read == 0 {
                self.eof = true;
            } else {
                self.buf.extend_from_slice(&chunk[..read]);
            }
        }
        Ok(self.available() >= n)
    }

    /// Reads until the first trustworthy header and returns its parameters.
    pub fn lock(&mut self) -> io::Result<AdtsHeader> {
        loop {
            if !self.fill(9)? {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "no AAC frames found"));
            }
            let Some(h) = AdtsHeader::parse(&self.buf[self.start..]) else {
                self.start += 1;
                continue;
            };
            // Trust a header only if another compatible one follows it.
            if !self.fill(h.frame_len + 9)? {
                self.start += 1; // stream too short for this "frame": a false sync
                continue;
            }
            let next = AdtsHeader::parse(&self.buf[self.start + h.frame_len..]);
            if next.map(|n| n.compatible(&h)).unwrap_or(false) {
                self.locked = Some(h);
                return Ok(h);
            }
            self.start += 1;
        }
    }

    /// Returns the payloads (raw data blocks) of the next frame, or `None` at
    /// the end of the stream.
    pub fn next_frame(&mut self) -> io::Result<Option<Vec<Vec<u8>>>> {
        let locked = match self.locked {
            Some(h) => h,
            None => self.lock()?,
        };
        loop {
            if !self.fill(9)? {
                return Ok(None);
            }
            let header = AdtsHeader::parse(&self.buf[self.start..]).filter(|h| h.compatible(&locked));
            let Some(h) = header else {
                self.start += 1; // lost sync: scan forward
                continue;
            };
            if !self.fill(h.frame_len)? {
                return Ok(None);
            }
            let frame = &self.buf[self.start..self.start + h.frame_len];
            let payload = &frame[h.header_len..];
            let blocks = split_blocks(payload, &h);
            self.start += h.frame_len;
            return Ok(Some(blocks));
        }
    }
}

/// Splits a frame payload into raw data blocks. With CRC the header carries
/// the block positions; without it we can only split evenly-sized blocks,
/// which is how encoders write them in practice.
fn split_blocks(payload: &[u8], h: &AdtsHeader) -> Vec<Vec<u8>> {
    if h.blocks <= 1 {
        return vec![payload.to_vec()];
    }
    let size = payload.len() / h.blocks;
    if size == 0 {
        return vec![payload.to_vec()];
    }
    (0..h.blocks)
        .map(|i| {
            let end = if i + 1 == h.blocks { payload.len() } else { (i + 1) * size };
            payload[i * size..end].to_vec()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ADTS frame (no CRC) with a fake payload.
    fn frame(payload_len: usize, fill: u8) -> Vec<u8> {
        let len = payload_len + 7;
        let (profile, rate, channels) = (1u8, 4u8, 2u8); // LC, 44.1 kHz, stereo
        let mut f = vec![
            0xFF,
            0xF1,
            (profile << 6) | (rate << 2) | (channels >> 2),
            ((channels & 3) << 6) | ((len >> 11) as u8 & 0x03),
            (len >> 3) as u8,
            (((len & 7) as u8) << 5) | 0x1F,
            0xFC,
        ];
        f.extend(std::iter::repeat(fill).take(payload_len));
        f
    }

    #[test]
    fn parses_headers() {
        let h = AdtsHeader::parse(&frame(100, 0)).unwrap();
        assert_eq!(h.object_type, 2);
        assert_eq!(h.sample_rate(), 44100);
        assert_eq!(h.channel_config, 2);
        assert_eq!(h.frame_len, 107);
        assert_eq!(h.blocks, 1);
        let params = h.codec_params();
        assert_eq!(params.extra_data.as_deref(), Some(&[0x12, 0x10][..]));
    }

    #[test]
    fn resyncs_after_garbage_and_false_syncs() {
        // Join mid-stream: garbage containing fake sync words, then real frames.
        let mut data = vec![0x00, 0xFF, 0xF1, 0x00, 0x13, 0xFF, 0xF9, 0xAA, 0x55, 0x01];
        for i in 0..5 {
            data.extend(frame(50 + i, i as u8 + 1));
        }
        let mut framer = AdtsFramer::new(io::Cursor::new(data));
        let header = framer.lock().unwrap();
        assert_eq!(header.sample_rate(), 44100);
        let mut payloads = Vec::new();
        while let Some(blocks) = framer.next_frame().unwrap() {
            payloads.extend(blocks);
        }
        assert_eq!(payloads.len(), 5);
        assert_eq!(payloads[0], vec![1u8; 50]);
        assert_eq!(payloads[4], vec![5u8; 54]);
    }

    #[test]
    fn rejects_streams_without_aac() {
        let mut framer = AdtsFramer::new(io::Cursor::new(vec![0x42u8; 4096]));
        assert!(framer.lock().is_err());
    }
}
