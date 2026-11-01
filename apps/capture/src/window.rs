//! The Capture window (Nyx Design 2.0): the shared header with the capture actions,
//! search or the selection in the middle; the sidebar does not grow, and it collapses
//! to icons under 980 px or when the user collapses it.

use crate::paths;
use crate::prefs;
use crate::settings_dialog;
use crate::widgets;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use nyx_capture_core::action::{self, Action, After};
use nyx_capture_core::annot;
use nyx_capture_core::capture::CaptureMode;
use nyx_capture_core::clipboard;
use nyx_capture_core::ocr::{OcrMode, recognize};
use nyx_capture_core::record::{self, Container, RecordRequest, Target};
use nyx_capture_core::search::View;
use nyx_capture_core::store::{self, CaptureKind, CaptureRow};
use nyx_ui::chrome;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

pub const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["Ctrl", "Shift", "L"], "Scrolling capture"),
    (&["Ctrl", "N"], "Capture a region"),
    (&["Ctrl", "Shift", "N"], "Capture a window"),
    (&["Ctrl", "Shift", "F"], "Capture the focused monitor"),
    (&["Ctrl", "R"], "Start or stop a recording"),
    (&["Ctrl", "Shift", "R"], "Record a region"),
    (&["Ctrl", "Alt", "P"], "Pause or resume a recording"),
    (&["Ctrl", "F"], "Search history"),
    (&["Ctrl", "Click"], "Select captures (Shift+Click for a range)"),
    (&["Ctrl", "A"], "Select every capture shown"),
    (&["Delete"], "Delete the selected captures"),
    (&["Esc"], "Clear the selection"),
    (&["Ctrl", "B"], "Collapse or expand sidebar"),
    (&["Ctrl", ","], "Settings"),
    (&["F1"], "Show keyboard shortcuts"),
    (&["Ctrl", "Q"], "Close"),
];

struct Ui {
    window: gtk::ApplicationWindow,
    header: chrome::AppHeader,
    overlay: gtk::Overlay,
    nav: gtk::Box,
    nav_list: gtk::Box,
    search: gtk::SearchEntry,
    status: gtk::Label,
    hint: gtk::Label,
    scroller: gtk::Widget,
    selection: gtk::Box,
    selection_count: gtk::Label,
    view: View,
    items: Vec<CaptureRow>,
}

thread_local! {
    static UI: RefCell<Option<Rc<RefCell<Ui>>>> = const { RefCell::new(None) };
    static NARROW: Cell<bool> = const { Cell::new(false) };
    static TOAST: RefCell<Option<gtk::Box>> = const { RefCell::new(None) };
    static MENU: RefCell<Option<gtk::Popover>> = const { RefCell::new(None) };
    static RECORD_BTN: RefCell<Option<gtk::Button>> = const { RefCell::new(None) };
    static PAUSE_BTN: RefCell<Option<gtk::Button>> = const { RefCell::new(None) };
    static CAM_BTN: RefCell<Option<gtk::Button>> = const { RefCell::new(None) };
    /// Deleted but still undoable: hidden now, removed when their toast has gone.
    static PENDING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// How long a toast with an Undo button stays.
const UNDO_TOAST: std::time::Duration = std::time::Duration::from_millis(6000);

fn ui() -> Option<Rc<RefCell<Ui>>> {
    UI.with(|u| u.borrow().clone())
}

pub fn window() -> Option<gtk::ApplicationWindow> {
    ui().map(|u| u.borrow().window.clone())
}

pub fn overlay() -> Option<gtk::Overlay> {
    ui().map(|u| u.borrow().overlay.clone())
}

pub fn present(app: &gtk::Application, section: Option<&str>) {
    if ui().is_none() {
        build(app);
    }
    let Some(ui) = ui() else { return };
    let window = ui.borrow().window.clone();
    window.present();
    match section {
        Some("settings") => settings_dialog::open(),
        Some(other) => navigate(view_from_section(other)),
        None => {
            let start = nyx_theme::start_page("recent", &prefs::get().last_section);
            navigate(view_from_section(&start));
        }
    }
    maybe_tutorial();
}

fn view_from_section(section: &str) -> View {
    match section {
        "starred" => View::Starred,
        "screenshots" => View::Screenshots,
        "recordings" => View::Recordings,
        _ => View::Recent,
    }
}

fn section_id(view: View) -> &'static str {
    match view {
        View::Recent => "recent",
        View::Screenshots => "screenshots",
        View::Recordings => "recordings",
        View::Starred => "starred",
    }
}

fn page_name(view: View) -> &'static str {
    match view {
        View::Recent => "Recent",
        View::Screenshots => "Screenshots",
        View::Recordings => "Recordings",
        View::Starred => "Starred",
    }
}

pub fn navigate(view: View) {
    let Some(ui) = ui() else { return };
    ui.borrow_mut().view = view;
    prefs::update(|p| p.last_section = section_id(view).into());
    chrome::set_title(&ui.borrow().window, "Capture", page_name(view));
    ui.borrow().header.set_subtitle(page_name(view));
    reload();
    rebuild_nav();
}

pub fn reload() {
    let Some(ui) = ui() else { return };
    let view = ui.borrow().view;
    let query = ui.borrow().search.text().to_string();
    if !prefs::get().history {
        ui.borrow_mut().items.clear();
        crate::grid::fill(&[]);
        ui.borrow().scroller.set_visible(false);
        ui.borrow().hint.set_visible(true);
        ui.borrow().hint.set_text("History is off. New shots still copy or save when you ask.");
        refresh_status();
        return;
    }
    match store::open(&paths::library_db()).and_then(|db| db.list(view, query.trim(), 400)) {
        Ok(mut items) => {
            PENDING.with(|p| {
                let p = p.borrow();
                items.retain(|r| !p.contains(&r.id));
            });
            let empty = items.is_empty();
            crate::grid::fill(&items);
            ui.borrow_mut().items = items;
            ui.borrow().scroller.set_visible(!empty);
            ui.borrow().hint.set_visible(empty);
            ui.borrow().hint.set_text(if empty { empty_copy(view) } else { "" });
            refresh_status();
        }
        Err(err) => toast(&err.to_string()),
    }
}

fn empty_copy(view: View) -> &'static str {
    match view {
        View::Recordings => "No recordings yet. Ctrl+R records the focused monitor.",
        View::Starred => "Star a capture from the grid.",
        View::Screenshots => "No screenshots yet. Ctrl+N grabs a region.",
        View::Recent => "No captures yet. Ctrl+N grabs a region.",
    }
}

pub fn search(query: &str) -> anyhow::Result<Value> {
    let db = store::open(&paths::library_db())?;
    let items = db.list(View::Recent, query, 20)?;
    Ok(json!(items
        .into_iter()
        .map(|c| json!({"id": c.id, "title": c.title, "path": c.path.display().to_string()}))
        .collect::<Vec<_>>()))
}

pub fn send_screenshot(id: &str) -> Result<Value, String> {
    let db = store::open(&paths::library_db()).map_err(|e| e.to_string())?;
    let row = db.get(id).map_err(|e| e.to_string())?.ok_or_else(|| "that capture is not in the library".to_string())?;
    if row.sensitive {
        return Err("that capture is marked sensitive".into());
    }
    let text = db.ocr_text(id).map_err(|e| e.to_string())?.unwrap_or_default();
    // An edited shot goes out as edited (its blurs and boxes burned in), never the original.
    let path = if nyx_capture_core::export::is_edited(&row.path) {
        let out = paths::thumbs_dir().with_file_name("shared").join(format!("{id}.png"));
        std::fs::create_dir_all(out.parent().unwrap_or(&out)).map_err(|e| e.to_string())?;
        nyx_capture_core::export::write_flat(&row.path, &out).map_err(|e| e.to_string())?;
        out
    } else {
        row.path.clone()
    };
    Ok(json!({"id": id, "text": text, "path": path.display().to_string()}))
}

pub fn start_scroll() {
    if record::active().is_some() {
        toast("Stop the recording first");
        return;
    }
    if !nyx_capture_core::scroll::available() {
        toast("Scrolling capture needs omasnap");
        return;
    }
    toast("Scroll the region in omasnap");
    // With history off, the shot goes where saved screenshots go, not the history folder.
    let dir = if prefs::get().history {
        paths::captures_dir()
    } else {
        nyx_capture_core::action::default_save_path().parent().map(PathBuf::from).unwrap_or_else(paths::captures_dir)
    };
    crate::cmd::background(
        move || nyx_capture_core::scroll::capture(&dir),
        |result| match result {
            Ok(path) => ingest_scroll(path),
            Err(err) => toast(&err.to_string()),
        },
    );
}

fn ingest_scroll(path: PathBuf) {
    crate::cmd::background(
        move || {
            let img = image::open(&path)?.to_rgba8();
            let (w, h) = (img.width() as i32, img.height() as i32);
            let id = if prefs::get().history {
                let db = store::open(&paths::library_db())?;
                let id = db.insert(&store::NewCapture {
                    kind: CaptureKind::Screenshot,
                    width: w,
                    height: h,
                    path: path.clone(),
                    source: "omasnap".into(),
                })?;
                let _ = db.thumb(&id, &path, &paths::thumb_file(&id), false);
                Some(id)
            } else {
                None
            };
            anyhow::Ok((img, id, path))
        },
        |result| match result {
            Ok((img, id, path)) => {
                reload();
                crate::editor::open_saved(img, annot::EditState::default(), id.unwrap_or_default(), path);
            }
            Err(err) => toast(&err.to_string()),
        },
    );
}

pub fn show_tutorial() {
    let (dialog, card) = widgets::dialog("Capture tutorial", 460);
    let note = widgets::label("1. Region (or Scroll) freezes the screen.\n2. Annotate, then Redact if the shot has secrets.\n3. Copy leaves the original file unchanged. Esc cancels.", "dim");
    note.set_wrap(true);
    card.append(&note);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let later = gtk::Button::with_label("Not now");
    let go = gtk::Button::with_label("Capture a region");
    go.add_css_class("suggested-action");
    buttons.append(&later);
    buttons.append(&go);
    card.append(&buttons);
    let d = dialog.clone();
    later.connect_clicked(move |_| {
        prefs::update(|p| p.tutorial_done = true);
        d.close();
    });
    let d = dialog.clone();
    go.connect_clicked(move |_| {
        prefs::update(|p| p.tutorial_done = true);
        d.close();
        capture_now(CaptureMode::SelectRegion);
    });
    dialog.present();
}

fn maybe_tutorial() {
    if prefs::get().tutorial_done {
        return;
    }
    show_tutorial();
}

pub fn ocr_text(id: &str) -> anyhow::Result<Value> {
    let db = store::open(&paths::library_db())?;
    let row = db.get(id)?.ok_or_else(|| anyhow::anyhow!("that capture is not in the library"))?;
    if row.sensitive {
        anyhow::bail!("that capture is marked sensitive");
    }
    let text = db.ocr_text(id)?.unwrap_or_default();
    Ok(json!({"id": id, "text": text}))
}

pub fn open_id(id: &str) -> anyhow::Result<Value> {
    let db = store::open(&paths::library_db())?;
    let row = db.get(id)?.ok_or_else(|| anyhow::anyhow!("that capture is not in the library"))?;
    open_capture(row);
    Ok(json!({"opened": true, "id": id}))
}

pub fn export_id(id: &str, dest: &std::path::Path) -> anyhow::Result<Value> {
    let db = store::open(&paths::library_db())?;
    let row = db.get(id)?.ok_or_else(|| anyhow::anyhow!("that capture is not in the library"))?;
    if row.kind == CaptureKind::Recording {
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::copy(&row.path, dest)?;
    } else {
        nyx_capture_core::export::write_flat(&row.path, dest)?;
    }
    Ok(json!({"path": dest.display().to_string(), "id": id}))
}

fn refresh_status() {
    let Some(ui) = ui() else { return };
    let count = ui.borrow().items.len();
    let mut text = if !prefs::get().history {
        "History off".into()
    } else if count == 0 {
        String::new()
    } else if count == 1 {
        "1 capture".into()
    } else {
        format!("{count} captures")
    };
    let selected = crate::grid::selected().len();
    if selected > 0 {
        text = format!("{text} · {selected} selected");
    }
    if let Some(session) = record::active() {
        text = if session.paused { format!("Paused · {text}") } else { format!("Recording · {text}") };
    }
    ui.borrow().status.set_text(&text);
}

fn rebuild_nav() {
    let Some(ui) = ui() else { return };
    let list = ui.borrow().nav_list.clone();
    let current = ui.borrow().view;
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for (view, icon, label) in [
        (View::Recent, "document-open-recent-symbolic", "Recent"),
        (View::Screenshots, "camera-photo-symbolic", "Screenshots"),
        (View::Recordings, "media-record-symbolic", "Recordings"),
        (View::Starred, "starred-symbolic", "Starred"),
    ] {
        let (btn, _) = nav_button(icon, label, label);
        if view == current {
            btn.add_css_class("active");
        }
        btn.connect_clicked(move |_| navigate(view));
        list.append(&btn);
    }
    let nav = ui.borrow().nav.clone();
    apply_compact(&nav, NARROW.with(|n| n.get()) || prefs::get().sidebar_collapsed);
}

fn nav_button(icon: &str, title: &str, tooltip: &str) -> (gtk::Button, gtk::Box) {
    let btn = gtk::Button::new();
    btn.add_css_class("nav-item");
    btn.set_tooltip_text(Some(tooltip));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.append(&gtk::Image::from_icon_name(icon));
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.add_css_class("compact-hide");
    row.append(&label);
    btn.set_child(Some(&row));
    (btn, row)
}

fn build(app: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder().application(app).title("Capture").default_width(1120).default_height(800).build();
    window.add_css_class("capture-window");
    window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    window.set_icon_name(Some("io.github.design_nexus.nyx.Capture"));
    nyx_ui::keys::install(
        &window,
        nyx_ui::keys::Common {
            settings: Some(settings_dialog::open),
            shortcuts: Some(show_shortcuts),
            close: true,
            ..Default::default()
        },
    );

    let nav = gtk::Box::new(gtk::Orientation::Vertical, 0);
    nav.add_css_class("settings-navigation");
    nav.set_hexpand(false);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.set_hexpand(false);
    let nav_scroll = gtk::ScrolledWindow::builder()
        .hexpand(false)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(&list)
        .build();
    nav.append(&nav_scroll);

    // The app and the page, the sidebar toggle; search (or the selection) in the
    // middle; what's going on, the capture actions and Settings at the end.
    let header = chrome::app_header(&window, "capture-region-symbolic", "Capture");
    let collapse = widgets::bar_button("sidebar-show-symbolic", "Collapse the sidebar (Ctrl+B)");
    collapse.connect_clicked(|_| toggle_sidebar());
    header.start.append(&collapse);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search captures (Ctrl+F)"));
    search.add_css_class("bar-search");
    // A small minimum keeps the header narrow enough for a half-screen tile.
    search.set_width_chars(8);
    search.set_max_width_chars(SEARCH_WIDE);
    search.connect_search_changed(|_| reload());
    search.connect_stop_search(|entry| entry.set_text(""));
    header.center.append(&search);

    let hint = widgets::label("No captures yet. Ctrl+N grabs a region.", "empty-hint");
    hint.set_wrap(true);
    hint.set_justify(gtk::Justification::Center);
    hint.set_valign(gtk::Align::Center);
    hint.set_vexpand(true);

    let scroller = crate::grid::build();

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let tools = nyx_capture_core::detect::Tools::probe();
    if tools.grim {
        let region = widgets::bar_button("capture-region-symbolic", "Capture a region (Ctrl+N)");
        region.connect_clicked(|_| capture_now(CaptureMode::SelectRegion));
        toolbar.append(&region);
        if tools.hyprctl {
            let window_btn = widgets::bar_button("focus-windows-symbolic", "Capture a window (Ctrl+Shift+N)");
            window_btn.connect_clicked(|_| capture_now(CaptureMode::SelectWindow));
            toolbar.append(&window_btn);
        }
        let screen = widgets::bar_button("video-display-symbolic", "Capture the focused monitor (Ctrl+Shift+F)");
        screen.connect_clicked(|_| capture_now(CaptureMode::FullDesktop));
        toolbar.append(&screen);
        if tools.can_scroll() {
            let scroll = widgets::bar_button("view-continuous-symbolic", "Scrolling capture (Ctrl+Shift+L)");
            scroll.connect_clicked(|_| start_scroll());
            toolbar.append(&scroll);
        }
    }
    if tools.can_record() {
        let rec = widgets::bar_button("media-record-symbolic", "Start a recording (Ctrl+R)");
        rec.connect_clicked(|_| toggle_record(false));
        RECORD_BTN.with(|b| *b.borrow_mut() = Some(rec.clone()));
        toolbar.append(&rec);
        let pause = widgets::bar_button("media-playback-pause-symbolic", "Pause recording (Ctrl+Alt+P)");
        pause.set_visible(false);
        pause.connect_clicked(|_| pause_recording());
        PAUSE_BTN.with(|b| *b.borrow_mut() = Some(pause.clone()));
        toolbar.append(&pause);
        glib::timeout_add_seconds_local(1, || {
            refresh_record_button();
            glib::ControlFlow::Continue
        });
        refresh_record_button();
    }
    if tools.can_webcam() {
        let cam = widgets::bar_button("camera-web-symbolic", "Picture-in-picture webcam");
        cam.connect_clicked(|_| toggle_webcam());
        CAM_BTN.with(|b| *b.borrow_mut() = Some(cam.clone()));
        toolbar.append(&cam);
        refresh_cam_button();
    }

    // While captures are selected the middle of the header is the selection: how
    // many, then the actions.
    let selection = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    selection.set_visible(false);
    let selection_count = widgets::label("", "selection-chip");
    selection_count.set_valign(gtk::Align::Center);
    selection_count.set_margin_end(4);
    let delete = widgets::bar_button("user-trash-symbolic", "Delete the selected captures (Delete)");
    delete.connect_clicked(|_| delete_later(crate::grid::selected()));
    let deselect = widgets::bar_button("edit-clear-symbolic", "Clear the selection (Esc)");
    deselect.connect_clicked(|_| crate::grid::clear_selection());
    selection.append(&selection_count);
    selection.append(&delete);
    selection.append(&deselect);
    header.center.append(&selection);

    header.end.set_spacing(4);
    let status = widgets::label("", "header-status");
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_max_width_chars(24);
    status.set_margin_end(4);
    header.end.append(&status);
    header.end.append(&toolbar);
    header.end.append(&chrome::settings_button(settings_dialog::open));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.add_css_class("settings-content");
    content.set_hexpand(true);
    content.append(&hint);
    content.append(&scroller);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.set_vexpand(true);
    body.append(&nav);
    body.append(&content);

    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("window-frame");
    frame.append(&header.root);
    frame.append(&body);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&frame));
    window.set_child(Some(&overlay));
    install_keys(&window, &search);

    let ui = Ui {
        window: window.clone(),
        header,
        overlay: overlay.clone(),
        nav: nav.clone(),
        nav_list: list,
        search,
        status,
        hint,
        scroller,
        selection,
        selection_count,
        view: View::Recent,
        items: Vec::new(),
    };
    UI.with(|slot| *slot.borrow_mut() = Some(Rc::new(RefCell::new(ui))));
    window.connect_close_request(|_| {
        flush_deletes();
        glib::Propagation::Proceed
    });

    let apply_width = {
        let nav = nav.clone();
        let window = window.clone();
        move || {
            let width = if window.width() > 0 { window.width() } else { window.default_width() };
            let narrow = width > 0 && width < 980;
            settings_dialog::fit(&window);
            if narrow == NARROW.with(|n| n.get()) && nav.has_css_class("sized") {
                return;
            }
            nav.add_css_class("sized");
            NARROW.with(|n| n.set(narrow));
            apply_compact(&nav, narrow || prefs::get().sidebar_collapsed);
            if let Some(ui) = self::ui() {
                let ui = ui.borrow();
                // The centred search mustn't reach the sidebar toggle or the actions.
                ui.search.set_max_width_chars(if narrow { SEARCH_NARROW } else { SEARCH_WIDE });
                ui.status.set_visible(!narrow);
            }
        }
    };
    let aw = apply_width.clone();
    window.connect_realize(move |_| aw());
    let probe = gtk::DrawingArea::new();
    probe.set_can_target(false);
    probe.set_can_focus(false);
    overlay.add_overlay(&probe);
    overlay.set_measure_overlay(&probe, false);
    probe.connect_resize(move |_, _, _| {
        let aw = apply_width.clone();
        glib::idle_add_local_once(aw);
    });
    rebuild_nav();
    reload();
    prune_history();
}

pub fn capture_now(mode: CaptureMode) {
    capture_with(mode, after_from_prefs(), 0);
}

thread_local! {
    /// The window that was focused when the last capture began: the shot's title.
    static LAST_TITLE: RefCell<String> = const { RefCell::new(String::new()) };
}

thread_local! {
    /// Brings the history window back once the editor a capture opened is closed.
    static RESTORE_AFTER_EDIT: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// The editor closed: show the history window again if the capture hid it.
pub fn editor_closed() {
    if let Some(restore) = RESTORE_AFTER_EDIT.with(|r| r.borrow_mut().take()) {
        restore();
    }
}

/// The title for the capture being taken (see `LAST_TITLE`).
pub fn capture_title() -> String {
    LAST_TITLE.with(|t| t.borrow().clone())
}

pub fn capture_with(mode: CaptureMode, after: After, delay: u32) {
    capture_with_path(mode, after, delay, None);
}

/// [`capture_with`], saving to `save_path` (from `--save PATH`) instead of the Save folder.
pub fn capture_with_path(mode: CaptureMode, after: After, delay: u32, save_path: Option<PathBuf>) {
    let title = nyx_capture_core::select::capture_title();
    LAST_TITLE.with(|t| *t.borrow_mut() = title);
    let win = window();
    // Hyprland doesn't minimize; hide the window so it isn't in its own shot (the
    // capture waits 250 ms for it to unmap).
    if let Some(w) = &win {
        w.set_visible(false);
    }
    let langs = prefs::get().ocr_langs;
    let ocr_mode = OcrMode::parse(&prefs::get().ocr_mode);
    let save_path = save_path.or_else(save_file);
    let restore = {
        let win = win.clone();
        Rc::new(move || {
            if let Some(w) = &win {
                w.present();
            }
        })
    };
    if delay > 0 {
        let restore_wait = restore.clone();
        let go = {
            let restore = restore.clone();
            let langs = langs.clone();
            let save_path = save_path.clone();
            let mode = mode.clone();
            move || begin_capture(mode, after, langs, ocr_mode, save_path, restore)
        };
        match crate::overlay::countdown(delay, move |ok| {
            if ok {
                go();
            } else {
                restore_wait();
            }
        }) {
            Ok(()) => return,
            Err(err) => toast(&err.to_string()),
        }
    }
    begin_capture(mode, after, langs, ocr_mode, save_path, restore);
}

fn begin_capture(
    mode: CaptureMode,
    after: After,
    langs: String,
    ocr_mode: OcrMode,
    save_path: Option<PathBuf>,
    restore: Rc<dyn Fn()>,
) {
    let backend = prefs::get().backend;
    let use_overlay = matches!(mode, CaptureMode::SelectRegion | CaptureMode::SelectWindow)
        && crate::overlay::prefer()
        && nyx_capture_core::capture::pick(&backend) != "portal";
    if use_overlay {
        let restore = restore.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            start_overlay(mode, after, langs, ocr_mode, save_path, restore);
        });
        return;
    }
    let langs_ui = langs.clone();
    let save_done = save_path.clone();
    crate::cmd::background(
        move || {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let action = Action { mode, after, delay: 0, save_path, ocr_langs: langs, ocr_mode, backend };
            action::capture(&action)
        },
        {
            let restore = restore.clone();
            move |result| {
                restore();
                match result {
                    Ok(Some(shot)) => finish_shot(shot, after, langs_ui, ocr_mode, save_done),
                    Ok(None) => {}
                    Err(err) => report_capture(Err(err)),
                }
            }
        },
    );
}

fn after_from_prefs() -> After {
    match prefs::get().after_capture.as_str() {
        "save" => After::Save,
        "ocr" => After::Ocr,
        "copy" => After::Copy,
        "pin" => After::Pin,
        _ => After::Edit,
    }
}

fn save_file() -> Option<PathBuf> {
    let dir = prefs::get().save_dir;
    if dir.is_empty() {
        None
    } else {
        Some(action::save_path_in(std::path::Path::new(&dir)))
    }
}

fn finish_shot(
    shot: nyx_capture_core::capture::Shot,
    after: After,
    langs: String,
    ocr_mode: OcrMode,
    save_path: Option<PathBuf>,
) {
    match after {
        After::Edit => crate::editor::open(shot),
        After::Pin => {
            remember(&shot);
            crate::pin::open_shot(shot);
        }
        other => {
            remember(&shot);
            crate::cmd::background(
                move || {
                    let action = Action { mode: CaptureMode::FullDesktop, after: other, delay: 0, save_path, ocr_langs: langs, ocr_mode, backend: String::new() };
                    action::finish(&shot, &action)
                },
                report_capture,
            );
        }
    }
}

fn start_overlay(mode: CaptureMode, after: After, langs: String, ocr_mode: OcrMode, save_path: Option<PathBuf>, restore: Rc<dyn Fn()>) {
    let snap = matches!(mode, CaptureMode::SelectWindow);
    if crate::overlay::supported() {
        match crate::overlay::freeze() {
            Ok(still) => {
                let cropped = still.clone();
                let restore_done = restore.clone();
                let langs_done = langs.clone();
                let save_picked = save_path.clone();
                match crate::overlay::pick(still, snap, move |rect| {
                    let Some(rect) = rect else {
                        restore_done();
                        return;
                    };
                    crate::cmd::background(
                        move || cropped.crop_layout(rect),
                        move |result| match result {
                            Ok(shot) => {
                                // The editor replaces the history window until it closes.
                                if after == After::Edit {
                                    RESTORE_AFTER_EDIT.with(|r| *r.borrow_mut() = Some(restore_done.clone()));
                                } else {
                                    restore_done();
                                }
                                finish_shot(shot, after, langs_done.clone(), ocr_mode, save_picked.clone());
                            }
                            Err(err) => {
                                restore_done();
                                report_capture(Err(err));
                            }
                        },
                    );
                }) {
                    Ok(()) => return,
                    Err(err) => toast(&err.to_string()),
                }
            }
            Err(err) => {
                restore();
                toast(&err.to_string());
                return;
            }
        }
    }
    let langs_ui = langs.clone();
    let save_done = save_path.clone();
    crate::cmd::background(
        move || {
            let action = Action { mode, after, delay: 0, save_path, ocr_langs: langs, ocr_mode, backend: String::new() };
            action::capture(&action)
        },
        {
            let restore = restore.clone();
            move |result| {
                restore();
                match result {
                    Ok(Some(shot)) => finish_shot(shot, after, langs_ui, ocr_mode, save_done),
                    Ok(None) => {}
                    Err(err) => report_capture(Err(err)),
                }
            }
        },
    );
}

fn remember(shot: &nyx_capture_core::capture::Shot) {
    if !prefs::get().history {
        return;
    }
    let Ok(png) = shot.png() else { return };
    if png.is_empty() {
        return;
    }
    let (w, h) = (shot.image.width() as i32, shot.image.height() as i32);
    let image = shot.image.clone();
    let ocr_on = prefs::get().ocr_on_capture;
    let langs = prefs::get().ocr_langs;
    let mode = OcrMode::parse(&prefs::get().ocr_mode);
    let title = capture_title();
    crate::cmd::background(
        move || {
            let db = store::open(&paths::library_db())?;
            let (id, path) = db.insert_png(&paths::captures_dir(), &png, w, h, "grim")?;
            let _ = db.set_title(&id, &title);
            let _ = db.thumb(&id, &path, &paths::thumb_file(&id), false);
            if ocr_on && let Ok(r) = recognize(&image, mode, &langs) {
                let _ = db.set_ocr(&id, &r.engine, &r.text);
            }
            anyhow::Ok(())
        },
        |result| match result {
            Ok(()) => reload(),
            Err(err) => toast(&err.to_string()),
        },
    );
}

fn prune_history() {
    if !prefs::get().history {
        return;
    }
    let days = prefs::get().retention_days;
    crate::cmd::background(
        move || {
            let db = store::open(&paths::library_db())?;
            db.prune(days)
        },
        |result| match result {
            Ok(gone) if !gone.is_empty() => {
                // Thumbnails are named after the capture's id.
                let thumbs = paths::thumbs_dir();
                for p in gone.iter().filter(|p| p.parent() == Some(thumbs.as_path())) {
                    if let Some(id) = p.file_stem() {
                        crate::grid::forget_thumb(&id.to_string_lossy());
                    }
                }
                reload();
            }
            Err(err) => toast(&err.to_string()),
            _ => {}
        },
    );
}

pub fn toggle_record(region: bool) {
    if record::active().is_some() {
        stop_recording();
        return;
    }
    start_recording(region);
}

pub fn start_recording(region: bool) {
    if record::active().is_some() {
        toast("Already recording");
        return;
    }
    let delay = prefs::get().record_delay;
    if delay > 0 {
        let _ = crate::overlay::countdown(delay, move |ok| {
            if ok {
                begin_record(region);
            }
        });
        return;
    }
    begin_record(region);
}

pub fn stop_recording() {
    crate::overlay::hide_recording_pill();
    crate::cmd::background(record::stop, |result| match result {
        Ok(Some(path)) => {
            remember_recording(&path);
            refresh_record_button();
            refresh_cam_button();
            toast(&format!("Saved {}", path.display()));
        }
        Ok(None) => {
            refresh_record_button();
            refresh_cam_button();
        }
        Err(err) => {
            refresh_record_button();
            refresh_cam_button();
            toast(&err.to_string());
        }
    });
}

fn begin_record(region: bool) {
    if record::pick(&prefs::get().recorder) == Some(nyx_capture_core::RecordBackend::GstPortal) {
        launch_record(Target::Portal);
        return;
    }
    if region && crate::overlay::prefer() && crate::overlay::supported() {
        match crate::overlay::freeze() {
            Ok(still) => {
                if crate::overlay::pick(still, false, move |rect| {
                    let Some(rect) = rect else { return };
                    launch_record(Target::Region(rect));
                })
                .is_err()
                {
                    launch_record(record_target(true));
                }
            }
            Err(err) => toast(&err.to_string()),
        }
        return;
    }
    // slurp waits for the person to drag: not on the GTK thread.
    crate::cmd::background(move || record_target(region), launch_record);
}

fn record_target(region: bool) -> Target {
    if !region && prefs::get().record_use_portal {
        return Target::Portal;
    }
    if region && let Ok(Some(r)) = nyx_capture_core::capture::slurp_region() {
        return Target::Region(r);
    }
    record::default_target().unwrap_or(Target::Portal)
}

fn record_request(target: Target, output: PathBuf) -> RecordRequest {
    let p = prefs::get();
    let (desktop_audio, microphone) = match p.record_audio.as_str() {
        "mic" => (false, true),
        "both" => (true, true),
        "none" => (false, false),
        _ => (true, false),
    };
    RecordRequest {
        target,
        desktop_audio,
        microphone,
        cursor: p.record_cursor,
        container: Container::parse(&p.record_container),
        fps: 60,
        output,
        backend: p.recorder,
    }
}

/// Recordings go to the Videos folder (history lists them there); they're the person's
/// files, so history retention never deletes them.
fn record_output_path() -> PathBuf {
    let container = Container::parse(&prefs::get().record_container);
    record::output_path(&record::default_output_dir(), container)
}

fn launch_record(target: Target) {
    let req = record_request(target, record_output_path());
    let p = prefs::get();
    let cam = p.record_webcam;
    let cam_device = p.record_webcam_device;
    let cam_size = p.record_webcam_size;
    crate::cmd::background(
        move || start_record_session(&req, cam, &cam_device, &cam_size),
        |result| match result {
            Ok(_) => {
                refresh_record_button();
                refresh_cam_button();
                crate::overlay::show_recording_pill();
                toast("Recording");
            }
            Err(err) => toast(&err.to_string()),
        },
    );
}

fn start_record_session(
    req: &RecordRequest,
    cam: bool,
    cam_device: &str,
    cam_size: &str,
) -> anyhow::Result<nyx_capture_core::RecordSession> {
    let started_cam = cam && nyx_capture_core::webcam::available() && !nyx_capture_core::webcam::active();
    if started_cam {
        let device = if cam_device.is_empty() { None } else { Some(cam_device) };
        if let Err(err) = nyx_capture_core::webcam::start(device, nyx_capture_core::webcam::Size::parse(cam_size), true) {
            eprintln!("nyx-capture: webcam overlay: {err}");
        }
    }
    match record::start(req) {
        Ok(session) => Ok(session),
        Err(err) => {
            if started_cam {
                nyx_capture_core::webcam::stop();
            }
            Err(err)
        }
    }
}

pub fn pause_recording() {
    crate::cmd::background(record::toggle_pause, |result| match result {
        Ok(true) => {
            refresh_record_button();
            toast("Paused");
        }
        Ok(false) => {
            refresh_record_button();
            toast("Recording");
        }
        Err(err) => toast(&err.to_string()),
    });
}

pub fn toggle_webcam() {
    if nyx_capture_core::webcam::active() {
        nyx_capture_core::webcam::stop();
        refresh_cam_button();
        return;
    }
    let p = prefs::get();
    let device = p.record_webcam_device.clone();
    let size = p.record_webcam_size.clone();
    crate::cmd::background(
        move || {
            let dev = if device.is_empty() { None } else { Some(device.as_str()) };
            nyx_capture_core::webcam::start(dev, nyx_capture_core::webcam::Size::parse(&size), false)
        },
        |result| match result {
            Ok(_) => refresh_cam_button(),
            Err(err) => {
                refresh_cam_button();
                toast(&err.to_string());
            }
        },
    );
}

fn remember_recording(path: &std::path::Path) {
    if !prefs::get().history {
        return;
    }
    let path = path.to_path_buf();
    crate::cmd::background(
        move || {
            let db = store::open(&paths::library_db())?;
            let id = db.insert(&store::NewCapture {
                kind: CaptureKind::Recording,
                width: 0,
                height: 0,
                path: path.clone(),
                source: "record".into(),
            })?;
            let _ = db.thumb(&id, &path, &paths::thumb_file(&id), false);
            anyhow::Ok(())
        },
        |result| match result {
            Ok(()) => reload(),
            Err(err) => toast(&err.to_string()),
        },
    );
}

fn refresh_record_button() {
    // A recorder that ended by itself (crashed, killed) still leaves its file for history.
    if let Some(path) = record::ended() {
        remember_recording(&path);
        toast(&format!("The recording ended; saved {}", path.display()));
    }
    RECORD_BTN.with(|b| {
        let Some(btn) = b.borrow().clone() else { return };
        if record::active().is_some() {
            btn.set_icon_name("media-playback-stop-symbolic");
            btn.set_tooltip_text(Some("Stop recording (Ctrl+R)"));
            btn.add_css_class("destructive-action");
        } else {
            btn.set_icon_name("media-record-symbolic");
            btn.set_tooltip_text(Some("Start a recording (Ctrl+R)"));
            btn.remove_css_class("destructive-action");
        }
    });
    PAUSE_BTN.with(|b| {
        let Some(btn) = b.borrow().clone() else { return };
        let session = record::active();
        btn.set_visible(record::can_pause());
        if let Some(session) = session {
            if session.paused {
                btn.set_icon_name("media-playback-start-symbolic");
                btn.set_tooltip_text(Some("Resume recording (Ctrl+Alt+P)"));
            } else {
                btn.set_icon_name("media-playback-pause-symbolic");
                btn.set_tooltip_text(Some("Pause recording (Ctrl+Alt+P)"));
            }
        }
    });
    refresh_cam_button();
    refresh_status();
}

fn refresh_cam_button() {
    CAM_BTN.with(|b| {
        let Some(btn) = b.borrow().clone() else { return };
        if nyx_capture_core::webcam::active() {
            btn.add_css_class("suggested-action");
            btn.set_tooltip_text(Some("Hide webcam overlay"));
        } else {
            btn.remove_css_class("suggested-action");
            btn.set_tooltip_text(Some("Picture-in-picture webcam"));
        }
    });
}

pub fn open_capture(row: CaptureRow) {
    if row.kind == CaptureKind::Recording {
        let path = row.path.to_string_lossy().into_owned();
        crate::cmd::spawn(&["xdg-open", &path]);
        return;
    }
    let path = row.path.clone();
    let id = row.id.clone();
    crate::cmd::background(
        move || {
            let img = image::open(&path)?.to_rgba8();
            let state = annot::EditState::load(&annot::sidecar_path(&path)).unwrap_or_default();
            anyhow::Ok((img, state, id, path))
        },
        |result| match result {
            Ok((img, state, id, path)) => crate::editor::open_saved(img, state, id, path),
            Err(err) => toast(&err.to_string()),
        },
    );
}

pub fn set_starred(id: &str, on: bool) {
    let id = id.to_string();
    crate::cmd::background(
        move || {
            store::open(&paths::library_db())?.set_starred(&id, on)
        },
        |result| match result {
            Ok(()) => reload(),
            Err(err) => toast(&err.to_string()),
        },
    );
}

pub fn item_menu(id: &str, parent: &gtk::Widget, x: f64, y: f64) {
    let Some(ui) = ui() else { return };
    let Some(row) = ui.borrow().items.iter().find(|c| c.id == id).cloned() else { return };
    MENU.with(|m| {
        if let Some(old) = m.borrow_mut().take() {
            old.unparent();
        }
    });
    let menu = gio::Menu::new();
    menu.append(Some("Open"), Some("item.open"));
    menu.append(Some(if row.starred { "Unstar" } else { "Star" }), Some("item.star"));
    if row.kind != CaptureKind::Recording {
        menu.append(Some("Pin"), Some("item.pin"));
        menu.append(Some("Copy"), Some("item.copy"));
    }
    menu.append(Some("Export…"), Some("item.export"));
    menu.append(Some("Share…"), Some("item.share"));
    if row.kind == CaptureKind::Recording && nyx_capture_core::detect::present("ffmpeg") {
        menu.append(Some("Trim…"), Some("item.trim"));
    }
    // A selected capture's Delete takes the whole selection along.
    let doomed = if crate::grid::is_selected(id) { crate::grid::selected() } else { vec![row.id.clone()] };
    let delete_label = if doomed.len() > 1 { format!("Delete {} captures", doomed.len()) } else { "Delete".into() };
    menu.append(Some(&delete_label), Some("item.delete"));
    let group = gio::SimpleActionGroup::new();
    let act = |name: &str, f: Box<dyn Fn()>| {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| f());
        group.add_action(&a);
    };
    let r = row.clone();
    act("open", Box::new(move || open_capture(r.clone())));
    let (id, on) = (row.id.clone(), !row.starred);
    act("star", Box::new(move || set_starred(&id, on)));
    let path = row.path.clone();
    act("pin", Box::new(move || pin_file(&path)));
    let path = row.path.clone();
    act("copy", Box::new(move || copy_capture(&path)));
    let id = row.id.clone();
    let name = row.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "capture.png".into());
    act("export", Box::new(move || export_capture(&id, &name)));
    let (path, recording) = (row.path.clone(), row.kind == CaptureKind::Recording);
    act(
        "share",
        Box::new(move || {
            crate::share::capture(path.clone(), recording, |r| {
                if let Err(e) = r {
                    toast(&e);
                }
            })
        }),
    );
    let r = row.clone();
    act("trim", Box::new(move || trim_capture(&r)));
    act("delete", Box::new(move || delete_later(doomed.clone())));
    let pop = gtk::PopoverMenu::from_model(Some(&menu));
    pop.insert_action_group("item", Some(&group));
    pop.set_parent(parent);
    pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    pop.set_has_arrow(false);
    MENU.with(|m| *m.borrow_mut() = Some(pop.clone().upcast()));
    pop.popup();
}

/// Pin a capture as it was edited (marks burned in), off the GTK thread.
fn pin_file(path: &std::path::Path) {
    let path = path.to_path_buf();
    crate::cmd::background(
        move || nyx_capture_core::export::shareable(&path).map(|img| img.to_rgba8()),
        |result| match result {
            Ok(img) => crate::pin::open_rgba(img),
            Err(err) => toast(&err.to_string()),
        },
    );
}

fn export_capture(id: &str, name: &str) {
    let id = id.to_string();
    let dialog = gtk::FileDialog::builder().title("Export capture").modal(true).initial_name(name).build();
    dialog.save(window().as_ref(), gio::Cancellable::NONE, move |res| {
        let Ok(file) = res else { return };
        let Some(path) = file.path() else { return };
        let id = id.clone();
        crate::cmd::background(
            move || export_id(&id, &path).map(|_| path),
            |result| match result {
                Ok(path) => toast(&format!("Saved {}", path.display())),
                Err(err) => toast(&err.to_string()),
            },
        );
    });
}

fn trim_capture(row: &CaptureRow) {
    let path = row.path.clone();
    let (dialog, card) = widgets::dialog("Trim recording", 420);
    let note = widgets::label("Seconds from the start. Leave the end empty to keep the rest. The original file stays.", "dim");
    note.set_wrap(true);
    card.append(&note);
    let start = gtk::Entry::new();
    start.set_placeholder_text(Some("Start"));
    start.set_text("0");
    card.append(&start);
    let end = gtk::Entry::new();
    end.set_placeholder_text(Some("End (optional)"));
    card.append(&end);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Not now");
    let ok = gtk::Button::with_label("Trim");
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let d = dialog.clone();
    ok.connect_clicked(move |_| {
        let start_s = start.text().to_string();
        let end_s = end.text().to_string();
        let Ok(start_t) = start_s.trim().parse::<f64>() else {
            start.add_css_class("error");
            return;
        };
        if start_t < 0.0 {
            start.add_css_class("error");
            return;
        }
        let end_t = match end_s.trim() {
            "" => None,
            s => match s.parse::<f64>() {
                Ok(n) if n > start_t => Some(n),
                _ => {
                    end.add_css_class("error");
                    return;
                }
            },
        };
        d.close();
        let src = path.clone();
        crate::cmd::background(
            move || {
                let dest = trim_dest(&src);
                record::trim(&src, &dest, start_t, end_t)?;
                anyhow::Ok(dest)
            },
            |result| match result {
                Ok(dest) => {
                    remember_recording(&dest);
                    toast(&format!("Saved {}", dest.display()));
                }
                Err(err) => toast(&err.to_string()),
            },
        );
    });
    dialog.present();
}

fn trim_dest(src: &std::path::Path) -> PathBuf {
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("recording");
    let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("mp4");
    let dir = src.parent().unwrap_or_else(|| std::path::Path::new("."));
    let dest = dir.join(format!("{stem}-trim.{ext}"));
    if dest.exists() {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        dir.join(format!("{stem}-trim-{stamp}.{ext}"))
    } else {
        dest
    }
}

fn copy_capture(path: &std::path::Path) {
    let path = path.to_path_buf();
    crate::cmd::background(
        // As edited: a redacted shot is never copied without its redaction.
        move || clipboard::copy_png(&nyx_capture_core::export::shareable_png(&path)?),
        |result| match result {
            Ok(()) => toast("Copied"),
            Err(err) => toast(&err.to_string()),
        },
    );
}

/// Hide `ids` now and delete them when the Undo toast has gone.
pub fn delete_later(ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    PENDING.with(|p| p.borrow_mut().extend(ids.iter().cloned()));
    crate::grid::clear_selection();
    reload();
    let message = if ids.len() == 1 { "Deleted the capture".to_string() } else { format!("Deleted {} captures", ids.len()) };
    let batch = ids.clone();
    toast_with_undo(&message, move || {
        PENDING.with(|p| p.borrow_mut().retain(|id| !batch.contains(id)));
        reload();
    });
    glib::timeout_add_local_once(UNDO_TOAST, move || {
        // Whatever Undo didn't bring back.
        let gone: Vec<String> = PENDING.with(|p| {
            let mut p = p.borrow_mut();
            let gone = ids.iter().filter(|id| p.contains(id)).cloned().collect();
            p.retain(|id| !ids.contains(id));
            gone
        });
        if gone.is_empty() {
            return;
        }
        let ids_ui = gone.clone();
        crate::cmd::background(
            move || delete_now(&gone),
            move |result| {
                for id in &ids_ui {
                    crate::grid::forget_thumb(id);
                }
                if let Err(err) = result {
                    toast(&err.to_string());
                }
                reload();
            },
        );
    });
}

fn delete_now(ids: &[String]) -> anyhow::Result<()> {
    let db = store::open(&paths::library_db())?;
    for id in ids {
        db.delete(id)?;
    }
    Ok(())
}

/// Delete what's still waiting on an Undo toast, before the window or app goes.
pub fn flush_deletes() {
    let gone = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    if gone.is_empty() {
        return;
    }
    if let Err(err) = delete_now(&gone) {
        eprintln!("nyx-capture: {err}");
    }
    for id in &gone {
        crate::grid::forget_thumb(id);
    }
}

/// The grid's selection changed: show what can be done with it.
pub fn selection_changed() {
    let Some(ui) = ui() else { return };
    let n = crate::grid::selected().len();
    ui.borrow().selection.set_visible(n > 0);
    ui.borrow().search.set_visible(n == 0);
    ui.borrow().selection_count.set_text(&format!("{n} selected"));
    refresh_status();
}

fn report_capture(result: Result<action::Outcome, anyhow::Error>) {
    match result {
        Ok(action::Outcome::Cancelled) => {}
        Ok(action::Outcome::Copied) => toast("Copied"),
        Ok(action::Outcome::Saved(path)) => toast(&format!("Saved {}", path.display())),
        Ok(action::Outcome::Text(_)) => toast("Copied text"),
        Err(err) => toast(&err.to_string()),
    }
}

const SEARCH_WIDE: i32 = 24;
const SEARCH_NARROW: i32 = 12;

fn apply_compact(nav: &gtk::Box, compact: bool) {
    if compact {
        nav.add_css_class("compact");
    } else {
        nav.remove_css_class("compact");
    }
    fn walk(widget: &gtk::Widget, compact: bool) {
        if widget.has_css_class("compact-hide") {
            widget.set_visible(!compact);
        }
        if widget.has_css_class("nav-item")
            && let Some(content) = widget.downcast_ref::<gtk::Button>().and_then(|b| b.child())
        {
            content.set_halign(if compact { gtk::Align::Center } else { gtk::Align::Fill });
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            walk(&next, compact);
            child = next.next_sibling();
        }
    }
    walk(nav.upcast_ref(), compact);
}

pub fn toggle_sidebar() {
    prefs::update(|p| p.sidebar_collapsed = !p.sidebar_collapsed);
    let Some(ui) = ui() else { return };
    let nav = ui.borrow().nav.clone();
    apply_compact(&nav, NARROW.with(|n| n.get()) || prefs::get().sidebar_collapsed);
}

fn typing(window: &gtk::Window) -> bool {
    let Some(focus) = gtk::prelude::GtkWindowExt::focus(window) else { return false };
    focus.is::<gtk::Text>() || focus.is::<gtk::TextView>() || focus.ancestor(gtk::Entry::static_type()).is_some()
}

fn install_keys(window: &gtk::ApplicationWindow, search: &gtk::SearchEntry) {
    let keys = gtk::EventControllerKey::new();
    let search = search.clone();
    let win = window.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
        if ctrl && key == gdk::Key::f {
            search.grab_focus();
            return glib::Propagation::Stop;
        }
        if ctrl && shift && key == gdk::Key::l {
            start_scroll();
            return glib::Propagation::Stop;
        }
        if ctrl && shift && key == gdk::Key::n {
            capture_now(CaptureMode::SelectWindow);
            return glib::Propagation::Stop;
        }
        if ctrl && shift && key == gdk::Key::f {
            capture_now(CaptureMode::FullDesktop);
            return glib::Propagation::Stop;
        }
        if ctrl && shift && key == gdk::Key::r {
            toggle_record(true);
            return glib::Propagation::Stop;
        }
        // Ctrl+Shift+P is the command palette.
        if ctrl && mods.contains(gdk::ModifierType::ALT_MASK) && matches!(key, gdk::Key::p | gdk::Key::P) {
            pause_recording();
            return glib::Propagation::Stop;
        }
        if ctrl && key == gdk::Key::r {
            toggle_record(false);
            return glib::Propagation::Stop;
        }
        if ctrl && key == gdk::Key::n {
            capture_now(CaptureMode::SelectRegion);
            return glib::Propagation::Stop;
        }
        if ctrl && key == gdk::Key::b {
            toggle_sidebar();
            return glib::Propagation::Stop;
        }
        if typing(win.upcast_ref()) {
            return glib::Propagation::Proceed;
        }
        if ctrl && key == gdk::Key::a {
            crate::grid::select_all();
            return glib::Propagation::Stop;
        }
        if matches!(key, gdk::Key::Delete | gdk::Key::KP_Delete) && crate::grid::has_selection() {
            delete_later(crate::grid::selected());
            return glib::Propagation::Stop;
        }
        if key == gdk::Key::Escape && crate::grid::has_selection() {
            crate::grid::clear_selection();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    window.add_controller(keys);
}

pub fn show_shortcuts() {
    let (dialog, card) = widgets::dialog("Keyboard shortcuts", 520);
    let list = widgets::vbox(0);
    list.add_css_class("group-list");
    for (keys, what) in SHORTCUTS {
        list.append(&widgets::row(what, "", Some(widgets::key_caps(keys).upcast_ref())));
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .propagate_natural_height(true)
        .max_content_height(560)
        .child(&list)
        .build();
    card.append(&scroll);
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    let dialog2 = dialog.clone();
    close.connect_clicked(move |_| dialog2.close());
    card.append(&close);
    dialog.present();
}

pub fn toast(message: &str) {
    show_toast(message, None);
}

/// A toast with an Undo button that runs `undo` and closes it.
pub fn toast_with_undo(message: &str, undo: impl Fn() + 'static) {
    show_toast(message, Some(Rc::new(undo)));
}

fn show_toast(message: &str, undo: Option<Rc<dyn Fn()>>) {
    let Some(ui) = ui() else {
        eprintln!("nyx-capture: {message}");
        return;
    };
    let overlay = ui.borrow().overlay.clone();
    if let Some(old) = TOAST.with(|t| t.borrow_mut().take())
        && old.parent().is_some()
    {
        overlay.remove_overlay(&old);
    }
    let label = gtk::Label::new(Some(message));
    label.set_wrap(true);
    label.set_max_width_chars(70);
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bx.add_css_class("toast");
    bx.set_halign(gtk::Align::Center);
    bx.set_valign(gtk::Align::End);
    bx.set_margin_bottom(48);
    bx.append(&label);
    // Only a toast with a button takes clicks; others let them through.
    bx.set_can_target(undo.is_some());
    let stay = if undo.is_some() { UNDO_TOAST } else { std::time::Duration::from_millis(3500) };
    if let Some(run) = undo {
        let b = gtk::Button::with_label("Undo");
        b.add_css_class("toast-action");
        let (o, t) = (overlay.clone(), bx.clone());
        b.connect_clicked(move |_| {
            run();
            if t.parent().is_some() {
                o.remove_overlay(&t);
            }
        });
        bx.append(&b);
    }
    overlay.add_overlay(&bx);
    TOAST.with(|t| *t.borrow_mut() = Some(bx.clone()));
    let overlay = overlay.clone();
    glib::timeout_add_local_once(stay, move || {
        if bx.parent().is_some() {
            overlay.remove_overlay(&bx);
        }
        // Only forget it if a newer toast hasn't taken its place.
        TOAST.with(|t| {
            let mut t = t.borrow_mut();
            if t.as_ref() == Some(&bx) {
                *t = None;
            }
        });
    });
}
