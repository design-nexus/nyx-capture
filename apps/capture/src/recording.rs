//! Recording from the capture bar, and the recording bar: the time, sound and mic, pause,
//! discard and stop. It sits just outside the recorded area so it isn't in the video;
//! recording the whole screen, it shrinks after a moment to a small pill in the top right
//! corner and opens again when pointed at. Stopping ends with the recording's card.

use crate::overlay::{self, Picked};
use crate::window;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use nyx_capture_core::geom::Rect;
use nyx_capture_core::record::{self, Target};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

const GAP: i32 = 14;
const TUCK_AFTER: Duration = Duration::from_secs(2);

struct Bar {
    window: gtk::Window,
    /// The red frame around a recorded area (four thin strips just outside it).
    frame: Vec<gtk::Window>,
    full: gtk::Box,
    mini: gtk::Box,
    times: [gtk::Label; 2],
    dot: gtk::Box,
    pause: Option<gtk::Button>,
    /// Recording the whole screen: tuck into the corner when not pointed at.
    tucks: bool,
    tuck: RefCell<Option<glib::SourceId>>,
    seconds: Cell<u64>,
}

struct State {
    bar: Option<Rc<Bar>>,
    restore: Option<Rc<dyn Fn()>>,
    hold: Option<gio::ApplicationHoldGuard>,
    /// The mic was muted from the bar: unmute it when the recording ends.
    muted_mic: bool,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State { bar: None, restore: None, hold: None, muted_mic: false }) };
}

/// Record what was picked in Record mode: an area, a window, or the whole screen
/// (`screen` is the monitor it's on). The history window stays away until it's stopped.
pub fn start(picked: &Picked, screen: Rect, restore: Rc<dyn Fn()>, hold: gio::ApplicationHoldGuard) {
    if record::active().is_some() {
        restore();
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.restore = Some(restore);
        s.hold = Some(hold);
    });
    let p = crate::prefs::get();
    let whole = picked.rect == screen;
    let target = if record::pick(&p.recorder) == Some(nyx_capture_core::RecordBackend::GstPortal) || (whole && p.record_use_portal) {
        Target::Portal
    } else if whole {
        match overlay::output_for(screen).ok().and_then(|(m, _)| m.connector()) {
            Some(name) => Target::Monitor(name.to_string()),
            None => Target::Region(picked.rect),
        }
    } else {
        Target::Region(picked.rect)
    };
    let delay = p.record_delay;
    if delay == 0 {
        launch(target);
        return;
    }
    let started = overlay::countdown(delay, move |go| {
        if go {
            launch(target);
        } else {
            ended();
        }
    });
    if let Err(err) = started {
        crate::naming::log_error("record", &err);
        ended();
    }
}

fn launch(target: Target) {
    let area = match &target {
        Target::Region(r) => Some(*r),
        _ => None,
    };
    let req = window::record_request(target, window::record_output_path());
    let p = crate::prefs::get();
    let (cam, cam_device, cam_size) = (p.record_webcam, p.record_webcam_device, p.record_webcam_size);
    crate::cmd::background(
        move || window::start_record_session(&req, cam, &cam_device, &cam_size),
        move |result| match result {
            Ok(_) => {
                window::refresh_record_button();
                window::refresh_cam_button();
                show_bar(area);
            }
            Err(err) => {
                crate::naming::log_error("record", &err);
                ended();
            }
        },
    );
}

/// The recording bar, for a recording of `area` (None: the whole screen).
pub fn show_bar(area: Option<Rect>) {
    hide_bar();
    let placed = match area {
        Some(r) => overlay::output_for(r),
        None => overlay::focused(),
    };
    let Ok((monitor, layout)) = placed else { return };
    let session = record::active();
    let p = crate::prefs::get();
    let (desktop, mic) = match p.record_audio.as_str() {
        "both" => (true, true),
        "mic" => (false, true),
        "none" => (false, false),
        _ => (true, false),
    };

    let time = gtk::Label::new(Some("0:00"));
    time.add_css_class("rec-time");
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("rec-dot");
    dot.set_valign(gtk::Align::Center);

    let sound = state_button("capture-speaker-symbolic", desktop, if desktop { "Recording system audio" } else { "System audio off" });
    sound.set_sensitive(false);
    let mic_btn = state_button("capture-mic-symbolic", mic, if mic { "Mute microphone" } else { "Microphone off" });
    mic_btn.set_sensitive(mic);
    mic_btn.connect_toggled(|b| set_mic(b.is_active()));

    let pause = session.as_ref().filter(|_| record::can_pause()).map(|_| icon("capture-pause-symbolic", "Pause recording (Ctrl+Alt+P)"));
    let discard = icon("capture-trash-symbolic", "Discard recording");
    let stop_btn = icon("capture-stop-symbolic", "Stop recording (Print)");
    stop_btn.add_css_class("rec-stop");

    let full = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    full.append(&dot);
    full.append(&time);
    full.append(&sep());
    full.append(&sound);
    full.append(&mic_btn);
    full.append(&sep());
    if let Some(p) = &pause {
        full.append(p);
    }
    full.append(&discard);
    full.append(&stop_btn);

    let mini_time = gtk::Label::new(Some("0:00"));
    mini_time.add_css_class("rec-time");
    let mini_dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    mini_dot.add_css_class("rec-dot");
    mini_dot.set_valign(gtk::Align::Center);
    let mini_stop = gtk::Image::from_icon_name("capture-stop-symbolic");
    mini_stop.add_css_class("rec-mini-stop");
    let mini = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    mini.append(&mini_dot);
    mini.append(&mini_time);
    mini.append(&mini_stop);
    mini.set_visible(false);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.add_css_class("capture-bar");
    body.add_css_class("rec-bar");
    body.append(&full);
    body.append(&mini);

    let window = gtk::Window::new();
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("nyx-capture-recording"));
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_exclusive_zone(-1);
    window.set_monitor(Some(&monitor));
    window.add_css_class("capture-window");
    window.add_css_class("capture-overlay");
    window.set_decorated(false);
    window.set_resizable(false);
    if let Some(app) = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) {
        window.set_application(Some(&app));
    }
    window.set_child(Some(&body));

    // Just outside the area: below it, else above it, else inside its foot.
    let (_, w, _, _) = body.measure(gtk::Orientation::Horizontal, -1);
    let (_, h, _, _) = body.measure(gtk::Orientation::Vertical, w);
    let tucks = area.is_none();
    match area {
        Some(r) => {
            let x = (r.x + r.w / 2 - w / 2).clamp(layout.x + 8, (layout.x + layout.w - w - 8).max(layout.x + 8));
            let y = if r.y + r.h + GAP + h <= layout.y + layout.h - 8 {
                r.y + r.h + GAP
            } else if r.y - GAP - h >= layout.y + 8 {
                r.y - GAP - h
            } else {
                r.y + r.h - h - GAP
            };
            window.set_anchor(Edge::Top, true);
            window.set_anchor(Edge::Left, true);
            window.set_margin(Edge::Left, x - layout.x);
            window.set_margin(Edge::Top, y - layout.y);
        }
        None => place_bottom(&window, layout, w),
    }

    let frame = area.map(|r| frame_strips(&monitor, layout, r)).unwrap_or_default();
    for f in &frame {
        f.present();
    }
    let bar = Rc::new(Bar {
        window: window.clone(),
        frame,
        full,
        mini,
        times: [time, mini_time],
        dot,
        pause: pause.clone(),
        tucks,
        tuck: RefCell::new(None),
        seconds: Cell::new(0),
    });
    let _ = mini_dot;

    let weak = Rc::downgrade(&bar);
    stop_btn.connect_clicked(move |_| {
        if weak.upgrade().is_some() {
            stop(false);
        }
    });
    discard.connect_clicked(|_| stop(true));
    if let Some(p) = &pause {
        p.connect_clicked(|_| window::pause_recording());
    }

    if tucks {
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(&bar);
        motion.connect_enter(move |_, _, _| {
            if let Some(bar) = weak.upgrade() {
                set_tucked(&bar, false);
            }
        });
        let weak = Rc::downgrade(&bar);
        motion.connect_leave(move |_| {
            if let Some(bar) = weak.upgrade() {
                schedule_tuck(&bar);
            }
        });
        window.add_controller(motion);
        // Clicking the pill's stop square stops.
        let click = gtk::GestureClick::new();
        click.connect_released(|g, _, x, _| {
            if let Some(w) = g.widget()
                && x > w.width() as f64 - 34.0
                && STATE.with(|s| s.borrow().bar.as_ref().is_some_and(|b| b.mini.is_visible()))
            {
                stop(false);
            }
        });
        window.add_controller(click);
        schedule_tuck(&bar);
    }

    // The clock: seconds recorded, paused time not counted.
    let weak = Rc::downgrade(&bar);
    glib::timeout_add_seconds_local(1, move || {
        let Some(bar) = weak.upgrade() else { return glib::ControlFlow::Break };
        let Some(session) = record::active() else {
            // The recorder ended by itself: keep what it wrote.
            stop(false);
            return glib::ControlFlow::Break;
        };
        if !session.paused {
            bar.seconds.set(bar.seconds.get() + 1);
        }
        let text = clock(bar.seconds.get());
        for t in &bar.times {
            t.set_text(&text);
        }
        if session.paused {
            bar.dot.add_css_class("paused");
        } else {
            bar.dot.remove_css_class("paused");
        }
        if let Some(p) = &bar.pause {
            p.set_icon_name(if session.paused { "capture-play-symbolic" } else { "capture-pause-symbolic" });
            p.set_tooltip_text(Some(if session.paused { "Resume recording (Ctrl+Alt+P)" } else { "Pause recording (Ctrl+Alt+P)" }));
        }
        glib::ControlFlow::Continue
    });

    window.present();
    STATE.with(|s| s.borrow_mut().bar = Some(bar));
}

fn place_bottom(window: &gtk::Window, layout: Rect, w: i32) {
    window.set_anchor(Edge::Top, false);
    window.set_anchor(Edge::Right, false);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Left, true);
    window.set_margin(Edge::Bottom, 22);
    window.set_margin(Edge::Left, (layout.w - w) / 2);
}

fn schedule_tuck(bar: &Rc<Bar>) {
    if let Some(id) = bar.tuck.take() {
        id.remove();
    }
    let weak = Rc::downgrade(bar);
    let id = glib::timeout_add_local_once(TUCK_AFTER, move || {
        if let Some(bar) = weak.upgrade() {
            bar.tuck.replace(None);
            set_tucked(&bar, true);
        }
    });
    bar.tuck.replace(Some(id));
}

/// The pill in the top right corner, or the full bar (there too, once tucked).
fn set_tucked(bar: &Bar, tucked: bool) {
    if let Some(id) = bar.tuck.take() {
        id.remove();
    }
    if !bar.tucks {
        return;
    }
    bar.full.set_visible(!tucked);
    bar.mini.set_visible(tucked);
    let w = &bar.window;
    w.set_anchor(Edge::Bottom, false);
    w.set_anchor(Edge::Left, false);
    w.set_anchor(Edge::Top, true);
    w.set_anchor(Edge::Right, true);
    w.set_margin(Edge::Top, 36);
    w.set_margin(Edge::Right, 16);
    if let Some(c) = bar.window.child() {
        if tucked {
            c.add_css_class("tucked");
        } else {
            c.remove_css_class("tucked");
        }
    }
}

/// Stop the recording (and throw it away with `discard`), then show its card.
pub fn stop(discard: bool) {
    hide_bar();
    crate::cmd::background(record::stop, move |result| {
        window::refresh_record_button();
        window::refresh_cam_button();
        match result {
            Ok(Some(path)) if discard => {
                let _ = std::fs::remove_file(&path);
            }
            Ok(Some(path)) => {
                window::remember_recording(&path);
                crate::card::show_video(path);
            }
            Ok(None) => {}
            Err(err) => crate::naming::log_error("record", &err),
        }
        ended();
    });
}

/// The recording is over (or never started): mic back on, history window back.
fn ended() {
    let (restore, hold, unmute) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        (s.restore.take(), s.hold.take(), std::mem::take(&mut s.muted_mic))
    });
    if unmute {
        set_mic(true);
    }
    if let Some(restore) = restore {
        restore();
    }
    drop(hold);
}

fn hide_bar() {
    if let Some(bar) = STATE.with(|s| s.borrow_mut().bar.take()) {
        if let Some(id) = bar.tuck.take() {
            id.remove();
        }
        bar.window.close();
        for f in &bar.frame {
            f.close();
        }
    }
}

/// A 2 px red line around `r`, just outside it so it isn't recorded, letting the
/// pointer through.
fn frame_strips(monitor: &gtk::gdk::Monitor, layout: Rect, r: Rect) -> Vec<gtk::Window> {
    const W: i32 = 2;
    let sides = [
        Rect::new(r.x - W, r.y - W, r.w + 2 * W, W),
        Rect::new(r.x - W, r.y + r.h, r.w + 2 * W, W),
        Rect::new(r.x - W, r.y, W, r.h),
        Rect::new(r.x + r.w, r.y, W, r.h),
    ];
    sides
        .into_iter()
        .filter(|s| s.x >= layout.x && s.y >= layout.y && s.x + s.w <= layout.x + layout.w && s.y + s.h <= layout.y + layout.h)
        .map(|s| {
            let win = gtk::Window::new();
            win.init_layer_shell();
            win.set_layer(Layer::Overlay);
            win.set_namespace(Some("nyx-capture-recording"));
            win.set_keyboard_mode(KeyboardMode::None);
            win.set_anchor(Edge::Top, true);
            win.set_anchor(Edge::Left, true);
            win.set_exclusive_zone(-1);
            win.set_margin(Edge::Left, s.x - layout.x);
            win.set_margin(Edge::Top, s.y - layout.y);
            win.set_monitor(Some(monitor));
            win.set_default_size(s.w, s.h);
            win.set_size_request(s.w, s.h);
            win.set_decorated(false);
            win.set_can_target(false);
            win.add_css_class("capture-window");
            win.add_css_class("rec-frame");
            if let Some(app) = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) {
                win.set_application(Some(&app));
            }
            win.connect_realize(|w| {
                if let Some(surface) = w.surface() {
                    surface.set_input_region(Some(&gtk::cairo::Region::create()));
                }
            });
            win
        })
        .collect()
}

/// The default microphone on or off (the recorder records whatever it hears).
fn set_mic(on: bool) {
    STATE.with(|s| s.borrow_mut().muted_mic = !on);
    crate::cmd::spawn(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SOURCE@", if on { "0" } else { "1" }]);
}

fn state_button(icon_name: &str, on: bool, tip: &str) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::new();
    b.set_icon_name(icon_name);
    b.add_css_class("flat");
    b.add_css_class("icon-button");
    b.set_tooltip_text(Some(tip));
    b.set_focus_on_click(false);
    b.set_can_focus(false);
    b.set_active(on);
    b
}

fn icon(icon_name: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon_name);
    b.add_css_class("flat");
    b.add_css_class("icon-button");
    b.set_tooltip_text(Some(tip));
    b.set_focus_on_click(false);
    b.set_can_focus(false);
    b
}

fn sep() -> gtk::Separator {
    let s = gtk::Separator::new(gtk::Orientation::Vertical);
    s.add_css_class("capture-bar-sep");
    s.set_valign(gtk::Align::Center);
    s
}

/// 67 → "1:07"; an hour or more → "1:02:03".
pub fn clock(n: u64) -> String {
    if n >= 3600 { format!("{}:{:02}:{:02}", n / 3600, n / 60 % 60, n % 60) } else { format!("{}:{:02}", n / 60, n % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_reads_like_a_timer() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(67), "1:07");
        assert_eq!(clock(3723), "1:02:03");
    }
}
