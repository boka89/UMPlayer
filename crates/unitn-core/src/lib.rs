//! Core of Unitn: the audio engine, the local music library and
//! internet radio support. The UI crate (`unitn`) only talks to this API,
//! so other front-ends (CLI, mobile) can reuse it later.

pub mod artwork;
pub mod audio;
pub mod library;
pub mod log;
pub mod model;
pub mod radio;
pub mod servers;
pub mod util;

/// User-Agent sent with every HTTP request (Radio Browser asks for a descriptive one).
pub const USER_AGENT: &str = concat!(
    "Unitn/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/boka89/Unitn)"
);
