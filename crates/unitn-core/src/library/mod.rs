//! Local music library: folder scanning, tag reading and the SQLite index.

mod db;
pub mod m3u;
pub mod scanner;

pub use db::Library;
