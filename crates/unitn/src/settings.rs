use std::path::{Path, PathBuf};

#[cfg(not(target_os = "android"))]
use anyhow::anyhow;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Where Unitn keeps its files.
#[derive(Clone)]
pub struct Paths {
    pub log: PathBuf,
    pub database: PathBuf,
    pub covers: PathBuf,
    pub settings: PathBuf,
}

impl Paths {
    /// The user's data and config folders, as usual on each desktop OS.
    #[cfg(not(target_os = "android"))]
    pub fn new() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("net", "bokors", "Unitn")
            .ok_or_else(|| anyhow!("could not determine the user's data directory"))?;
        Self::from_dirs(dirs.data_dir().to_path_buf(), dirs.config_dir().to_path_buf())
    }

    /// Everything inside one folder (Android: the app's private storage).
    #[cfg(target_os = "android")]
    pub fn in_dir(dir: &Path) -> Result<Self> {
        Self::from_dirs(dir.to_path_buf(), dir.to_path_buf())
    }

    fn from_dirs(data: PathBuf, config: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&data)?;
        std::fs::create_dir_all(&config)?;
        Ok(Self {
            log: data.join("unitn.log"),
            database: data.join("library.sqlite3"),
            covers: data.join("covers"),
            settings: config.join("settings.json"),
        })
    }
}

/// The OS music folder, which a new library starts with.
#[cfg(not(target_os = "android"))]
pub fn default_music_folder() -> Option<PathBuf> {
    directories::UserDirs::new().and_then(|d| d.audio_dir().map(Path::to_path_buf))
}

/// Android: reading the shared Music folder needs a storage permission,
/// which the test build does not ask for yet.
#[cfg(target_os = "android")]
pub fn default_music_folder() -> Option<PathBuf> {
    None
}

/// Theme mode as used by the UI: follow the OS, or force light/dark.
pub const THEME_SYSTEM: i32 = 0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 0 = follow system, 1 = light, 2 = dark.
    pub theme: i32,
    pub volume: f32,
    pub shuffle: bool,
    /// 0 = off, 1 = repeat all, 2 = repeat one.
    pub repeat: i32,
    /// Window opacity, 0.5 (see-through) to 1.0 (solid).
    pub opacity: f32,
    /// Visualizer style: 0 Spectrum, 1 Mirror, 2 Wave, 3 Oscilloscope, 4 Radial.
    pub viz_style: i32,
    /// Look up missing album art on MusicBrainz / Cover Art Archive.
    pub download_art: bool,
    /// "Cozy" (compact) song lists.
    pub compact_lists: bool,
    pub eq_enabled: bool,
    /// Gain per band in dB (31 Hz … 16 kHz).
    pub eq_gains: [f32; 10],
    /// Selected preset, or -1 for custom.
    pub eq_preset: i32,
    pub eq_preamp: f32,
    /// The equalizer section is shown on Now Playing.
    pub eq_open: bool,
    /// Frosted-glass (blurred) window background, on macOS. Saved under a new
    /// name: 0.3.1 stored "glass" and its blur hid the whole window.
    #[serde(rename = "frosted_glass")]
    pub glass: bool,
    /// Connected music servers.
    pub servers: Vec<unitn_core::servers::ServerConfig>,
    /// Crossfade between songs in seconds (0 = off, gapless).
    pub crossfade: i32,
    /// Don't crossfade between songs of the same album.
    pub gapless_albums: bool,
    /// Look up lyrics on LRCLIB when there are none locally.
    pub lyrics_online: bool,
    /// The lyrics panel is shown on Now Playing.
    pub lyrics_open: bool,
    /// Play the sound of the startup animation.
    pub startup_sound: bool,
    /// Window size (logical pixels) when the app was last closed.
    pub window_width: Option<f32>,
    pub window_height: Option<f32>,
    pub window_maximized: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { theme: THEME_SYSTEM, volume: 0.8, shuffle: false, repeat: 0, opacity: 1.0, viz_style: 0, download_art: true, compact_lists: false, eq_enabled: false, eq_gains: [0.0; 10], eq_preset: 0, eq_preamp: 0.0, eq_open: false, glass: false, servers: Vec::new(), crossfade: 0, gapless_albums: false, lyrics_online: true, lyrics_open: false, startup_sound: true, window_width: None, window_height: None, window_maximized: false }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            if let Err(e) = std::fs::write(path, json) {
                eprintln!("unitn: could not save settings: {e}");
            }
        }
    }
}
