//! This app's own preferences (`~/.config/nyx-capture/settings.toml`).

pub use nyx_theme::ThemeMode;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub mode: ThemeMode,
    pub theme: String,
    pub reduce_motion: bool,
    pub sidebar_collapsed: bool,
    pub glow: bool,
    pub last_section: String,
    pub after_capture: String,
    /// What the capture bar starts on: `smart`, `window` or `screen`.
    pub capture_start: String,
    /// Show the shot card after a copy-and-save capture.
    pub shot_card: bool,
    /// Seconds the card stays; 0 keeps it until closed.
    pub card_secs: u32,
    /// Saved file names: `{date}`, `{time}` and `{app}` are filled in.
    pub file_name: String,
    pub save_dir: String,
    pub history: bool,
    pub retention_days: i64,
    pub ocr_mode: String,
    pub ocr_langs: String,
    pub ocr_on_capture: bool,
    pub backend: String,
    pub selector: String,
    pub recorder: String,
    pub record_audio: String,
    pub record_cursor: bool,
    pub record_container: String,
    pub record_delay: u32,
    pub record_use_portal: bool,
    pub record_webcam: bool,
    pub record_webcam_size: String,
    pub record_webcam_device: String,
    pub assistant_can_send: bool,
    pub auto_redact: bool,
    pub tutorial_done: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Omarchy,
            theme: "tokyo-night".into(),
            reduce_motion: false,
            sidebar_collapsed: false,
            glow: true,
            last_section: "recent".into(),
            after_capture: "quick".into(),
            capture_start: "smart".into(),
            shot_card: true,
            card_secs: 7,
            file_name: "screenshot-{date}_{time}-{app}".into(),
            save_dir: String::new(),
            history: true,
            retention_days: 90,
            ocr_mode: "automatic".into(),
            ocr_langs: "eng".into(),
            ocr_on_capture: true,
            backend: "auto".into(),
            selector: "auto".into(),
            recorder: "auto".into(),
            record_audio: "desktop".into(),
            record_cursor: true,
            record_container: "mp4".into(),
            record_delay: 0,
            record_use_portal: false,
            record_webcam: false,
            record_webcam_size: "medium".into(),
            record_webcam_device: String::new(),
            assistant_can_send: false,
            auto_redact: false,
            tutorial_done: false,
        }
    }
}

nyx_core::prefs_store!(Prefs);
