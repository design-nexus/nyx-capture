//! Fullscreen still overlay: region/window pick, magnifier, dimensions, keys.
//!
//! Layer Shell on Hyprland. slurp if the compositor has no layer-shell.

use crate::prefs;
use gtk::cairo::{self, Context, Format, ImageSurface};
use gtk::gdk::{self, prelude::*};
use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use nyx_capture_core::capture::{CaptureBackend, CaptureMode, CaptureRequest, Grim, Shot};
use nyx_capture_core::geom::Rect;
use nyx_capture_core::select;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// A click that covers less than this many layout pixels is a snap, not a drag.
const CLICK_AREA: i64 = 20;
const MAG_SIZE: f64 = 116.0;
const MAG_ZOOM: f64 = 4.0;

/// What a click or drag captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Drag a region, click a window, or click empty space for the screen.
    Smart,
    /// Click a window.
    Window,
    /// The whole monitor.
    Screen,
    /// An area (or a window) scrolled through and stitched into one tall shot.
    Scroll,
    /// An area, a window or the whole monitor, recorded.
    Record,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "window" => Mode::Window,
            "screen" => Mode::Screen,
            "scroll" => Mode::Scroll,
            "record" => Mode::Record,
            _ => Mode::Smart,
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Mode::Smart => "Drag a region · click a window · click empty space for the screen",
            Mode::Window => "Click a window",
            Mode::Screen => "Click or press Enter for the whole screen",
            Mode::Scroll => "Drag the area to scroll through, or click a window",
            Mode::Record => "Drag an area · click a window · click empty space for the screen",
        }
    }

    /// Modes where a drag draws a region.
    fn drags(self) -> bool {
        matches!(self, Mode::Smart | Mode::Scroll | Mode::Record)
    }
}

/// The capture bar's choices for this shot only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extras {
    pub edit: bool,
    pub text: bool,
    pub delay: bool,
}

/// What was chosen: the area, the window class under it, and the bar's choices.
#[derive(Clone, Debug)]
pub struct Picked {
    pub rect: Rect,
    /// Window class, empty for a screen.
    pub app: String,
    /// Window title, empty for a screen.
    pub title: String,
    pub extras: Extras,
    /// What to do with the area: a shot, a scrolling capture, or a recording.
    pub mode: Mode,
}

/// A window (with its class) or a monitor (empty class) the pointer can light.
#[derive(Clone)]
struct Target {
    rect: Rect,
    class: String,
    title: String,
    window: bool,
}

struct Bar {
    modes: Vec<(Mode, gtk::ToggleButton)>,
    edit: gtk::ToggleButton,
    text: gtk::ToggleButton,
    delay: gtk::ToggleButton,
    /// The recording options, shown instead of Text and Delay in Record mode.
    record: gtk::Box,
    hint: gtk::Label,
}

impl Bar {
    /// Each mode shows the choices that apply to it.
    fn show_for(&self, mode: Mode) {
        let shot = matches!(mode, Mode::Smart | Mode::Window | Mode::Screen);
        self.edit.set_visible(mode != Mode::Record);
        self.text.set_visible(shot);
        self.delay.set_visible(shot);
        self.record.set_visible(mode == Mode::Record);
        self.hint.set_text(mode.hint());
    }
}

struct Session {
    still: Shot,
    surface: ImageSurface,
    mode: Mode,
    extras: Extras,
    dragging: bool,
    origin: Option<(i32, i32)>,
    current: Option<Rect>,
    hinted: Option<Target>,
    cursor: (i32, i32),
    targets: Vec<Target>,
    canvases: Vec<gtk::DrawingArea>,
    windows: Vec<gtk::Window>,
    bar: Option<Bar>,
    on_pick: Option<Box<dyn FnOnce(Option<Picked>)>>,
}

impl Session {
    /// What the pointer would capture now, in this mode.
    fn target_at(&self, pt: (i32, i32)) -> Option<Target> {
        self.targets
            .iter()
            .filter(|t| t.rect.contains(pt.0, pt.1))
            .filter(|t| match self.mode {
                Mode::Smart | Mode::Scroll | Mode::Record => true,
                Mode::Window => t.window,
                Mode::Screen => !t.window,
            })
            .min_by_key(|t| t.rect.area())
            .cloned()
    }

    /// The window under the middle of `r` (class, title), for the file name and the card.
    fn window_under(&self, r: Rect) -> (String, String) {
        let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
        self.targets
            .iter()
            .filter(|t| t.window && t.rect.contains(cx, cy))
            .min_by_key(|t| t.rect.area())
            .map(|t| (t.class.clone(), t.title.clone()))
            .unwrap_or_default()
    }

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.dragging = false;
        self.origin = None;
        self.current = None;
        self.hinted = self.target_at(self.cursor);
        if let Some(bar) = &self.bar {
            bar.show_for(mode);
        }
    }

    /// The bar's button for `mode`, to select after the session is released (its
    /// toggled handler reads the session).
    fn mode_button(&self, mode: Mode) -> Option<gtk::ToggleButton> {
        self.bar.as_ref()?.modes.iter().find(|(m, _)| *m == mode).map(|(_, b)| b.clone())
    }
}

type DelayDone = Box<dyn FnOnce(bool)>;

struct DelayUi {
    window: gtk::Window,
    label: gtk::Label,
    remaining: Cell<u32>,
    tick: RefCell<Option<glib::SourceId>>,
    on_done: RefCell<Option<DelayDone>>,
}

thread_local! {
    static SESSION: RefCell<Option<Rc<RefCell<Session>>>> = const { RefCell::new(None) };
    static THEME: Cell<bool> = const { Cell::new(false) };
    static DISMISSING: Cell<bool> = const { Cell::new(false) };
    static DELAY: RefCell<Option<Rc<DelayUi>>> = const { RefCell::new(None) };
    static DELAY_DISMISSING: Cell<bool> = const { Cell::new(false) };
}

/// `auto` and `overlay` try the GTK selector; `slurp` skips it.
pub fn prefer() -> bool {
    prefs::get().selector != "slurp"
}

pub fn supported() -> bool {
    gtk4_layer_shell::is_supported()
}

/// A still of the focused monitor, which the overlay then covers. grim can crash
/// capturing every output at once.
pub fn freeze() -> anyhow::Result<Shot> {
    let name = test_monitor().or_else(|| select::focused_monitor().ok().flatten().map(|m| m.name));
    let mode = match name {
        Some(name) => CaptureMode::Monitor(name),
        None => CaptureMode::FullDesktop,
    };
    Grim.capture(&CaptureRequest { mode })
}

/// Debug builds on a virtual monitor.
pub fn testing() -> bool {
    test_monitor().is_some()
}

/// GTK + theme for the cold `--region` path, which never opens the history window.
pub fn ensure_gtk() -> anyhow::Result<()> {
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: still single-threaded; nothing else reads the environment yet.
        unsafe { std::env::set_var("GSK_RENDERER", "ngl") };
    }
    gtk::init()?;
    if !THEME.with(|t| t.get()) {
        crate::theme::install();
        THEME.with(|t| t.set(true));
    }
    Ok(())
}

pub fn mark_theme_installed() {
    THEME.with(|t| t.set(true));
}

/// Seconds on a small overlay, then `on_done(true)`. Esc calls `on_done(false)`.
pub fn countdown(secs: u32, on_done: impl FnOnce(bool) + 'static) -> anyhow::Result<()> {
    if secs == 0 {
        on_done(true);
        return Ok(());
    }
    finish_delay(false);
    if !supported() {
        let left = Rc::new(Cell::new(secs));
        let done = Rc::new(RefCell::new(Some(on_done)));
        glib::timeout_add_seconds_local(1, move || {
            let n = left.get().saturating_sub(1);
            left.set(n);
            if n == 0 {
                if let Some(cb) = done.borrow_mut().take() {
                    cb(true);
                }
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        return Ok(());
    }
    let monitor = focused_output()?;
    let window = gtk::Window::new();
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("nyx-capture-delay"));
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_anchor(Edge::Top, true);
    window.set_margin(Edge::Top, 56);
    let g = monitor.geometry();
    window.set_margin(Edge::Left, ((g.width() - 168) / 2).max(0));
    window.set_exclusive_zone(0);
    window.set_monitor(Some(&monitor));
    window.set_default_size(168, 108);
    window.set_size_request(168, 108);
    window.add_css_class("capture-countdown");
    window.set_decorated(false);
    window.set_resizable(false);
    window.set_deletable(false);
    if let Some(app) = gtk::gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) {
        window.set_application(Some(&app));
    }

    let label = gtk::Label::new(Some(&secs.to_string()));
    label.add_css_class("capture-countdown-num");
    let hint = gtk::Label::new(Some("Esc cancels"));
    hint.add_css_class("dim");
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("capture-countdown-card");
    card.set_halign(gtk::Align::Center);
    card.set_valign(gtk::Align::Center);
    card.append(&label);
    card.append(&hint);
    window.set_child(Some(&card));

    let ui = Rc::new(DelayUi {
        window: window.clone(),
        label,
        remaining: Cell::new(secs),
        tick: RefCell::new(None),
        on_done: RefCell::new(Some(Box::new(on_done))),
    });
    DELAY.with(|d| *d.borrow_mut() = Some(ui.clone()));

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            finish_delay(false);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);
    window.connect_close_request(|_| {
        if !DELAY_DISMISSING.get() {
            finish_delay(false);
        }
        glib::Propagation::Proceed
    });

    let tick_ui = ui.clone();
    let id = glib::timeout_add_seconds_local(1, move || {
        let n = tick_ui.remaining.get().saturating_sub(1);
        tick_ui.remaining.set(n);
        if n == 0 {
            finish_delay(true);
            glib::ControlFlow::Break
        } else {
            tick_ui.label.set_text(&n.to_string());
            glib::ControlFlow::Continue
        }
    });
    ui.tick.replace(Some(id));
    window.present();
    Ok(())
}

/// CLI wait. Sleeps when layer-shell is missing. False means the user cancelled.
pub fn run_countdown_blocking(secs: u32) -> bool {
    if secs == 0 {
        return true;
    }
    if ensure_gtk().is_err() || !supported() {
        std::thread::sleep(std::time::Duration::from_secs(secs as u64));
        return true;
    }
    let ok = Rc::new(Cell::new(true));
    let main = glib::MainLoop::new(None, false);
    let ok2 = ok.clone();
    let main2 = main.clone();
    match countdown(secs, move |proceed| {
        ok2.set(proceed);
        main2.quit();
    }) {
        Ok(()) => {
            main.run();
            ok.get()
        }
        Err(_) => {
            std::thread::sleep(std::time::Duration::from_secs(secs as u64));
            true
        }
    }
}

fn finish_delay(proceed: bool) {
    if DELAY_DISMISSING.get() {
        return;
    }
    let Some(ui) = DELAY.with(|d| d.borrow_mut().take()) else {
        return;
    };
    DELAY_DISMISSING.set(true);
    if let Some(id) = ui.tick.borrow_mut().take() {
        id.remove();
    }
    let cb = ui.on_done.borrow_mut().take();
    dismiss_windows(vec![ui.window.clone()]);
    glib::idle_add_local_once(move || {
        DELAY_DISMISSING.set(false);
        if let Some(cb) = cb {
            cb(proceed);
        }
    });
}

/// Show the overlay and invoke `on_pick` with a layout rect, or `None` on cancel.
pub fn pick(still: Shot, snap: bool, on_pick: impl FnOnce(Option<Rect>) + 'static) -> anyhow::Result<()> {
    open(still, if snap { Mode::Window } else { Mode::Smart }, false, Box::new(move |p: Option<Picked>| on_pick(p.map(|p| p.rect))))
}

/// The Print key's overlay: the capture bar at the bottom, starting on `start`.
pub fn pick_with_bar(still: Shot, start: Mode, on_pick: impl FnOnce(Option<Picked>) + 'static) -> anyhow::Result<()> {
    open(still, start, true, Box::new(on_pick))
}

fn open(still: Shot, mode: Mode, with_bar: bool, on_pick: Box<dyn FnOnce(Option<Picked>)>) -> anyhow::Result<()> {
    if SESSION.with(|s| s.borrow().is_some()) {
        finish(None);
    }
    // Only the monitors the still shows get an overlay.
    let mut outputs = outputs()?;
    outputs.retain(|(_, r)| intersection(*r, still.area).is_some());
    if outputs.is_empty() {
        anyhow::bail!("no monitors");
    }
    let surface = still_surface(&still).ok_or_else(|| anyhow::anyhow!("the freeze could not be drawn"))?;
    let mut targets: Vec<Target> = outputs.iter().map(|(_, r)| Target { rect: *r, class: String::new(), title: String::new(), window: false }).collect();
    if let Ok(list) = select::windows() {
        targets.extend(list.into_iter().filter(|w| !w.hidden).map(|w| Target { rect: w.geometry, class: w.class, title: w.title, window: true }));
    }
    let targets: Vec<Target> = targets.into_iter().filter_map(|t| Some(Target { rect: intersection(t.rect, still.area)?, ..t })).collect();
    // Start where the pointer is, with the window under it already lit, rather than a
    // dark screen and a magnifier in the corner until the mouse moves.
    let cursor = test_pointer().or_else(pointer).unwrap_or((i32::MIN, i32::MIN));
    let session = Rc::new(RefCell::new(Session {
        still,
        surface,
        mode,
        extras: Extras::default(),
        dragging: false,
        origin: None,
        current: None,
        hinted: None,
        cursor,
        targets,
        canvases: Vec::new(),
        windows: Vec::new(),
        bar: None,
        on_pick: Some(on_pick),
    }));
    {
        let mut s = session.borrow_mut();
        s.hinted = s.target_at(cursor);
    }
    SESSION.with(|s| *s.borrow_mut() = Some(session.clone()));
    // The bar goes on the monitor with the pointer.
    let bar_output = outputs.iter().position(|(_, r)| r.contains(cursor.0, cursor.1)).unwrap_or(0);

    for (i, (monitor, layout)) in outputs.into_iter().enumerate() {
        let window = gtk::Window::new();
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("nyx-capture"));
        // A test copy's selector never takes the keyboard from the real session.
        window.set_keyboard_mode(if test_monitor().is_some() { KeyboardMode::None } else { KeyboardMode::Exclusive });
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Bottom, true);
        window.set_anchor(Edge::Left, true);
        window.set_anchor(Edge::Right, true);
        window.set_exclusive_zone(-1);
        window.set_monitor(Some(&monitor));
        let g = monitor.geometry();
        window.set_default_size(g.width(), g.height());
        window.set_size_request(g.width(), g.height());
        window.add_css_class("capture-window");
        window.add_css_class("capture-overlay");
        window.set_decorated(false);
        window.set_resizable(false);
        window.set_deletable(false);
        if let Some(app) = gtk::gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) {
            window.set_application(Some(&app));
        }

        let canvas = gtk::DrawingArea::new();
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        canvas.set_cursor_from_name(Some("crosshair"));
        canvas.set_draw_func({
            let session = session.clone();
            move |_, cr, w, h| {
                if let Ok(s) = session.try_borrow() {
                    draw(&s, cr, w as f64, h as f64, layout);
                }
            }
        });
        wire_pointer(&canvas, session.clone(), layout);
        wire_keys(&window, session.clone());
        if with_bar && i == bar_output {
            let stack = gtk::Overlay::new();
            stack.set_child(Some(&canvas));
            let (bar, parts) = build_bar(&session);
            stack.add_overlay(&bar);
            session.borrow_mut().bar = Some(parts);
            window.set_child(Some(&stack));
        } else {
            window.set_child(Some(&canvas));
        }
        // Proceed so close() actually unmaps. Stop left Exclusive overlay layers
        // mapped on every workspace after a pick.
        window.connect_close_request(|_| {
            if !DISMISSING.get() {
                finish(None);
            }
            glib::Propagation::Proceed
        });
        session.borrow_mut().canvases.push(canvas);
        session.borrow_mut().windows.push(window.clone());
        window.present();
    }
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("CAPTURE_TEST_CANCEL_MS").ok().and_then(|v| v.parse().ok()) {
        glib::timeout_add_local_once(std::time::Duration::from_millis(ms), || finish(None));
    }
    // Debug builds: pick the first output whole after a delay, as a drag across it would.
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("CAPTURE_TEST_PICK_MS").ok().and_then(|v| v.parse().ok())
        && let Some((_, layout)) = self::outputs()?.into_iter().next()
    {
        glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
            // `CAPTURE_TEST_REGION=x,y,w,h` (from the test monitor's corner) picks that area.
            let region = std::env::var("CAPTURE_TEST_REGION").ok().and_then(|v| {
                let n: Vec<i32> = v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                (n.len() == 4).then(|| Rect::new(layout.x + n[0], layout.y + n[1], n[2], n[3]))
            });
            if let Some(r) = region {
                pick_now(r, (String::new(), String::new()));
                return;
            }
            let lit = SESSION.with(|s| s.borrow().as_ref().and_then(|s| s.borrow().hinted.clone()));
            match lit {
                Some(t) => pick_now(t.rect, (t.class, t.title)),
                None => pick_now(layout, (String::new(), String::new())),
            }
        });
    }
    Ok(())
}

/// Debug builds: `CAPTURE_TEST_MONITOR=<connector>` keeps the selector on that output
/// (a virtual monitor for layout checks), without the keyboard.
fn test_monitor() -> Option<String> {
    if cfg!(debug_assertions) { std::env::var("CAPTURE_TEST_MONITOR").ok() } else { None }
}

/// Debug builds: `CAPTURE_TEST_POINTER=x,y` (from the test monitor's corner) stands in
/// for the pointer, which stays on the real screen.
fn test_pointer() -> Option<(i32, i32)> {
    let v = if cfg!(debug_assertions) { std::env::var("CAPTURE_TEST_POINTER").ok()? } else { return None };
    let (x, y) = v.split_once(',')?;
    let mon = select::monitor_named(&test_monitor()?).ok()??.geometry;
    Some((mon.x + x.trim().parse::<i32>().ok()?, mon.y + y.trim().parse::<i32>().ok()?))
}

/// The capture bar: Smart / Window / Screen, three one-shot toggles, and the hint.
fn build_bar(session: &Rc<RefCell<Session>>) -> (gtk::Box, Bar) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("capture-bar");
    bar.set_halign(gtk::Align::Center);
    bar.set_valign(gtk::Align::End);
    bar.set_margin_bottom(22);

    let seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    seg.add_css_class("segmented");
    let mode = session.borrow().mode;
    let mut modes = Vec::new();
    let mut first: Option<gtk::ToggleButton> = None;
    for (m, icon, label, tip) in [
        (Mode::Smart, "capture-mode-smart-symbolic", "Smart", "Smart: drag a region, click a window, or click empty space (1)"),
        (Mode::Window, "capture-mode-window-symbolic", "Window", "Window: click one (2)"),
        (Mode::Screen, "capture-mode-screen-symbolic", "Screen", "Screen: the whole monitor (3)"),
        (Mode::Scroll, "capture-mode-scroll-symbolic", "Scroll", "Scroll: an area you scroll through, stitched into one tall shot (4)"),
        (Mode::Record, "capture-mode-record-symbolic", "Record", "Record: an area, a window or the screen (5)"),
    ] {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&gtk::Image::from_icon_name(icon));
        content.append(&gtk::Label::new(Some(label)));
        let b = gtk::ToggleButton::new();
        b.set_child(Some(&content));
        b.add_css_class("segment");
        b.set_tooltip_text(Some(tip));
        b.set_focus_on_click(false);
        b.set_can_focus(false);
        if let Some(f) = &first {
            b.set_group(Some(f));
        } else {
            first = Some(b.clone());
        }
        b.set_active(m == mode);
        let s = session.clone();
        b.connect_toggled(move |b| {
            if b.is_active() && s.borrow().mode != m {
                s.borrow_mut().set_mode(m);
                redraw();
            }
        });
        seg.append(&b);
        modes.push((m, b));
    }
    bar.append(&seg);
    bar.append(&separator());

    let toggle = |icon: &str, tip: &str, set: fn(&mut Extras, bool)| {
        let b = gtk::ToggleButton::new();
        b.set_icon_name(icon);
        b.add_css_class("flat");
        b.add_css_class("icon-button");
        b.set_tooltip_text(Some(tip));
        b.set_focus_on_click(false);
        b.set_can_focus(false);
        let s = session.clone();
        b.connect_toggled(move |b| set(&mut s.borrow_mut().extras, b.is_active()));
        bar.append(&b);
        b
    };
    let edit = toggle("capture-edit-symbolic", "Edit afterwards (E)", |x, on| x.edit = on);
    let text = toggle("capture-text-symbolic", "Copy its text instead (O)", |x, on| x.text = on);
    let delay = toggle("capture-timer-symbolic", "Wait 3 seconds, then take it (D)", |x, on| x.delay = on);
    let record = record_options();
    bar.append(&record);
    bar.append(&separator());

    let hint = gtk::Label::new(Some(mode.hint()));
    hint.add_css_class("capture-bar-hint");
    bar.append(&hint);
    let esc = gtk::Label::new(Some("Esc"));
    esc.add_css_class("capture-kbd");
    esc.set_margin_end(4);
    bar.append(&esc);
    let parts = Bar { modes, edit, text, delay, record, hint };
    parts.show_for(mode);
    (bar, parts)
}

/// Record mode's choices: system sound, mic, pointer, webcam, countdown and format.
/// They are the Recording settings, so they're remembered.
fn record_options() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let p = prefs::get();
    let audio = p.record_audio.clone();
    let (desktop, mic) = (matches!(audio.as_str(), "desktop" | "both"), matches!(audio.as_str(), "mic" | "both"));
    let option = |icon: &str, tip: &str, on: bool, set: fn(bool)| {
        let b = gtk::ToggleButton::new();
        b.set_icon_name(icon);
        b.add_css_class("flat");
        b.add_css_class("icon-button");
        b.set_tooltip_text(Some(tip));
        b.set_focus_on_click(false);
        b.set_can_focus(false);
        b.set_active(on);
        b.connect_toggled(move |b| set(b.is_active()));
        row.append(&b);
    };
    option("capture-speaker-symbolic", "System sound", desktop, |on| set_audio(Some(on), None));
    option("capture-mic-symbolic", "Microphone", mic, |on| set_audio(None, Some(on)));
    option("capture-pointer-symbolic", "Show the pointer", p.record_cursor, |on| prefs::update(|p| p.record_cursor = on));
    if nyx_capture_core::webcam::available() {
        option("capture-cam-symbolic", "Webcam in the corner", p.record_webcam, |on| prefs::update(|p| p.record_webcam = on));
    }
    option("capture-timer-symbolic", "Count down 3 seconds first", p.record_delay > 0, |on| prefs::update(|p| p.record_delay = if on { 3 } else { 0 }));
    let formats = gtk::StringList::new(&["MP4", "WebM", "GIF"]);
    let format = gtk::DropDown::new(Some(formats), gtk::Expression::NONE);
    format.add_css_class("capture-format");
    format.set_hexpand(false);
    format.set_halign(gtk::Align::Start);
    format.set_tooltip_text(Some("Format"));
    format.set_focus_on_click(false);
    format.set_selected(match p.record_container.as_str() {
        "webm" => 1,
        "gif" => 2,
        _ => 0,
    });
    format.connect_selected_notify(|d| {
        let value = ["mp4", "webm", "gif"][d.selected().min(2) as usize];
        prefs::update(|p| p.record_container = value.to_string());
    });
    row.append(&format);
    row
}

/// System sound and mic as the Recording setting's one value.
fn set_audio(desktop: Option<bool>, mic: Option<bool>) {
    let now = prefs::get().record_audio;
    let desktop = desktop.unwrap_or(matches!(now.as_str(), "desktop" | "both"));
    let mic = mic.unwrap_or(matches!(now.as_str(), "mic" | "both"));
    let value = match (desktop, mic) {
        (true, true) => "both",
        (true, false) => "desktop",
        (false, true) => "mic",
        (false, false) => "none",
    };
    prefs::update(|p| p.record_audio = value.to_string());
}

fn separator() -> gtk::Separator {
    let s = gtk::Separator::new(gtk::Orientation::Vertical);
    s.add_css_class("capture-bar-sep");
    s.set_valign(gtk::Align::Center);
    s
}

/// The pointer in layout coordinates (Hyprland's), if Hyprland can say.
fn pointer() -> Option<(i32, i32)> {
    let out = std::process::Command::new("hyprctl").args(["cursorpos", "-j"]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    Some((v["x"].as_i64()? as i32, v["y"].as_i64()? as i32))
}

/// Cold CLI: freeze, pick, crop, then copy/save/OCR. Caller has already delayed.
pub fn run_cli(
    action: &nyx_capture_core::action::Action,
) -> anyhow::Result<(nyx_capture_core::action::Outcome, Option<Shot>)> {
    ensure_gtk()?;
    if !supported() {
        anyhow::bail!("layer-shell isn't available");
    }
    nyx_capture_core::action::require_tools_for(&CaptureMode::FullDesktop, action.after, action.ocr_mode)?;
    let still = freeze()?;
    let snap = matches!(action.mode, CaptureMode::SelectWindow);
    let picked = run_blocking(still.clone(), snap);
    match picked {
        None => Ok((nyx_capture_core::action::Outcome::Cancelled, None)),
        Some(rect) => {
            let shot = still.crop_layout(rect)?;
            Ok((nyx_capture_core::action::finish(&shot, action)?, Some(shot)))
        }
    }
}

fn run_blocking(still: Shot, snap: bool) -> Option<Rect> {
    let result = Rc::new(RefCell::new(None::<Option<Rect>>));
    let main = glib::MainLoop::new(None, false);
    let result2 = result.clone();
    let main2 = main.clone();
    if pick(still, snap, move |rect| {
        *result2.borrow_mut() = Some(rect);
        main2.quit();
    })
    .is_err()
    {
        return None;
    }
    main.run();
    result.borrow_mut().take().flatten()
}

/// The monitor with the focused window (Hyprland's focused monitor), else the first.
fn focused_output() -> anyhow::Result<gdk::Monitor> {
    let outputs = outputs()?;
    let focused = select::focused_monitor().ok().flatten().map(|m| m.geometry);
    outputs
        .iter()
        .find(|(_, r)| Some(*r) == focused)
        .or(outputs.first())
        .map(|(m, _)| m.clone())
        .ok_or_else(|| anyhow::anyhow!("no monitors"))
}

/// The focused monitor and its layout rect.
pub fn focused() -> anyhow::Result<(gdk::Monitor, Rect)> {
    let focused = focused_output()?;
    outputs()?.into_iter().find(|(m, _)| *m == focused).ok_or_else(|| anyhow::anyhow!("no monitors"))
}

/// The monitor (and its layout rect) holding the middle of `area`, else the focused one.
pub fn output_for(area: Rect) -> anyhow::Result<(gdk::Monitor, Rect)> {
    let outputs = outputs()?;
    let (cx, cy) = (area.x + area.w / 2, area.y + area.h / 2);
    if let Some(found) = outputs.iter().find(|(_, r)| r.contains(cx, cy)) {
        return Ok(found.clone());
    }
    let focused = focused_output()?;
    outputs.into_iter().find(|(m, _)| *m == focused).ok_or_else(|| anyhow::anyhow!("no monitors"))
}

fn outputs() -> anyhow::Result<Vec<(gdk::Monitor, Rect)>> {
    let display = gdk::Display::default().ok_or_else(|| anyhow::anyhow!("no display"))?;
    let list = display.monitors();
    let hypr = select::monitors().unwrap_or_default();
    let mut out = Vec::new();
    for i in 0..list.n_items() {
        let Some(mon) = list.item(i).and_downcast::<gdk::Monitor>() else { continue };
        if let Some(only) = test_monitor()
            && mon.connector().is_none_or(|c| c != only)
        {
            continue;
        }
        let layout = match_hypr(&mon, &hypr).unwrap_or_else(|| gdk_rect(&mon));
        out.push((mon, layout));
    }
    Ok(out)
}

fn gdk_rect(mon: &gdk::Monitor) -> Rect {
    let g = mon.geometry();
    Rect::new(g.x(), g.y(), g.width(), g.height())
}

fn match_hypr(mon: &gdk::Monitor, hypr: &[select::Monitor]) -> Option<Rect> {
    if let Some(name) = mon.connector()
        && let Some(m) = hypr.iter().find(|m| m.name == name.as_str())
    {
        return Some(m.geometry);
    }
    let g = mon.geometry();
    hypr.iter().find(|m| m.geometry.x == g.x() && m.geometry.y == g.y()).map(|m| m.geometry)
}

fn wire_pointer(canvas: &gtk::DrawingArea, session: Rc<RefCell<Session>>, layout: Rect) {
    let motion = gtk::EventControllerMotion::new();
    let s = session.clone();
    let c = canvas.clone();
    motion.connect_motion(move |_, x, y| {
        let pt = layout_point(layout, c.width() as f64, c.height() as f64, x, y);
        let mut sess = s.borrow_mut();
        sess.cursor = pt;
        sess.hinted = sess.target_at(pt);
        if sess.dragging
            && let Some(origin) = sess.origin
        {
            sess.current = Some(drag_rect(origin, pt));
        }
        drop(sess);
        redraw();
    });
    canvas.add_controller(motion);

    let drag = gtk::GestureDrag::new();
    drag.set_button(gdk::BUTTON_PRIMARY);
    let s = session.clone();
    let c = canvas.clone();
    drag.connect_drag_begin(move |_, x, y| {
        let pt = layout_point(layout, c.width() as f64, c.height() as f64, x, y);
        let mut sess = s.borrow_mut();
        sess.cursor = pt;
        sess.hinted = sess.target_at(pt);
        // Window and Screen take what is lit on release; the others draw regions.
        if sess.mode.drags() {
            sess.dragging = true;
            sess.origin = Some(pt);
            sess.current = Some(drag_rect(pt, pt));
        }
        drop(sess);
        redraw();
    });
    let s = session.clone();
    let c = canvas.clone();
    drag.connect_drag_update(move |g, dx, dy| {
        let Some((x, y)) = g.start_point() else { return };
        let pt = layout_point(layout, c.width() as f64, c.height() as f64, x + dx, y + dy);
        let mut sess = s.borrow_mut();
        sess.cursor = pt;
        if sess.dragging
            && let Some(origin) = sess.origin
        {
            sess.current = Some(drag_rect(origin, pt));
        } else {
            sess.hinted = sess.target_at(pt);
        }
        drop(sess);
        redraw();
    });
    let s = session.clone();
    drag.connect_drag_end(move |_, _, _| {
        let picked = {
            let mut sess = s.borrow_mut();
            sess.dragging = false;
            resolve_release(&sess)
        };
        if let Some((rect, app)) = picked {
            pick_now(rect, app);
        } else {
            s.borrow_mut().current = None;
            redraw();
        }
    });
    canvas.add_controller(drag);
}

fn wire_keys(window: &gtk::Window, session: Rc<RefCell<Session>>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(move |_, key, _, mods| {
        let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
        let mode = |m: Mode| {
            let button = {
                let mut s = session.borrow_mut();
                s.set_mode(m);
                s.mode_button(m)
            };
            if let Some(b) = button {
                b.set_active(true);
            }
            redraw();
        };
        let flip = |which: fn(&Bar) -> &gtk::ToggleButton| {
            let b = session.borrow().bar.as_ref().map(|bar| which(bar).clone()).filter(|b| b.is_visible());
            if let Some(b) = b {
                b.set_active(!b.is_active());
            }
        };
        match key {
            gdk::Key::Escape => finish(None),
            gdk::Key::Return | gdk::Key::KP_Enter => {
                let picked = {
                    let sess = session.borrow();
                    match (sess.current, &sess.hinted) {
                        (Some(r), _) if !r.is_empty() => Some((r, sess.window_under(r))),
                        (_, Some(t)) if !t.rect.is_empty() => Some((t.rect, (t.class.clone(), t.title.clone()))),
                        _ => None,
                    }
                };
                if let Some((rect, app)) = picked {
                    pick_now(rect, app);
                }
            }
            gdk::Key::space => {
                let next = if session.borrow().mode == Mode::Window { Mode::Smart } else { Mode::Window };
                mode(next);
            }
            gdk::Key::_1 | gdk::Key::KP_1 => mode(Mode::Smart),
            gdk::Key::_2 | gdk::Key::KP_2 => mode(Mode::Window),
            gdk::Key::_3 | gdk::Key::KP_3 => mode(Mode::Screen),
            gdk::Key::_4 | gdk::Key::KP_4 => mode(Mode::Scroll),
            gdk::Key::_5 | gdk::Key::KP_5 => mode(Mode::Record),
            gdk::Key::e | gdk::Key::E => flip(|b| &b.edit),
            gdk::Key::o | gdk::Key::O => flip(|b| &b.text),
            gdk::Key::d | gdk::Key::D => flip(|b| &b.delay),
            gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down => {
                let (dx, dy) = match key {
                    gdk::Key::Left => (-1, 0),
                    gdk::Key::Right => (1, 0),
                    gdk::Key::Up => (0, -1),
                    gdk::Key::Down => (0, 1),
                    _ => (0, 0),
                };
                let mut sess = session.borrow_mut();
                if sess.current.is_none() {
                    sess.current = sess.hinted.as_ref().map(|t| t.rect);
                }
                if let Some(r) = sess.current {
                    let bounds = sess.still.area;
                    sess.current = Some(if shift { resize_rect(r, dx, dy, bounds) } else { nudge_rect(r, dx, dy, bounds) });
                }
                drop(sess);
                redraw();
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    window.add_controller(keys);
}

/// A drag in Smart mode is the region; a click (or any release in the other modes)
/// takes what is lit.
fn resolve_release(sess: &Session) -> Option<(Rect, (String, String))> {
    if let Some(current) = sess.current
        && !is_click(current)
        && !current.is_empty()
    {
        return Some((current, sess.window_under(current)));
    }
    let lit = sess.hinted.as_ref()?;
    if lit.rect.is_empty() {
        return None;
    }
    Some((lit.rect, (lit.class.clone(), lit.title.clone())))
}

/// The region overlay is on screen.
pub fn is_open() -> bool {
    SESSION.with(|s| s.borrow().is_some())
}

/// Close the overlay as Esc would.
pub fn dismiss() {
    finish(None);
}

fn pick_now(rect: Rect, (app, title): (String, String)) {
    let (extras, mode) = SESSION.with(|s| s.borrow().as_ref().map(|s| (s.borrow().extras, s.borrow().mode))).unwrap_or((Extras::default(), Mode::Smart));
    finish(Some(Picked { rect, app, title, extras, mode }));
}

fn finish(picked: Option<Picked>) {
    if DISMISSING.get() {
        return;
    }
    DISMISSING.set(true);
    let Some(sess) = SESSION.with(|s| s.borrow_mut().take()) else {
        DISMISSING.set(false);
        return;
    };
    let mut sess = sess.borrow_mut();
    let windows = std::mem::take(&mut sess.windows);
    let canvases = std::mem::take(&mut sess.canvases);
    let cb = sess.on_pick.take();
    sess.bar = None;
    drop(sess);
    drop(canvases);
    dismiss_windows(windows);
    glib::idle_add_local_once(move || {
        DISMISSING.set(false);
        if let Some(cb) = cb {
            cb(picked);
        }
    });
}

fn dismiss_windows(windows: Vec<gtk::Window>) {
    for w in windows {
        w.set_keyboard_mode(KeyboardMode::None);
        w.set_can_target(false);
        w.set_child(None::<&gtk::Widget>);
        w.set_application(None::<&gtk::Application>);
        w.set_visible(false);
        w.close();
    }
}

fn redraw() {
    SESSION.with(|s| {
        if let Some(sess) = s.borrow().as_ref() {
            for c in &sess.borrow().canvases {
                c.queue_draw();
            }
        }
    });
}

fn draw(sess: &Session, cr: &Context, ww: f64, wh: f64, layout: Rect) {
    if ww <= 0.0 || wh <= 0.0 || layout.w <= 0 || layout.h <= 0 {
        return;
    }
    paint_still(cr, &sess.surface, &sess.still, layout, ww, wh);

    if let Some(c) = token("veil") {
        set_rgba(cr, c, None);
        let _ = cr.paint();
    }

    let dragged = sess.current.filter(|r| !is_click(*r));
    let shown = dragged.or(sess.hinted.as_ref().map(|t| t.rect));
    if let Some(r) = shown
        && let Some(part) = intersection(r, layout)
    {
        let (x, y, w, h) = widget_rect(layout, ww, wh, part);
        cr.rectangle(x, y, w, h);
        cr.clip();
        paint_still(cr, &sess.surface, &sess.still, layout, ww, wh);
        cr.reset_clip();
        if let Some(stroke) = token("theme_accent") {
            set_rgba(cr, stroke, None);
            cr.set_line_width(1.5);
            cr.rectangle(x + 0.75, y + 0.75, (w - 1.5).max(0.0), (h - 1.5).max(0.0));
            let _ = cr.stroke();
        }
    }

    if layout.contains(sess.cursor.0, sess.cursor.1) {
        let (px, py) = widget_point(layout, ww, wh, sess.cursor);
        let app = match (dragged, &sess.hinted) {
            (None, Some(t)) if t.window => app_name(&t.class),
            (None, Some(_)) => "Screen".to_string(),
            _ => String::new(),
        };
        let size = shown.map(|r| ((r.w as f64 * sess.still.scale).round() as i64, (r.h as f64 * sess.still.scale).round() as i64));
        paint_readouts(sess, cr, layout, ww, wh, (px, py), size, &app);
    }
}

/// A class as a name for the chip: `org.mozilla.firefox` → `Firefox`.
fn app_name(class: &str) -> String {
    let last = class.rsplit('.').next().unwrap_or(class);
    let mut chars = last.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn paint_still(cr: &Context, surface: &ImageSurface, still: &Shot, mon: Rect, ww: f64, wh: f64) {
    let scale = still.scale.max(0.0001);
    let sx = ww / mon.w as f64;
    let sy = wh / mon.h as f64;
    cr.save().ok();
    cr.scale(sx / scale, sy / scale);
    let ox = (mon.x - still.area.x) as f64 * scale;
    let oy = (mon.y - still.area.y) as f64 * scale;
    if cr.set_source_surface(surface, -ox, -oy).is_ok() {
        cr.source().set_filter(cairo::Filter::Nearest);
        let _ = cr.paint();
    }
    cr.restore().ok();
}

/// Beside the pointer: a chip with the size (in saved pixels) and the app, then the
/// magnifier and the pointer's position under it. Flips left or up near an edge.
#[allow(clippy::too_many_arguments)]
fn paint_readouts(sess: &Session, cr: &Context, layout: Rect, ww: f64, wh: f64, at: (f64, f64), size: Option<(i64, i64)>, app: &str) {
    const GAP: f64 = 22.0;
    cr.select_font_face("Inter", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(12.0);
    let size_text = size.map(|(w, h)| format!("{w} × {h}")).unwrap_or_default();
    let ext = |cr: &Context, t: &str| cr.text_extents(t).map(|e| (e.x_advance(), e.height(), e.y_bearing())).unwrap_or((0.0, 0.0, 0.0));
    let (sw, _, _) = ext(cr, &size_text);
    cr.select_font_face("Inter", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    let (aw, _, _) = ext(cr, app);
    let (pad_x, chip_h) = (9.0, 24.0);
    let gap_between = if size_text.is_empty() || app.is_empty() { 0.0 } else { 8.0 };
    let chip_w = if size_text.is_empty() && app.is_empty() { 0.0 } else { pad_x * 2.0 + sw + gap_between + aw };
    let block_w = chip_w.max(MAG_SIZE);
    let block_h = chip_h + 8.0 + MAG_SIZE + 22.0;
    let mut x = at.0 + GAP;
    let mut y = at.1 + GAP;
    if x + block_w > ww - 8.0 {
        x = at.0 - GAP - block_w;
    }
    if y + block_h > wh - 8.0 {
        y = at.1 - GAP - block_h;
    }
    x = x.clamp(8.0, (ww - block_w - 8.0).max(8.0));
    y = y.clamp(8.0, (wh - block_h - 8.0).max(8.0));

    if chip_w > 0.0 {
        if let Some(bg) = token("surface") {
            set_rgba(cr, bg, None);
            rounded(cr, x, y, chip_w, chip_h, 7.0);
            let _ = cr.fill();
        }
        let baseline = y + chip_h / 2.0 + 4.0;
        if let Some(fg) = token("theme_text") {
            set_rgba(cr, fg, None);
            cr.select_font_face("Inter", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
            cr.move_to(x + pad_x, baseline);
            let _ = cr.show_text(&size_text);
        }
        if let Some(dim) = token("dim_text") {
            set_rgba(cr, dim, None);
            cr.select_font_face("Inter", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
            cr.move_to(x + pad_x + sw + gap_between, baseline);
            let _ = cr.show_text(app);
        }
    }

    let (mx, my) = (x, y + chip_h + 8.0);
    cr.save().ok();
    rounded(cr, mx, my, MAG_SIZE, MAG_SIZE, 11.0);
    cr.clip();
    if let Some(bg) = token("theme_bg") {
        set_rgba(cr, bg, None);
        let _ = cr.paint();
    }
    let scale = sess.still.scale.max(0.0001);
    let img_x = (sess.cursor.0 - sess.still.area.x) as f64 * scale;
    let img_y = (sess.cursor.1 - sess.still.area.y) as f64 * scale;
    cr.translate(mx + MAG_SIZE / 2.0, my + MAG_SIZE / 2.0);
    cr.scale(MAG_ZOOM, MAG_ZOOM);
    if cr.set_source_surface(&sess.surface, -img_x, -img_y).is_ok() {
        cr.source().set_filter(cairo::Filter::Nearest);
        let _ = cr.paint();
    }
    cr.restore().ok();
    if let Some(border) = token("theme_border") {
        set_rgba(cr, border, None);
        cr.set_line_width(1.0);
        rounded(cr, mx + 0.5, my + 0.5, MAG_SIZE - 1.0, MAG_SIZE - 1.0, 11.0);
        let _ = cr.stroke();
    }
    if let Some(accent) = token("theme_accent") {
        set_rgba(cr, accent, None);
        cr.set_line_width(1.5);
        let px = MAG_ZOOM;
        cr.rectangle(mx + MAG_SIZE / 2.0 - 0.75, my + MAG_SIZE / 2.0 - 0.75, px + 1.5, px + 1.5);
        let _ = cr.stroke();
    }
    if let Some(dim) = token("dim_text") {
        set_rgba(cr, dim, None);
        cr.select_font_face("Inter", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(11.5);
        let rel = (sess.cursor.0 - layout.x, sess.cursor.1 - layout.y);
        cr.move_to(mx + 2.0, my + MAG_SIZE + 16.0);
        let _ = cr.show_text(&format!("X {} · Y {}", rel.0, rel.1));
    }
}

/// A rounded rectangle path.
fn rounded(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    let pi = std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -pi / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, pi / 2.0);
    cr.arc(x + r, y + h - r, r, pi / 2.0, pi);
    cr.arc(x + r, y + r, r, pi, 3.0 * pi / 2.0);
    cr.close_path();
}

fn token(name: &str) -> Option<gdk::RGBA> {
    let p = crate::theme::palette();
    match name {
        "theme_accent" => parse_hex(&p.accent),
        "theme_bg" => parse_hex(&p.bg),
        "theme_text" => parse_hex(&p.text),
        "theme_border" => parse_hex(&p.border),
        "surface" => parse_hex(&p.surface),
        "dim_text" => parse_hex(&p.dim_text),
        "veil" => {
            let mut c = mix(parse_hex(&p.bg)?, parse_hex(&p.shadow)?, 0.5);
            c.set_alpha(0.6);
            Some(c)
        }
        _ => None,
    }
}

fn parse_hex(hex: &str) -> Option<gdk::RGBA> {
    gdk::RGBA::parse(hex).ok()
}

fn mix(a: gdk::RGBA, b: gdk::RGBA, t: f32) -> gdk::RGBA {
    gdk::RGBA::new(
        a.red() + (b.red() - a.red()) * t,
        a.green() + (b.green() - a.green()) * t,
        a.blue() + (b.blue() - a.blue()) * t,
        a.alpha() + (b.alpha() - a.alpha()) * t,
    )
}

fn set_rgba(cr: &Context, c: gdk::RGBA, alpha: Option<f64>) {
    cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, alpha.unwrap_or(c.alpha() as f64));
}

fn still_surface(still: &Shot) -> Option<ImageSurface> {
    let rgba = still.image.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut surface = ImageSurface::create(Format::ARgb32, w as i32, h as i32).ok()?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().ok()?;
        for (y, row) in rgba.rows().enumerate() {
            for (x, p) in row.enumerate() {
                data[y * stride + x * 4..][..4].copy_from_slice(&premultiply(p.0).to_ne_bytes());
            }
        }
    }
    Some(surface)
}

fn premultiply(rgba: [u8; 4]) -> u32 {
    let a = rgba[3] as u32;
    let r = (rgba[0] as u32 * a + 127) / 255;
    let g = (rgba[1] as u32 * a + 127) / 255;
    let b = (rgba[2] as u32 * a + 127) / 255;
    (a << 24) | (r << 16) | (g << 8) | b
}

fn layout_point(mon: Rect, ww: f64, wh: f64, x: f64, y: f64) -> (i32, i32) {
    if ww <= 0.0 || wh <= 0.0 {
        return (mon.x, mon.y);
    }
    let lx = mon.x as f64 + x * mon.w as f64 / ww;
    let ly = mon.y as f64 + y * mon.h as f64 / wh;
    (lx.round() as i32, ly.round() as i32)
}

fn widget_point(mon: Rect, ww: f64, wh: f64, pt: (i32, i32)) -> (f64, f64) {
    let sx = ww / mon.w as f64;
    let sy = wh / mon.h as f64;
    ((pt.0 - mon.x) as f64 * sx, (pt.1 - mon.y) as f64 * sy)
}

fn widget_rect(mon: Rect, ww: f64, wh: f64, r: Rect) -> (f64, f64, f64, f64) {
    let sx = ww / mon.w as f64;
    let sy = wh / mon.h as f64;
    ((r.x - mon.x) as f64 * sx, (r.y - mon.y) as f64 * sy, r.w as f64 * sx, r.h as f64 * sy)
}

pub fn drag_rect(a: (i32, i32), b: (i32, i32)) -> Rect {
    let x = a.0.min(b.0);
    let y = a.1.min(b.1);
    Rect::new(x, y, (a.0 - b.0).abs().max(1), (a.1 - b.1).abs().max(1))
}

pub fn is_click(rect: Rect) -> bool {
    rect.area() < CLICK_AREA
}

fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w).min(b.x + b.w);
    let y1 = (a.y + a.h).min(b.y + b.h);
    if x1 > x0 && y1 > y0 { Some(Rect::new(x0, y0, x1 - x0, y1 - y0)) } else { None }
}

fn nudge_rect(r: Rect, dx: i32, dy: i32, bounds: Rect) -> Rect {
    clamp_rect(Rect::new(r.x + dx, r.y + dy, r.w, r.h), bounds)
}

fn resize_rect(r: Rect, dw: i32, dh: i32, bounds: Rect) -> Rect {
    clamp_rect(Rect::new(r.x, r.y, r.w + dw, r.h + dh), bounds)
}

fn clamp_rect(r: Rect, bounds: Rect) -> Rect {
    let w = r.w.clamp(1, bounds.w.max(1));
    let h = r.h.clamp(1, bounds.h.max(1));
    let x = r.x.clamp(bounds.x, bounds.x + bounds.w - w);
    let y = r.y.clamp(bounds.y, bounds.y + bounds.h - h);
    Rect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_normalizes_corners() {
        assert_eq!(drag_rect((10, 20), (40, 50)), Rect::new(10, 20, 30, 30));
        assert_eq!(drag_rect((40, 50), (10, 20)), Rect::new(10, 20, 30, 30));
    }

    #[test]
    fn tiny_drag_is_a_click() {
        assert!(is_click(Rect::new(0, 0, 3, 4)));
        assert!(!is_click(Rect::new(0, 0, 20, 20)));
    }

    #[test]
    fn arrows_nudge_and_resize() {
        let bounds = Rect::new(0, 0, 100, 80);
        let r = Rect::new(10, 10, 20, 15);
        assert_eq!(nudge_rect(r, 1, -1, bounds), Rect::new(11, 9, 20, 15));
        assert_eq!(resize_rect(r, 2, -3, bounds), Rect::new(10, 10, 22, 12));
        assert_eq!(nudge_rect(Rect::new(0, 0, 10, 10), -5, -5, bounds), Rect::new(0, 0, 10, 10));
    }

    #[test]
    fn layout_maps_widget_origin() {
        let mon = Rect::new(100, 50, 200, 100);
        assert_eq!(layout_point(mon, 200.0, 100.0, 0.0, 0.0), (100, 50));
        assert_eq!(layout_point(mon, 200.0, 100.0, 200.0, 100.0), (300, 150));
    }
}
