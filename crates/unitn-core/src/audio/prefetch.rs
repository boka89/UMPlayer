//! Read-ahead for local files.
//!
//! Files on network shares or slow/sleeping disks can stall for seconds. A
//! background thread reads the whole file into memory as fast as the storage
//! allows (typically within a few seconds, far ahead of playback), and the
//! decoder reads from that memory. Reads only wait when the decoder gets ahead
//! of the download, e.g. right after seeking far into a file still loading.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use symphonia::core::io::MediaSource;

/// Larger files are read directly (to keep memory use reasonable).
pub const MAX_PREFETCH: u64 = 768 * 1024 * 1024;
const CHUNK: usize = 256 * 1024;

#[derive(Default)]
struct State {
    data: Vec<u8>,
    done: bool,
    error: Option<String>,
}

struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}

pub struct PrefetchReader {
    shared: Arc<Shared>,
    pos: u64,
    /// Total size, if known (unknown sizes can't be seeked from the end).
    len: Option<u64>,
}

impl PrefetchReader {
    /// Starts loading `path` in the background.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Self::from_reader(file, Some(len))
    }

    /// Starts loading everything `reader` delivers (e.g. an HTTP download).
    pub fn from_reader(mut file: impl Read + Send + 'static, len: Option<u64>) -> io::Result<Self> {
        let capacity = len.unwrap_or(0).min(MAX_PREFETCH) as usize;
        let shared = Arc::new(Shared {
            state: Mutex::new(State { data: Vec::with_capacity(capacity), ..Default::default() }),
            ready: Condvar::new(),
        });
        let loader = Arc::downgrade(&shared);
        std::thread::Builder::new().name("unitn-prefetch".into()).spawn(move || {
            let mut chunk = vec![0u8; CHUNK];
            loop {
                let result = file.read(&mut chunk);
                // Stop early if the track was skipped (reader dropped).
                let Some(shared) = loader.upgrade() else { return };
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                match result {
                    Ok(0) => state.done = true,
                    Ok(n) => state.data.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        state.error = Some(e.to_string());
                        state.done = true;
                    }
                }
                let finished = state.done;
                drop(state);
                shared.ready.notify_all();
                if finished {
                    return;
                }
            }
        })?;
        Ok(Self { shared, pos: 0, len })
    }

    /// Bytes loaded so far (for diagnostics and tests).
    #[cfg(test)]
    pub fn loaded(&self) -> usize {
        self.shared.state.lock().map(|s| s.data.len()).unwrap_or(0)
    }
}

impl Read for PrefetchReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        while (state.data.len() as u64) <= self.pos && !state.done {
            state = self.shared.ready.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        let available = state.data.len() as u64;
        if self.pos >= available {
            return match &state.error {
                Some(e) => Err(io::Error::new(io::ErrorKind::Other, e.clone())),
                None => Ok(0),
            };
        }
        let start = self.pos as usize;
        let n = buf.len().min(state.data.len() - start);
        buf[..n].copy_from_slice(&state.data[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for PrefetchReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => match self.len {
                Some(len) => len as i128 + d as i128,
                None => return Err(io::Error::new(io::ErrorKind::Unsupported, "size unknown")),
            },
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start of file"));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

impl MediaSource for PrefetchReader {
    fn is_seekable(&self) -> bool {
        self.len.is_some()
    }

    fn byte_len(&self) -> Option<u64> {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_seeks_like_the_file() {
        let path = std::env::temp_dir().join(format!("unitn-prefetch-{}.bin", std::process::id()));
        let data: Vec<u8> = (0..1_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let mut r = PrefetchReader::open(&path).unwrap();
        let mut all = Vec::new();
        r.read_to_end(&mut all).unwrap();
        assert_eq!(all, data);
        assert_eq!(r.loaded(), data.len());

        r.seek(SeekFrom::Start(123_456)).unwrap();
        let mut buf = [0u8; 10];
        r.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, &data[123_456..123_466]);

        r.seek(SeekFrom::End(-5)).unwrap();
        let mut tail = Vec::new();
        r.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, &data[data.len() - 5..]);
        let _ = std::fs::remove_file(path);
    }
}
