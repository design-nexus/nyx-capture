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
use nyx_capture_core::capture::{CaptureBackend, CaptureMode};
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
    (&["1–8"], "A capture mode, on the Capture page"),
    (&["Ctrl", "Shift", "L"], "Scrolling capture"),
    (&["Ctrl", "N"], "Capture a region"),
    (&["Ctrl", "Shift", "N"], "Capture a window"),
    (&["Ctrl", "Shift", "F"], "Capture the screen"),
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

/// The window's two pages: the Capture page (the modes) and History.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Capture,
    History,
}

struct Ui {
    window: gtk::ApplicationWindow,
    header: chrome::AppHeader,
    overlay: gtk::Overlay,
    tab: Tab,
    pages: gtk::Stack,
    /// The header's Capture and History switch.
    tabs: Vec<(Tab, gtk::ToggleButton)>,
    collapse: gtk::Button,
    nav: gtk::Box,
    nav_list: gtk::Box,
    search: gtk::SearchEntry,
    status: gtk::Label,
    hint: gtk::Label,
    empty: gtk::Box,
    empty_title: gtk::Label,
    empty_go: gtk::Button,
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
        Some(other) => open_section(other),
        None => open_section(&nyx_theme::start_page("capture", &prefs::get().last_section)),
    }
    maybe_tutorial();
}

/// `capture` is the Capture page; the rest are History's sections.
fn open_section(section: &str) {
    if section == "capture" {
        show_capture_page();
    } else {
        navigate(view_from_section(section));
    }
}

/// The Capture page: the modes, their options and the latest captures.
pub fn show_capture_page() {
    let Some(ui) = ui() else { return };
    prefs::update(|p| p.last_section = "capture".into());
    chrome::set_title(&ui.borrow().window, "Capture", "");
    ui.borrow().header.set_subtitle("");
    set_tab(Tab::Capture);
}

fn set_tab(tab: Tab) {
    let Some(ui) = ui() else { return };
    ui.borrow_mut().tab = tab;
    // The switch's handler reads the window: let go of it first.
    let button = ui.borrow().tabs.iter().find(|(t, _)| *t == tab).map(|(_, b)| b.clone());
    if let Some(b) = button {
        b.set_active(true);
    }
    let ui = ui.borrow();
    let history = tab == Tab::History;
    ui.pages.set_visible_child_name(if history { "history" } else { "capture" });
    ui.nav.set_visible(history);
    ui.collapse.set_visible(history);
    let selected = history && crate::grid::has_selection();
    ui.search.set_visible(history && !selected);
    ui.selection.set_visible(selected);
    drop(ui);
    if !history {
        crate::home::refresh_options();
    }
    refresh_status();
}

fn on_history() -> bool {
    ui().is_some_and(|u| u.borrow().tab == Tab::History)
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
    set_tab(Tab::History);
    reload();
    rebuild_nav();
}

pub fn reload() {
    let Some(ui) = ui() else { return };
    crate::home::refresh_recent(&PENDING.with(|p| p.borrow().clone()));
    let view = ui.borrow().view;
    let query = ui.borrow().search.text().to_string();
    if !prefs::get().history {
        ui.borrow_mut().items.clear();
        crate::grid::fill(&[]);
        ui.borrow().scroller.set_visible(false);
        show_empty(&ui.borrow(), "History is off", "New shots still copy or save when you ask.", false);
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
            if empty {
                let searching = !query.trim().is_empty();
                let (title, desc) = if searching { ("Nothing found", "No capture, or text read from one, matches that.") } else { empty_copy(view) };
                show_empty(&ui.borrow(), title, desc, !searching && view != View::Starred);
            } else {
                ui.borrow().empty.set_visible(false);
            }
            refresh_status();
        }
        Err(err) => toast(&err.to_string()),
    }
}

fn empty_copy(view: View) -> (&'static str, &'static str) {
    match view {
        View::Recordings => ("No recordings yet", "Record from the Capture page, or press Ctrl+R."),
        View::Starred => ("Nothing starred yet", "Star a capture from the grid to keep it however old it gets."),
        View::Screenshots => ("No screenshots yet", "Pick a mode on the Capture page, or press Print anywhere."),
        View::Recent => ("No captures yet", "Pick a mode on the Capture page, or press Print anywhere."),
    }
}

/// History's empty state, with a way to the Capture page when there's nothing yet.
fn show_empty(ui: &Ui, title: &str, desc: &str, go: bool) {
    ui.empty_title.set_text(title);
    ui.hint.set_text(desc);
    ui.empty_go.set_visible(go);
    ui.empty.set_visible(true);
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

/// The window's Scrolling capture button, Ctrl+Shift+L and `--scroll`: the capture bar
/// on Scroll.
pub fn start_scroll() {
    if record::active().is_some() {
        toast("Stop the recording first");
        return;
    }
    if let Some(app) = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) {
        capture_with_bar(&app, crate::overlay::Mode::Scroll);
    }
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
    // The Capture page says how many there are under Recent; only a recording here.
    let mut text = if ui.borrow().tab == Tab::Capture {
        String::new()
    } else if !prefs::get().history {
        "History off".into()
    } else if count == 0 {
        String::new()
    } else if count == 1 {
        "1 capture".into()
    } else {
        format!("{count} captures")
    };
    let selected = crate::grid::selected().len();
    if selected > 0 && ui.borrow().tab == Tab::History {
        text = format!("{text} · {selected} selected");
    }
    if let Some(session) = record::active() {
        let state = if session.paused { "Paused" } else { "Recording" };
        text = if text.is_empty() { state.into() } else { format!("{state} · {text}") };
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

    // The app and History's section, its sidebar toggle; the Capture and History
    // switch in the middle, with History's search (or the selection); what's going on,
    // a recording's controls and Settings at the end.
    let header = chrome::app_header(&window, "capture-region-symbolic", "Capture");
    let collapse = widgets::bar_button("sidebar-show-symbolic", "Collapse sidebar (Ctrl+B)");
    collapse.connect_clicked(|_| toggle_sidebar());
    header.start.append(&collapse);

    let switch = widgets::segmented(&widgets::opts(&[("capture", "Capture"), ("history", "History")]), "capture", |id| {
        let tab = if id == "history" { Tab::History } else { Tab::Capture };
        let now = ui().map(|u| u.borrow().tab);
        if now == Some(tab) {
            return;
        }
        match tab {
            Tab::History => {
                let view = ui().map(|u| u.borrow().view).unwrap_or(View::Recent);
                navigate(view);
            }
            Tab::Capture => show_capture_page(),
        }
    });
    switch.add_css_class("view-switch");
    let mut tabs = Vec::new();
    let mut child = switch.first_child();
    for tab in [Tab::Capture, Tab::History] {
        let Some(c) = child else { break };
        if let Ok(b) = c.clone().downcast::<gtk::ToggleButton>() {
            tabs.push((tab, b));
        }
        child = c.next_sibling();
    }
    header.center.append(&switch);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search captures (Ctrl+F)"));
    search.add_css_class("bar-search");
    // A small minimum keeps the header narrow enough for a half-screen tile.
    search.set_width_chars(8);
    search.set_max_width_chars(SEARCH_WIDE);
    search.connect_search_changed(|_| reload());
    search.connect_stop_search(|entry| entry.set_text(""));
    header.center.append(&search);

    let empty = widgets::vbox(8);
    empty.add_css_class("empty-state");
    empty.set_valign(gtk::Align::Center);
    empty.set_halign(gtk::Align::Center);
    empty.set_vexpand(true);
    let empty_icon = gtk::Image::from_icon_name("capture-region-symbolic");
    empty_icon.set_pixel_size(48);
    empty_icon.add_css_class("dim");
    empty.append(&empty_icon);
    let empty_title = widgets::label("No captures yet", "empty-title");
    empty_title.set_xalign(0.5);
    empty.append(&empty_title);
    let hint = widgets::label("", "dim");
    hint.set_xalign(0.5);
    hint.set_wrap(true);
    hint.set_justify(gtk::Justification::Center);
    hint.set_max_width_chars(52);
    empty.append(&hint);
    let empty_go = gtk::Button::with_label("Take a capture");
    empty_go.add_css_class("suggested-action");
    empty_go.set_halign(gtk::Align::Center);
    empty_go.set_margin_top(8);
    empty_go.connect_clicked(|_| show_capture_page());
    empty.append(&empty_go);
    empty.set_visible(false);

    let scroller = crate::grid::build();

    // The capture modes are on the Capture page; the header keeps a recording's Stop
    // and Pause while one runs, and the webcam overlay's switch.
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let tools = nyx_capture_core::detect::Tools::probe();
    if tools.can_record() {
        let rec = widgets::bar_button("media-playback-stop-symbolic", "Stop recording (Ctrl+R)");
        rec.add_css_class("destructive-action");
        rec.set_visible(false);
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
        let cam = widgets::bar_button("camera-web-symbolic", "Show webcam");
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
    let delete = widgets::bar_button("user-trash-symbolic", "Delete selected (Delete)");
    delete.connect_clicked(|_| delete_later(crate::grid::selected()));
    let deselect = widgets::bar_button("edit-clear-symbolic", "Clear selection (Esc)");
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
    content.append(&empty);
    content.append(&scroller);

    let pages = gtk::Stack::new();
    pages.set_hhomogeneous(false);
    pages.set_vhomogeneous(false);
    pages.add_named(&crate::home::build(), Some("capture"));
    pages.add_named(&content, Some("history"));

    let frame = nyx_ui::frame::AppFrame::new(&header.root);
    frame.set_sidebar(&nav);
    frame.set_page(&pages);
    let overlay = frame.install(&window);
    install_keys(&window, &search);

    let ui = Ui {
        window: window.clone(),
        header,
        overlay: overlay.clone(),
        tab: Tab::Capture,
        pages,
        tabs,
        collapse,
        nav: nav.clone(),
        nav_list: list,
        search,
        status,
        hint,
        empty,
        empty_title,
        empty_go,
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
            crate::home::set_narrow(narrow);
            if let Some(ui) = self::ui() {
                let ui = ui.borrow();
                // The centred switch and search mustn't reach the app's name or the actions.
                ui.search.set_max_width_chars(if narrow { SEARCH_NARROW } else { SEARCH_WIDE });
                ui.status.set_visible(!narrow);
                set_app_name_visible(ui.header.root.upcast_ref(), !narrow);
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

/// The header's app name, hidden under 980 px like the sidebar's labels; the icon stays.
fn set_app_name_visible(widget: &gtk::Widget, visible: bool) {
    if widget.has_css_class("app-title") {
        widget.set_visible(visible);
        return;
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        set_app_name_visible(&c, visible);
        child = c.next_sibling();
    }
}

/// A card on the Capture page, or its number key: the capture bar on that mode, as the
/// Print key opens it; without the overlay, the nearest capture that runs without it.
pub fn start_mode(mode: crate::overlay::Mode) {
    use crate::overlay::Mode;
    let app = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok());
    let bar = crate::overlay::prefer() && crate::overlay::supported();
    // The portal takes its own screenshots, so shots skip the bar's frozen still.
    let portal = nyx_capture_core::capture::pick(&prefs::get().backend) == "portal";
    match mode {
        Mode::Record => {
            toggle_record(false);
            return;
        }
        Mode::Scroll => {
            start_scroll();
            return;
        }
        Mode::Smart | Mode::Window | Mode::Screen if portal => {}
        _ if bar => {
            if let Some(app) = &app {
                capture_with_bar(app, mode);
            }
            return;
        }
        _ => {}
    }
    let p = prefs::get();
    let after = if p.capture_edit { After::Edit } else { after_from_prefs() };
    let delay = if p.capture_delay { 3 } else { 0 };
    match mode {
        Mode::Window => capture_with(CaptureMode::SelectWindow, after, delay),
        Mode::Screen => capture_with(CaptureMode::FullDesktop, after, delay),
        Mode::Text => capture_with(CaptureMode::SelectRegion, After::Ocr, delay),
        Mode::Qr => capture_with(CaptureMode::SelectRegion, After::Qr, 0),
        Mode::Colour => toast("Picking a colour needs the capture overlay"),
        _ => capture_with(CaptureMode::SelectRegion, after, delay),
    }
}

/// The window's capture keys (and the Assistant's): the capture bar on that mode,
/// as the Print key opens it, where the overlay can run.
pub fn capture_now(mode: CaptureMode) {
    let start = match mode {
        CaptureMode::SelectRegion => Some(crate::overlay::Mode::Smart),
        CaptureMode::SelectWindow => Some(crate::overlay::Mode::Window),
        CaptureMode::FullDesktop => Some(crate::overlay::Mode::Screen),
        _ => None,
    };
    let app = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok());
    if let (Some(start), Some(app)) = (start, app)
        && crate::overlay::prefer()
        && crate::overlay::supported()
        && nyx_capture_core::capture::pick(&prefs::get().backend) != "portal"
    {
        capture_with_bar(&app, start);
        return;
    }
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

/// How long to wait after hiding the history window before taking the still.
const HIDE_WAIT_MS: u64 = 450;

/// The Print key (`--capture`): the overlay with the capture bar, then what Settings →
/// Capture says to do with the shot. Pressed again while choosing, it closes the overlay.
pub fn capture_from_key(app: &gtk::Application, mode: Option<&str>) {
    let mode = mode.map(str::to_string).unwrap_or_else(|| prefs::get().capture_start);
    capture_with_bar(app, crate::overlay::Mode::parse(&mode));
}

/// `nyx-capture FILE`: the image in the editor, outside history.
pub fn edit_file(path: PathBuf) {
    let hold = gio::Application::default().map(|a| a.hold());
    let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    crate::cmd::background(
        move || image::open(&path).map(|i| i.to_rgba8()).map_err(|e| anyhow::anyhow!("{}: {e}", path.display())),
        move |result| {
            match result {
                Ok(img) => crate::editor::open_image(img, &title),
                Err(err) => open_failed(&err),
            }
            drop(hold);
        },
    );
}

/// `nyx-capture --clipboard`: the copied image in the editor, outside history.
pub fn edit_clipboard() {
    let hold = gio::Application::default().map(|a| a.hold());
    crate::cmd::background(
        || clipboard::paste_image().map(|i| i.to_rgba8()),
        move |result| {
            match result {
                Ok(img) => crate::editor::open_image(img, "Clipboard image"),
                Err(err) => open_failed(&err),
            }
            drop(hold);
        },
    );
}

/// An image that couldn't be opened: said in the window when it's up, else on screen.
fn open_failed(err: &anyhow::Error) {
    crate::naming::log_error("open", err);
    if window().is_some_and(|w| w.is_visible()) {
        toast(&err.to_string());
    } else if let Ok((_, area)) = crate::overlay::focused() {
        crate::notice::show(area, &err.to_string(), None, None);
    }
}

/// The capture bar on `mode`, from inside the app (Assistant).
pub fn open_bar(mode: &str) {
    if let Some(app) = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) {
        capture_with_bar(&app, crate::overlay::Mode::parse(mode));
    }
}

/// The capture bar starting on `start`: the Print key, and the window's capture buttons.
fn capture_with_bar(app: &gtk::Application, start: crate::overlay::Mode) {
    if crate::overlay::is_open() {
        crate::overlay::dismiss();
        return;
    }
    // The key again finishes what it started.
    if crate::scrolling::is_active() {
        crate::scrolling::done();
        return;
    }
    if record::active().is_some() {
        stop_recording();
        return;
    }
    let hold = app.hold();
    let win = window();
    let shown = win.as_ref().is_some_and(|w| w.is_visible());
    if let Some(w) = &win {
        w.set_visible(false);
    }
    let cards = crate::card::hide_all();
    let restore: Rc<dyn Fn()> = Rc::new(move || {
        crate::card::unhide_all();
        if shown && let Some(w) = &win {
            w.present();
        }
    });
    // The history window and the cards must be gone from the screen before the still,
    // including Hyprland's animations: the windows left behind resize to fill the gap
    // (Omarchy: about 380 ms) and layers fade (about 140 ms).
    let wait = if shown { HIDE_WAIT_MS } else if cards { 200 } else { 0 };
    glib::timeout_add_local_once(std::time::Duration::from_millis(wait), move || {
        let still = match crate::overlay::freeze() {
            Ok(still) => still,
            Err(err) => {
                crate::naming::log_error("capture", &err);
                restore();
                return;
            }
        };
        let frozen = still.clone();
        let on_cancel = restore.clone();
        let picked = crate::overlay::pick_with_bar(still, start, move |picked| match picked {
            Some(picked) => take_picked(frozen, picked, restore, hold),
            None => restore(),
        });
        if let Err(err) = picked {
            crate::naming::log_error("capture", &err);
            on_cancel();
        }
    });
}

/// The shot itself: cropped from the still, or taken fresh after the 3 s wait; or, in
/// Scroll mode, stitched while the page scrolls; or, in Record mode, a recording.
fn take_picked(still: nyx_capture_core::capture::Shot, picked: crate::overlay::Picked, restore: Rc<dyn Fn()>, hold: gio::ApplicationHoldGuard) {
    let rect = picked.rect;
    match picked.mode {
        crate::overlay::Mode::Scroll => {
            let on_cancel = restore.clone();
            let started = crate::scrolling::start(rect, move |shot| match shot {
                Some(shot) => deliver_shot(shot, picked, restore, hold),
                None => restore(),
            });
            if let Err(err) = started {
                crate::naming::log_error("scroll", &err);
                on_cancel();
            }
            return;
        }
        crate::overlay::Mode::Record => {
            crate::recording::start(&picked, still.area, restore, hold);
            return;
        }
        crate::overlay::Mode::Colour => {
            restore();
            if let Some(rgb) = picked.colour {
                copy_colour(rgb, picked.rect);
            }
            drop(hold);
            return;
        }
        _ => {}
    }
    let delay = picked.extras.delay;
    let deliver = move |result: anyhow::Result<nyx_capture_core::capture::Shot>| match result {
        Ok(shot) => deliver_shot(shot, picked, restore, hold),
        Err(err) => {
            crate::naming::log_error("capture", &err);
            restore();
        }
    };
    if !delay {
        crate::cmd::background(move || still.crop_layout(rect), deliver);
        return;
    }
    let started = crate::overlay::countdown(3, move |go| {
        if go {
            crate::cmd::background(
                move || nyx_capture_core::capture::Grim.capture(&nyx_capture_core::capture::CaptureRequest { mode: CaptureMode::Region(rect) }),
                deliver,
            );
        }
    });
    if let Err(err) = started {
        crate::naming::log_error("capture", &err);
    }
}

/// What Settings → Capture (or the bar's Edit and Text) says to do with a shot.
fn deliver_shot(shot: nyx_capture_core::capture::Shot, picked: crate::overlay::Picked, restore: Rc<dyn Fn()>, hold: gio::ApplicationHoldGuard) {
    let title = if !picked.title.trim().is_empty() {
        picked.title.trim().chars().take(120).collect()
    } else if picked.app.is_empty() {
        "Screen".to_string()
    } else {
        String::new()
    };
    LAST_TITLE.with(|t| *t.borrow_mut() = title.clone());
    let after = match picked.mode {
        crate::overlay::Mode::Text => After::Ocr,
        crate::overlay::Mode::Qr => After::Qr,
        _ if picked.extras.edit => After::Edit,
        _ => after_from_prefs(),
    };
    if after == After::Qr {
        restore();
        read_qr(shot, picked.rect, Some(hold));
        return;
    }
    if after == After::Ocr {
        restore();
        remember(&shot);
        read_text(shot, Some(hold));
        return;
    }
    if after == After::Edit {
        crate::card::unhide_all();
        // The editor replaces the history window until it closes.
        RESTORE_AFTER_EDIT.with(|r| *r.borrow_mut() = Some(restore));
        crate::editor::open(shot);
        drop(hold);
        return;
    }
    restore();
    remember(&shot);
    if after == After::Pin {
        crate::pin::open_shot(shot);
        drop(hold);
        return;
    }
    let path = matches!(after, After::Quick | After::Save).then(|| crate::naming::next_path(&picked.app));
    let tag = (picked.mode == crate::overlay::Mode::Scroll).then_some("Scrolled");
    let work_shot = shot.clone();
    let work_path = path.clone();
    crate::cmd::background(
        move || -> anyhow::Result<crate::card::Done> {
            let png = work_shot.png()?;
            if let Some(path) = &work_path {
                nyx_capture_core::atomic_write(path, &png)?;
            }
            Ok(match after {
                After::Copy => {
                    clipboard::copy_png(&png)?;
                    crate::card::Done::Copied
                }
                After::Save => crate::card::Done::Saved,
                _ => {
                    clipboard::copy_png(&png)?;
                    crate::card::Done::CopiedSaved
                }
            })
        },
        move |result| {
            match result {
                Ok(done) => crate::card::show(shot, path, done, tag),
                Err(err) => {
                    crate::naming::log_error("capture", &err);
                    toast(&err.to_string());
                }
            }
            drop(hold);
        },
    );
}

/// Text mode: read the shot, then copy the text (joined into paragraphs unless line
/// breaks are kept) with a card, or show it first when Review is on.
fn read_text(shot: nyx_capture_core::capture::Shot, hold: Option<gio::ApplicationHoldGuard>) {
    let p = prefs::get();
    let (langs, mode, keep, review) = (p.ocr_langs, OcrMode::parse(&p.ocr_mode), p.ocr_keep_lines, p.ocr_review);
    let image = shot.image.clone();
    crate::cmd::background(
        move || -> anyhow::Result<nyx_capture_core::OcrResult> {
            let mut r = recognize(&image, mode, &langs)?;
            if !keep {
                r.text = nyx_capture_core::ocr::unwrap_lines(&r.text);
            }
            if !review && !r.text.trim().is_empty() {
                clipboard::copy_text(&r.text)?;
            }
            Ok(r)
        },
        move |result| {
            match result {
                Ok(r) if r.text.trim().is_empty() => crate::notice::show(shot.area, "No text found", None, None),
                Ok(r) if review => crate::ocr_view::show(None, &shot.image.to_rgba8(), &r),
                Ok(_) => crate::card::show(shot, None, crate::card::Done::TextCopied, None),
                Err(err) => {
                    crate::naming::log_error("text", &err);
                    crate::notice::show(shot.area, &err.to_string(), None, None);
                }
            }
            drop(hold);
        },
    );
}

/// QR mode: decode the shot and copy what it says, marked sensitive. Never kept, logged
/// or shown: the note names only its kind.
fn read_qr(shot: nyx_capture_core::capture::Shot, area: nyx_capture_core::geom::Rect, hold: Option<gio::ApplicationHoldGuard>) {
    crate::cmd::background(
        move || -> anyhow::Result<Option<String>> {
            let text = nyx_capture_core::qr::decode(&shot.image);
            if let Some(t) = &text {
                clipboard::copy_secret(t)?;
            }
            Ok(text)
        },
        move |result| {
            match result {
                Ok(Some(text)) => announce_qr(&text, area),
                Ok(None) => crate::notice::show(area, "No QR code found", None, None),
                Err(err) => crate::notice::show(area, &err.to_string(), None, None),
            }
            drop(hold);
        },
    );
}

/// The note for a copied QR code, with Open for a link (opened at once when Open links
/// is on).
fn announce_qr(text: &str, area: nyx_capture_core::geom::Rect) {
    let kind = nyx_capture_core::qr::classify(text);
    let link = nyx_capture_core::qr::link(text).map(str::to_string);
    let open = |uri: &str| {
        if let Err(err) = gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>) {
            crate::naming::log_error("qr", &anyhow::anyhow!("{err}"));
        }
    };
    if let Some(uri) = &link
        && prefs::get().qr_open_links
    {
        open(uri);
        crate::notice::show(area, &format!("{} · opened", kind.copied()), None, None);
        return;
    }
    let action = link.map(|uri| crate::notice::Action { label: "Open", run: Rc::new(move || open(&uri)) });
    crate::notice::show(area, &kind.copied(), None, action);
}

/// Colour mode: copy the pixel's value in the chosen format.
fn copy_colour(rgb: [u8; 3], area: nyx_capture_core::geom::Rect) {
    let value = nyx_capture_core::colour::format(rgb, nyx_capture_core::colour::Format::parse(&prefs::get().colour_format));
    match clipboard::copy_text(&value) {
        Ok(()) => crate::notice::show(area, &format!("Copied {value}"), Some(rgb), None),
        Err(err) => crate::notice::show(area, &err.to_string(), None, None),
    }
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
    // capture waits HIDE_WAIT_MS for it, and the windows resizing into its place, to settle).
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
        glib::timeout_add_local_once(std::time::Duration::from_millis(HIDE_WAIT_MS), move || {
            start_overlay(mode, after, langs, ocr_mode, save_path, restore);
        });
        return;
    }
    let langs_ui = langs.clone();
    let save_done = save_path.clone();
    crate::cmd::background(
        move || {
            std::thread::sleep(std::time::Duration::from_millis(HIDE_WAIT_MS));
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
        "quick" => After::Quick,
        "pin" => After::Pin,
        _ => After::Edit,
    }
}

fn save_file() -> Option<PathBuf> {
    Some(crate::naming::next_path(""))
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
        After::Qr => {
            let area = shot.area;
            read_qr(shot, area, None);
        }
        After::Ocr => {
            remember(&shot);
            read_text(shot, None);
        }
        After::Pin => {
            remember(&shot);
            crate::pin::open_shot(shot);
        }
        After::Quick => {
            remember(&shot);
            let card_shot = shot.clone();
            crate::cmd::background(
                move || {
                    let action = Action { mode: CaptureMode::FullDesktop, after: After::Quick, delay: 0, save_path, ocr_langs: langs, ocr_mode, backend: String::new() };
                    action::finish(&shot, &action)
                },
                move |result| match result {
                    Ok(action::Outcome::Quick(path)) => crate::card::show(card_shot, Some(path), crate::card::Done::CopiedSaved, None),
                    other => report_capture(other),
                },
            );
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
    let overlay_mode = crate::overlay::mode_for(&mode, after);
    if crate::overlay::supported() {
        match crate::overlay::freeze() {
            Ok(still) => {
                let cropped = still.clone();
                let restore_done = restore.clone();
                let langs_done = langs.clone();
                let save_picked = save_path.clone();
                match crate::overlay::pick_in(still, overlay_mode, move |picked| {
                    let Some(rect) = picked.map(|p| p.rect) else {
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
    remove_history(move |db| db.prune(days));
}

/// Forget every unstarred screenshot and the text read from it.
pub fn clear_history() {
    remove_history(|db| db.clear());
}

fn remove_history(remove: impl FnOnce(&store::Db) -> anyhow::Result<Vec<std::path::PathBuf>> + Send + 'static) {
    crate::cmd::background(
        move || {
            let db = store::open(&paths::library_db())?;
            remove(&db)
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

/// The window's record button, Ctrl+R and Ctrl+Shift+R: stop a recording, or open the
/// capture bar on Record (where the overlay can run).
pub fn toggle_record(region: bool) {
    if record::active().is_some() {
        stop_recording();
        return;
    }
    let app = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok());
    if let Some(app) = app
        && crate::overlay::prefer()
        && crate::overlay::supported()
        && record::pick(&prefs::get().recorder) != Some(nyx_capture_core::RecordBackend::GstPortal)
    {
        capture_with_bar(&app, crate::overlay::Mode::Record);
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
    crate::recording::stop(false);
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

pub(crate) fn record_request(target: Target, output: PathBuf) -> RecordRequest {
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
pub(crate) fn record_output_path() -> PathBuf {
    let container = Container::parse(&prefs::get().record_container);
    record::output_path(&record::default_output_dir(), container)
}

fn launch_record(target: Target) {
    let area = match &target {
        Target::Region(r) => Some(*r),
        _ => None,
    };
    let req = record_request(target, record_output_path());
    let p = prefs::get();
    let cam = p.record_webcam;
    let cam_device = p.record_webcam_device;
    let cam_size = p.record_webcam_size;
    crate::cmd::background(
        move || start_record_session(&req, cam, &cam_device, &cam_size),
        move |result| match result {
            Ok(_) => {
                refresh_record_button();
                refresh_cam_button();
                crate::recording::show_bar(area);
            }
            Err(err) => toast(&err.to_string()),
        },
    );
}

pub(crate) fn start_record_session(
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

pub(crate) fn remember_recording(path: &std::path::Path) {
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

pub(crate) fn refresh_record_button() {
    // A recorder that ended by itself (crashed, killed) still leaves its file for history.
    if let Some(path) = record::ended() {
        remember_recording(&path);
        toast(&format!("The recording ended; saved {}", path.display()));
    }
    let recording = record::active().is_some();
    RECORD_BTN.with(|b| {
        if let Some(btn) = b.borrow().as_ref() {
            btn.set_visible(recording);
        }
    });
    crate::home::set_recording(recording);
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

pub(crate) fn refresh_cam_button() {
    CAM_BTN.with(|b| {
        let Some(btn) = b.borrow().clone() else { return };
        if nyx_capture_core::webcam::active() {
            btn.add_css_class("suggested-action");
            btn.set_tooltip_text(Some("Hide webcam"));
        } else {
            btn.remove_css_class("suggested-action");
            btn.set_tooltip_text(Some("Show webcam"));
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
    let history = ui.borrow().tab == Tab::History;
    ui.borrow().selection.set_visible(history && n > 0);
    ui.borrow().search.set_visible(history && n == 0);
    ui.borrow().selection_count.set_text(&format!("{n} selected"));
    refresh_status();
}

fn report_capture(result: Result<action::Outcome, anyhow::Error>) {
    match result {
        Ok(action::Outcome::Cancelled) => {}
        Ok(action::Outcome::Copied) => toast("Copied"),
        Ok(action::Outcome::Saved(path)) => toast(&format!("Saved {}", path.display())),
        Ok(action::Outcome::Quick(path)) => toast(&format!("Copied and saved {}", path.display())),
        Ok(action::Outcome::Text(_)) => toast("Copied text"),
        Ok(action::Outcome::Qr(text)) => toast(&nyx_capture_core::qr::classify(&text).copied()),
        Ok(action::Outcome::Colour(value)) => toast(&format!("Copied {value}")),
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
            if !on_history() {
                let view = ui().map(|u| u.borrow().view).unwrap_or(View::Recent);
                navigate(view);
            }
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
        let plain = !mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK);
        if !on_history() {
            if plain && let Some(mode) = crate::home::mode_for_key(key) {
                start_mode(mode);
                return glib::Propagation::Stop;
            }
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
