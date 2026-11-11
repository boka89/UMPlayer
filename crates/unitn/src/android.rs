//! Android entry point (experimental). `crates/unitn-android` builds the
//! shared library that Android's NativeActivity loads and forwards its
//! `android_main` to the one here.

use std::ffi::{c_char, c_int, CStr, CString};

use anyhow::{anyhow, Result};

pub use slint::android::AndroidApp;

use crate::settings::Paths;

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

pub fn android_main(app: AndroidApp) {
    if let Err(e) = start(app) {
        logcat(&format!("fatal: {e:#}"));
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
    crate::run(Paths::in_dir(&data)?)
}
