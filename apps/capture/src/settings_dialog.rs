//! Settings as a card over the window.

use crate::settings_page;
use crate::widgets;
use crate::window;
use gtk::prelude::*;
use nyx_ui::settings_card::{self, About, Spec, Standard};

pub use nyx_ui::settings_card::fit;

const GROUPS: &[(&str, &str, &str)] = &[
    ("Capture", "camera-photo-symbolic", "Print key, shot card, save folder"),
    ("History", "document-open-recent-symbolic", "Keep, search, and forget captures"),
    ("OCR", "edit-find-symbolic", "Tesseract and RapidOCR"),
    ("Privacy", "channel-secure-symbolic", "Redaction and Nyx Assistant"),
    ("Recording", "media-record-symbolic", "Recorder, audio, format, webcam"),
];

const SPEC: Spec = Spec {
    groups: GROUPS,
    search_text: None,
    standard: Some(Standard {
        app: "capture",
        about: About {
            name: "Capture",
            icon: "io.github.design_nexus.nyx.Capture",
            version: env!("CARGO_PKG_VERSION"),
            description: "Screenshots, annotation, text recognition and screen recording.",
            licence: "MIT",
            website: "https://github.com/design-nexus/nyx-capture",
            issues: "https://github.com/design-nexus/nyx-capture/issues",
            credits: &[("Tesseract", "text recognition")],
        },
        shortcuts: window::SHORTCUTS,
        reduce_motion: || crate::prefs::get().reduce_motion,
        set_reduce_motion: |on| crate::prefs::update(|p| p.reduce_motion = on),
    }),
};

pub fn open() {
    let (Some(overlay), Some(window)) = (window::overlay(), window::window()) else { return };
    settings_card::open(&overlay, &window, SPEC, || {
        let page = widgets::page("settings");
        settings_page::build(&page);
        (page.root.upcast(), page.body)
    });
}
