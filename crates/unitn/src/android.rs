//! Android entry point and the bridge to the Java side (experimental).
//!
//! `crates/unitn-android` builds the shared library that Android loads. It
//! forwards `android_main` to the one here and implements [`Platform`] with
//! JNI calls into `MainActivity` / `PlaybackService` (see its `java/` folder).

use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Result};

pub use slint::android::AndroidApp;

use crate::media::MediaCmd;
use crate::settings::Paths;

/// What the notification, the lock screen and Bluetooth devices show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Seconds; `None` for live radio.
    pub duration: Option<f64>,
    /// Seconds.
    pub position: f64,
    pub playing: bool,
}

/// The parts of Android that need Java. Implemented in `unitn-android`.
pub trait Platform: Send + Sync {
    /// Shows or updates the playback notification and media session, and
    /// keeps the app alive in the background. `None` removes them.
    fn playback(&self, now: Option<&NowPlaying>);
    /// Opens a web page in the browser.
    fn open_url(&self, url: &str) -> bool;
    /// Asks for access to the music files and for notifications, if missing.
    fn request_permissions(&self);
    /// Dark icons in the status and navigation bars (for the light theme).
    fn light_system_bars(&self, light: bool);
    /// Leaves the app open in the background (Back on the first page).
    fn move_to_back(&self);
}

static PLATFORM: OnceLock<Box<dyn Platform>> = OnceLock::new();
static MEDIA_TX: Mutex<Option<Sender<MediaCmd>>> = Mutex::new(None);
static PERMISSIONS_CHANGED: AtomicBool = AtomicBool::new(false);
static BACK_PRESSED: AtomicBool = AtomicBool::new(false);

pub(crate) fn platform() -> Option<&'static dyn Platform> {
    PLATFORM.get().map(|p| p.as_ref())
}

/// Media commands from the notification, headset buttons, etc. go here.
pub(crate) fn set_media_sender(tx: Sender<MediaCmd>) {
    if let Ok(mut slot) = MEDIA_TX.lock() {
        *slot = Some(tx);
    }
}

/// True once after the user answered a permission question.
pub(crate) fn take_permissions_changed() -> bool {
    PERMISSIONS_CHANGED.swap(false, Ordering::Relaxed)
}

/// True once after Android's Back button was pressed.
pub(crate) fn take_back_pressed() -> bool {
    BACK_PRESSED.swap(false, Ordering::Relaxed)
}

/// Events from the Java side (any thread). Codes as in `MainActivity.java`:
/// 0 play, 1 pause, 2 play/pause, 3 next, 4 previous, 5 stop,
/// 6 seek to `arg` milliseconds, 10 a permission question was answered,
/// 11 the Back button.
pub fn on_java_event(code: i32, arg: i64) {
    let cmd = match code {
        0 => MediaCmd::Play,
        1 => MediaCmd::Pause,
        2 => MediaCmd::Toggle,
        3 => MediaCmd::Next,
        4 => MediaCmd::Previous,
        5 => MediaCmd::Stop,
        6 => MediaCmd::SeekTo(arg as f64 / 1000.0),
        10 => {
            PERMISSIONS_CHANGED.store(true, Ordering::Relaxed);
            return;
        }
        11 => {
            BACK_PRESSED.store(true, Ordering::Relaxed);
            return;
        }
        _ => return,
    };
    if let Ok(slot) = MEDIA_TX.lock() {
        if let Some(tx) = slot.as_ref() {
            let _ = tx.send(cmd);
        }
    }
}

#[link(name = "log")]
extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

extern "C" {
    fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
}

const ANDROID_LOG_INFO: c_int = 4;
/// Longest value of an Android system property, including the final NUL.
const PROP_VALUE_MAX: usize = 92;

/// Writes to the Android system log under the tag "unitn"
/// (read it with `adb logcat -s unitn`).
pub fn logcat(message: &str) {
    // logcat cuts long entries, so write line by line (panic backtraces).
    for line in message.lines() {
        if let Ok(text) = CString::new(line.replace('\0', " ")) {
            unsafe {
                __android_log_write(ANDROID_LOG_INFO, c"unitn".as_ptr(), text.as_ptr());
            }
        }
    }
}

fn system_property(name: &CStr) -> Option<String> {
    let mut value = [0 as c_char; PROP_VALUE_MAX];
    let len = unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };
    if len <= 0 {
        return None;
    }
    let value = unsafe { CStr::from_ptr(value.as_ptr()) }.to_string_lossy().into_owned();
    (!value.is_empty()).then_some(value)
}

pub fn android_main(app: AndroidApp, platform: Box<dyn Platform>) {
    let _ = PLATFORM.set(platform);
    if let Err(e) = start(app) {
        logcat(&format!("fatal: {e:#}"));
    }
    if let Some(p) = crate::android::platform() {
        p.playback(None);
    }
    // Android may keep the process after the activity is closed and would
    // then call android_main again in it; always start from a fresh process.
    std::process::exit(0);
}

fn start(app: AndroidApp) -> Result<()> {
    // Apps can't be started with environment variables, so the test hooks
    // come from system properties, e.g.
    //   adb shell setprop debug.unitn.autoplay https://example.org/stream
    for (property, variable) in [(c"debug.unitn.autoplay", "UNITN_AUTOPLAY"), (c"debug.unitn.no_intro", "UNITN_NO_INTRO")] {
        if let Some(value) = system_property(property) {
            std::env::set_var(variable, value);
        }
    }
    let data = app
        .internal_data_path()
        .ok_or_else(|| anyhow!("Android did not report the app's data folder"))?;
    // The default temporary folder (/data/local/tmp) is not writable for apps.
    let tmp = data.join("tmp");
    std::fs::create_dir_all(&tmp)?;
    std::env::set_var("TMPDIR", &tmp);

    slint::android::init(app).map_err(|e| anyhow!("could not start the Android UI backend: {e:?}"))?;

    // Test hook: open a web page right away (checks the browser hand-over).
    if let (Some(url), Some(p)) = (system_property(c"debug.unitn.open_url"), platform()) {
        logcat(&format!("android: open_url test: {}", p.open_url(&url)));
    }
    crate::run(Paths::in_dir(&data)?)
}

/// Storage roots: the built-in one first, then SD cards and USB drives.
fn storage_roots() -> Vec<std::path::PathBuf> {
    let mut roots = vec![std::path::PathBuf::from("/storage/emulated/0")];
    if let Ok(entries) = std::fs::read_dir("/storage") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name != "emulated" && name != "self" && entry.path().is_dir() {
                roots.push(entry.path());
            }
        }
    }
    roots
}

/// Where music usually is on a phone: the Music and Download folders.
pub(crate) fn music_folders() -> Vec<std::path::PathBuf> {
    let mut folders = Vec::new();
    for (i, root) in storage_roots().into_iter().enumerate() {
        folders.push(root.join("Music"));
        if i == 0 {
            folders.push(root.join("Download"));
        }
    }
    folders
}
