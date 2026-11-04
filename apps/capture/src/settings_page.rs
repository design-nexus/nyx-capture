//! The settings card's groups.

use crate::prefs;
use crate::widgets::{self, Page};
use crate::window;
use gtk::gio;
use gtk::prelude::*;
use nyx_capture_core::action;
use nyx_capture_core::detect::Tools;
use std::path::Path;

pub fn build(page: &Page) {
    capture_group(page);
    history_group(page);
    privacy_group(page);
    ocr_group(page);
    record_group(page);
}

fn capture_group(page: &Page) {
    let group = page.group("Capture");
    let prefs = prefs::get();
    let tools = Tools::probe();
    group.add(&widgets::segmented_row(
        "Start with",
        "What the capture bar has chosen when the Print key opens it.",
        widgets::opts(&[("smart", "Smart"), ("window", "Window"), ("screen", "Screen")]),
        &prefs.capture_start,
        |v| prefs::update(|p| p.capture_start = v),
    ));
    let mut after = vec![("edit", "Edit"), ("pin", "Pin")];
    if tools.wl_copy {
        after.insert(0, ("copy", "Copy"));
        after.insert(0, ("quick", "Copy and save"));
    } else {
        after.insert(0, ("save", "Save"));
    }
    group.add(&widgets::segmented_row(
        "After a screenshot",
        "What happens once you've chosen.",
        widgets::opts(&after),
        &prefs.after_capture,
        |v| prefs::update(|p| p.after_capture = v),
    ));
    let (row, _) = widgets::switch_row(
        "Shot card",
        "Show the shot in the corner with Edit, Copy and Keep.",
        prefs.shot_card,
        |on| prefs::update(|p| p.shot_card = on),
    );
    group.add(&row);
    group.add(&widgets::segmented_row(
        "Card stays",
        "Pointing at the card always stops the clock.",
        widgets::opts(&[("4", "4 s"), ("7", "7 s"), ("15", "15 s"), ("0", "Until closed")]),
        &prefs.card_secs.to_string(),
        |v| {
            if let Ok(n) = v.parse() {
                prefs::update(|p| p.card_secs = n);
            }
        },
    ));
    let save_label = save_dir_label(&prefs.save_dir);
    let (row, _) = widgets::button_row(
        "Save folder",
        "Where shots are saved. Read each time; no restart.",
        &save_label,
        |btn| choose_save_dir(btn.clone()),
    );
    group.add(&row);
    let name = gtk::Entry::new();
    name.set_text(&prefs.file_name);
    name.set_width_chars(28);
    name.set_placeholder_text(Some("screenshot-{date}_{time}-{app}"));
    let save_name = |e: &gtk::Entry| {
        let text = e.text().trim().to_string();
        prefs::update(|p| p.file_name = text);
    };
    name.connect_activate(save_name);
    let focus = gtk::EventControllerFocus::new();
    let entry = name.clone();
    focus.connect_leave(move |_| save_name(&entry));
    name.add_controller(focus);
    group.add(&widgets::row("File name", "{date}, {time} and {app} are filled in.", Some(name.upcast_ref())));
    let mut selector = vec![("auto", "Auto")];
    if crate::overlay::supported() {
        selector.push(("overlay", "Overlay"));
    }
    if tools.slurp {
        selector.push(("slurp", "slurp"));
    }
    if selector.len() > 1 {
        group.add(&widgets::segmented_row(
            "Selector",
            "The GTK overlay on Hyprland, or slurp when layer-shell is missing.",
            widgets::opts(&selector),
            &prefs.selector,
            |v| prefs::update(|p| p.selector = v),
        ));
    }
    let mut backends = vec![("auto", "Auto")];
    if tools.grim {
        backends.push(("grim", "grim"));
    }
    if tools.portal {
        backends.push(("portal", "Portal"));
    }
    if backends.len() > 1 {
        group.add(&widgets::segmented_row(
            "Backend",
            "grim on Hyprland. The XDG screenshot portal when grim is missing.",
            widgets::opts(&backends),
            &prefs.backend,
            |v| prefs::update(|p| p.backend = v),
        ));
    }
}

fn save_dir_label(dir: &str) -> String {
    if dir.is_empty() {
        return "Pictures / Nyx Capture".into();
    }
    crate::paths::pretty(Path::new(dir))
}

fn choose_save_dir(btn: gtk::Button) {
    let dialog = gtk::FileDialog::builder().title("Save screenshots here").modal(true).build();
    let start = {
        let dir = prefs::get().save_dir;
        if dir.is_empty() { action::default_save_dir() } else { Path::new(&dir).to_path_buf() }
    };
    if start.is_dir() {
        dialog.set_initial_folder(Some(&gio::File::for_path(start)));
    }
    dialog.select_folder(window::window().as_ref(), gio::Cancellable::NONE, move |res| {
        let Ok(file) = res else { return };
        let Some(path) = file.path() else { return };
        let text = path.to_string_lossy().into_owned();
        btn.set_label(&save_dir_label(&text));
        prefs::update(|p| p.save_dir = text);
    });
}

fn history_group(page: &Page) {
    let group = page.group("History");
    let prefs = prefs::get();
    let (row, _) = widgets::switch_row("Keep a history", "Store captures on this computer so they can be searched.", prefs.history, |on| {
        prefs::update(|p| p.history = on);
        window::reload();
    });
    group.add(&row);
    group.add(&widgets::segmented_row(
        "Keep for",
        "Unstarred captures older than this are removed on launch. Starred ones stay.",
        widgets::opts(&[("7", "7 days"), ("30", "30 days"), ("90", "90 days"), ("365", "1 year"), ("0", "Forever")]),
        &prefs.retention_days.to_string(),
        |v| {
            if let Ok(n) = v.parse() {
                prefs::update(|p| p.retention_days = n);
            }
        },
    ));
}

fn privacy_group(page: &Page) {
    let group = page.group("Privacy");
    let prefs = prefs::get();
    let (row, _) = widgets::switch_row(
        "Assistant can send a screenshot",
        "Let Nyx Assistant capture the screen or read OCR from history while Capture is open. Off by default.",
        prefs.assistant_can_send,
        |on| prefs::update(|p| p.assistant_can_send = on),
    );
    group.add(&row);
    let (row, _) = widgets::switch_row(
        "Redact secrets after OCR",
        "Cover emails, URLs, phone numbers, cards, and keys found in the text with solid boxes, and leave the shot out of search. Stays on this computer.",
        prefs.auto_redact,
        |on| prefs::update(|p| p.auto_redact = on),
    );
    group.add(&row);
    let tutorial = widgets::icon_button("help-browser-symbolic", "Show the walkthrough");
    tutorial.connect_clicked(|_| {
        prefs::update(|p| p.tutorial_done = false);
        window::show_tutorial();
    });
    group.add(&widgets::row("Tutorial", "Show the three-step capture walkthrough again.", Some(tutorial.upcast_ref())));
}

fn ocr_group(page: &Page) {
    let group = page.group("OCR");
    let prefs = prefs::get();
    group.add(&widgets::segmented_row(
        "Engine",
        "Automatic uses Tesseract first, then RapidOCR when the text looks unreliable.",
        widgets::opts(&[("automatic", "Automatic"), ("tesseract", "Tesseract"), ("rapidocr", "RapidOCR")]),
        &prefs.ocr_mode,
        |v| prefs::update(|p| p.ocr_mode = v),
    ));
    let (row, _) = widgets::switch_row("Read text after capture", "Run OCR in the background and index it for search.", prefs.ocr_on_capture, |on| {
        prefs::update(|p| p.ocr_on_capture = on);
    });
    group.add(&row);
    // Tesseract's installed languages (more come as tesseract-data-* packages).
    let mut langs = nyx_capture_core::ocr::tesseract_languages();
    if !langs.contains(&prefs.ocr_langs) {
        langs.insert(0, prefs.ocr_langs.clone());
    }
    if langs.len() > 1 {
        let opts = langs.iter().map(|l| (l.clone(), l.clone())).collect();
        let (row, _) = widgets::choice_row(
            "Language",
            "The language Tesseract reads. Install others as <tt>tesseract-data-*</tt> packages.",
            opts,
            &prefs.ocr_langs,
            |v| prefs::update(|p| p.ocr_langs = v),
        );
        group.add(&row);
    }
    let present = nyx_capture_core::ocr::models_present();
    let get = widgets::icon_button("folder-download-symbolic", "Download the RapidOCR models");
    get.set_visible(!present);
    let state = widgets::label(if present { "Installed" } else { "Not downloaded" }, "dim");
    let right = widgets::hbox(8);
    right.append(&state);
    right.append(&get);
    get.connect_clicked(move |b| {
        b.set_sensitive(false);
        state.set_text("Downloading…");
        let (b, state) = (b.clone(), state.clone());
        crate::cmd::background(nyx_capture_core::ocr::download_models, move |result| match result {
            Ok(()) => {
                state.set_text("Installed");
                b.set_visible(false);
            }
            Err(err) => {
                state.set_text(&err.to_string());
                b.set_sensitive(true);
            }
        });
    });
    group.add(&widgets::row(
        "RapidOCR models",
        "Automatic falls back to RapidOCR on hard-to-read text once its models are here (about 15 MB).",
        Some(right.upcast_ref()),
    ));
}

fn record_group(page: &Page) {
    let tools = Tools::probe();
    if !tools.can_record() {
        return;
    }
    let group = page.group("Recording");
    let prefs = prefs::get();
    let mut rec = vec![("auto", "Auto")];
    if tools.gpu_screen_recorder {
        rec.push(("gpu-screen-recorder", "GPU Screen Recorder"));
    }
    if tools.wf_recorder {
        rec.push(("wf-recorder", "wf-recorder"));
    }
    if tools.wl_screenrec {
        rec.push(("wl-screenrec", "wl-screenrec"));
    }
    if tools.can_gst_record() {
        rec.push(("pipewire", "PipeWire portal"));
    }
    if rec.len() > 1 {
        group.add(&widgets::segmented_row(
            "Recorder",
            "gpu-screen-recorder first, then wf-recorder, then wl-screenrec, then PipeWire via the ScreenCast portal.",
            widgets::opts(&rec),
            &prefs.recorder,
            |v| prefs::update(|p| p.recorder = v),
        ));
    }
    group.add(&widgets::segmented_row(
        "Audio",
        "Tracks mixed into the file. Off records video only.",
        widgets::opts(&[("none", "Off"), ("desktop", "System"), ("mic", "Mic"), ("both", "Both")]),
        &prefs.record_audio,
        |v| prefs::update(|p| p.record_audio = v),
    ));
    group.add(&widgets::segmented_row(
        "Format",
        "GIF is converted with ffmpeg after the recording stops.",
        widgets::opts(&[("mp4", "MP4"), ("webm", "WebM"), ("gif", "GIF")]),
        &prefs.record_container,
        |v| prefs::update(|p| p.record_container = v),
    ));
    group.add(&widgets::segmented_row(
        "Countdown",
        "Seconds on screen before the recorder starts.",
        widgets::opts(&[("0", "Off"), ("3", "3 s"), ("5", "5 s")]),
        &prefs.record_delay.to_string(),
        |v| {
            if let Ok(n) = v.parse() {
                prefs::update(|p| p.record_delay = n);
            }
        },
    ));
    let (row, _) = widgets::switch_row("Cursor", "Include the pointer in the recording.", prefs.record_cursor, |on| {
        prefs::update(|p| p.record_cursor = on);
    });
    group.add(&row);
    if tools.gpu_screen_recorder {
        let (row, _) = widgets::switch_row(
            "Use the ScreenCast portal",
            "gpu-screen-recorder -w portal. Off uses the focused monitor or a dragged region.",
            prefs.record_use_portal,
            |on| prefs::update(|p| p.record_use_portal = on),
        );
        group.add(&row);
    }
    if tools.can_webcam() {
        let (row, _) = widgets::switch_row(
            "Webcam overlay",
            "mpv picture-in-picture using Omarchy WebcamOverlay window rules.",
            prefs.record_webcam,
            |on| prefs::update(|p| p.record_webcam = on),
        );
        group.add(&row);
        group.add(&widgets::segmented_row(
            "Webcam size",
            "Hyprland places the overlay in the corner. SUPER+ALT [ ] resizes it.",
            widgets::opts(&[("small", "Small"), ("medium", "Medium"), ("large", "Large")]),
            &prefs.record_webcam_size,
            |v| prefs::update(|p| p.record_webcam_size = v),
        ));
        let devices = nyx_capture_core::webcam::list_devices();
        if !devices.is_empty() {
            let mut opts = vec![("".into(), "First camera".into())];
            for d in devices {
                let label = if d.name.is_empty() { d.path.clone() } else { format!("{} ({})", d.name, d.path) };
                opts.push((d.path, label));
            }
            let (row, _) = widgets::choice_row(
                "Webcam",
                "Device for the overlay. First camera is the default.",
                opts,
                &prefs.record_webcam_device,
                |v| prefs::update(|p| p.record_webcam_device = v),
            );
            group.add(&row);
        }
    }
}

