//! Where a saved shot goes: the Save folder and File name settings, read each time.

use crate::paths;
use crate::prefs;
use gtk::glib;
use nyx_capture_core::action;
use std::path::PathBuf;

/// The Save folder from settings, else the default.
pub fn save_dir() -> PathBuf {
    let dir = prefs::get().save_dir;
    if dir.is_empty() { action::default_save_dir() } else { paths::expand(&dir) }
}

/// An error from a capture: on stderr and in `~/.cache/nyx-capture/capture.log`, since
/// a key binding has no terminal to show it.
pub fn log_error(what: &str, err: &dyn std::fmt::Display) {
    eprintln!("nyx-capture: {what}: {err}");
    log_line(what, err);
}

fn log_line(what: &str, err: &dyn std::fmt::Display) {
    let stamp = glib::DateTime::now_local().ok().and_then(|t| t.format("%F %T").ok()).map(|s| s.to_string()).unwrap_or_default();
    let line = format!("{stamp} {what}: {err}\n");
    let path = paths::cache_dir().join("capture.log");
    let _ = std::fs::create_dir_all(paths::cache_dir());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = std::io::Write::write_all(&mut f, line.as_bytes());
    }
}

/// A free path in the Save folder named by the File name setting. `app` is the
/// window class of what was captured (empty for a screen or a file).
pub fn next_path(app: &str) -> PathBuf {
    let now = glib::DateTime::now_local().ok();
    let fmt = |f: &str| now.as_ref().and_then(|t| t.format(f).ok()).map(|s| s.to_string()).unwrap_or_default();
    let pattern = prefs::get().file_name;
    let pattern = if pattern.trim().is_empty() { "screenshot-{date}_{time}-{app}".to_string() } else { pattern };
    let stem = action::file_stem(&pattern, &fmt("%Y-%m-%d"), &fmt("%H-%M-%S"), &action::app_slug(app));
    action::unique_png(&save_dir(), &stem)
}
