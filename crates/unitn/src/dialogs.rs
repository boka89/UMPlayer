//! Native message boxes and file dialogs (the `rfd` crate) on the desktop.
//! Android has no counterpart yet, so there they only write to the log.

use std::path::PathBuf;

#[cfg(not(target_os = "android"))]
mod imp {
    use super::PathBuf;

    pub fn error_box(title: &str, text: &str) {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title(title)
            .set_description(text)
            .show();
    }

    pub fn pick_cover_image() -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Choose a cover image")
            .add_filter("Images", &["jpg", "jpeg", "png"])
            .pick_file()
    }

    pub fn save_playlist(file_name: &str) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Export playlist")
            .set_file_name(file_name)
            .add_filter("M3U playlist", &["m3u", "m3u8"])
            .save_file()
    }

    pub fn pick_playlist() -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Import playlist")
            .add_filter("M3U playlist", &["m3u", "m3u8"])
            .pick_file()
    }

    pub fn pick_music_folder() -> Option<PathBuf> {
        rfd::FileDialog::new().set_title("Add music folder").pick_folder()
    }
}

#[cfg(target_os = "android")]
mod imp {
    use super::PathBuf;
    use unitn_core::log::log;

    fn unavailable(what: &str) -> Option<PathBuf> {
        log(format!("android: {what} is not available yet"));
        None
    }

    pub fn error_box(title: &str, text: &str) {
        log(format!("{title}: {text}"));
    }

    pub fn pick_cover_image() -> Option<PathBuf> {
        unavailable("choosing a cover image")
    }

    pub fn save_playlist(_file_name: &str) -> Option<PathBuf> {
        unavailable("exporting a playlist")
    }

    pub fn pick_playlist() -> Option<PathBuf> {
        unavailable("importing a playlist")
    }

    pub fn pick_music_folder() -> Option<PathBuf> {
        unavailable("adding a music folder")
    }
}

pub use imp::*;
