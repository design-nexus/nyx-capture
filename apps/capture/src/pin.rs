//! Floating pin windows. Hyprland pins them via the Capture.Pin window class.

use crate::widgets;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use image::RgbaImage;
use nyx_capture_core::capture::Shot;
use std::cell::RefCell;
use std::ffi::{CString, c_char, c_void};

/// Wayland app-id / Hyprland class. `install.sh` writes a float+pin rule for it.
pub const CLASS: &str = "io.github.design_nexus.nyx.Capture.Pin";
const TITLE: &str = "Capture pin";
const MIN_EDGE: i32 = 160;
const MAX_EDGE: i32 = 1600;

thread_local! {
    static PINS: RefCell<Vec<gtk::Window>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "C" {
    fn gdk_wayland_toplevel_set_application_id(toplevel: *mut c_void, application_id: *const c_char);
}

pub fn open_shot(shot: Shot) {
    open_rgba(shot.image.to_rgba8());
}

pub fn open_rgba(img: RgbaImage) {
    let (iw, ih) = img.dimensions();
    let (dw, dh) = fitted_size(iw, ih, 480, 320);
    let bytes = glib::Bytes::from(&img.into_raw());
    let texture = gdk::MemoryTexture::new(iw as i32, ih as i32, gdk::MemoryFormat::R8g8b8a8, &bytes, iw as usize * 4);

    // A GtkApplicationWindow inherits the Capture app-id, so Hyprland never matches the
    // Capture.Pin float+pin rule. A plain Window plus the Wayland app-id does.
    let window = gtk::Window::builder()
        .title(TITLE)
        .default_width(dw)
        .default_height(dh + 40)
        .build();
    window.add_css_class("capture-window");
    window.add_css_class("capture-pin");
    window.set_resizable(true);
    // Not an application window (see above), so hold the app ourselves: closing the
    // history window or the editor mustn't take the pins with it.
    let hold = std::cell::RefCell::new(gtk::gio::Application::default().map(|a| a.hold()));
    window.connect_close_request(move |_| {
        hold.borrow_mut().take();
        glib::Propagation::Proceed
    });

    let picture = gtk::Picture::for_paintable(&texture);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.set_content_fit(gtk::ContentFit::Contain);

    let opacity = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.2, 1.0, 0.05);
    opacity.set_value(1.0);
    opacity.set_draw_value(false);
    opacity.set_hexpand(true);
    opacity.set_tooltip_text(Some("Opacity"));
    let win_opacity = window.clone();
    opacity.connect_value_changed(move |s| {
        win_opacity.set_opacity(s.value());
    });

    let close = widgets::icon_button("window-close-symbolic", "Close");
    let win_close = window.clone();
    close.connect_clicked(move |_| win_close.close());

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.add_css_class("pin-bar");
    bar.append(&opacity);
    bar.append(&close);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&picture);
    root.append(&bar);
    window.set_child(Some(&root));
    window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let win_esc = window.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            win_esc.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    let win_scroll = window.clone();
    scroll.connect_scroll(move |_, _, dy| {
        if dy == 0.0 {
            return glib::Propagation::Proceed;
        }
        let factor = if dy < 0.0 { 1.08 } else { 0.92 };
        let w = win_scroll.width().max(1);
        let h = win_scroll.height().max(1);
        let (nw, nh) = scale_size(w, h, factor, MIN_EDGE, MAX_EDGE);
        win_scroll.set_default_size(nw, nh);
        glib::Propagation::Stop
    });
    picture.add_controller(scroll);

    window.connect_realize(|w| set_wayland_app_id(w, CLASS));
    let win_forget = window.clone();
    window.connect_close_request(move |_| {
        PINS.with(|p| p.borrow_mut().retain(|w| w != &win_forget));
        glib::Propagation::Proceed
    });

    PINS.with(|p| p.borrow_mut().push(window.clone()));
    window.present();
    glib::timeout_add_local_once(std::time::Duration::from_millis(80), || {
        hypr_pin_latest();
    });
}

fn set_wayland_app_id(window: &impl IsA<gtk::Native>, id: &str) {
    let Some(surface) = window.surface() else { return };
    if !surface.type_().name().contains("Wayland") {
        return;
    }
    let Ok(c_id) = CString::new(id) else { return };
    use glib::translate::ToGlibPtr;
    let raw: *mut gdk::ffi::GdkSurface = surface.to_glib_none().0;
    unsafe {
        gdk_wayland_toplevel_set_application_id(raw as *mut c_void, c_id.as_ptr());
    }
}

fn hypr_pin_latest() {
    let Ok(out) = std::process::Command::new("hyprctl").args(["clients", "-j"]).output() else { return };
    if !out.status.success() {
        return;
    }
    let Ok(list) = serde_json::from_slice::<Vec<HyprClient>>(&out.stdout) else { return };
    let Some(client) = list.iter().filter(|c| c.title == TITLE && !c.pinned).min_by_key(|c| c.focus_history_id) else {
        return;
    };
    let addr = client.address.clone();
    // Pin is a toggle: try each way until one has pinned it, and no more after that.
    let pinned = || {
        std::process::Command::new("hyprctl")
            .args(["clients", "-j"])
            .output()
            .ok()
            .and_then(|o| serde_json::from_slice::<Vec<HyprClient>>(&o.stdout).ok())
            .is_some_and(|list| list.iter().any(|c| c.address == addr && c.pinned))
    };
    let attempts = [
        vec!["eval".to_string(), format!(r#"hl.dispatch(hl.dsp.pin({{ window = "{addr}" }}))"#)],
        vec!["eval".to_string(), format!(r#"hl.dispatch("pin", "address:{addr}")"#)],
        vec!["dispatch".to_string(), "pin".to_string(), format!("address:{addr}")],
    ];
    for args in attempts {
        let _ = std::process::Command::new("hyprctl").args(&args).status();
        if pinned() {
            return;
        }
    }
}

#[derive(serde::Deserialize)]
struct HyprClient {
    address: String,
    title: String,
    #[serde(default)]
    pinned: bool,
    #[serde(rename = "focusHistoryID", default)]
    focus_history_id: i32,
}

pub fn fitted_size(img_w: u32, img_h: u32, max_w: i32, max_h: i32) -> (i32, i32) {
    if img_w == 0 || img_h == 0 {
        return (max_w, max_h);
    }
    let s = (max_w as f64 / img_w as f64).min(max_h as f64 / img_h as f64);
    let w = (img_w as f64 * s).round() as i32;
    let h = (img_h as f64 * s).round() as i32;
    (w.max(1), h.max(1))
}

pub fn scale_size(w: i32, h: i32, factor: f64, min_edge: i32, max_edge: i32) -> (i32, i32) {
    let mut nw = w as f64 * factor;
    let mut nh = h as f64 * factor;
    let long = nw.max(nh);
    if long > max_edge as f64 && long > 0.0 {
        let s = max_edge as f64 / long;
        nw *= s;
        nh *= s;
    }
    let short = nw.min(nh);
    if short < min_edge as f64 && short > 0.0 {
        let s = min_edge as f64 / short;
        nw *= s;
        nh *= s;
    }
    (nw.round() as i32, nh.round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitted_keeps_aspect() {
        assert_eq!(fitted_size(1920, 1080, 480, 320), (480, 270));
        assert_eq!(fitted_size(100, 200, 480, 320), (160, 320));
    }

    #[test]
    fn scroll_resize_keeps_aspect_and_clamps() {
        let (w, h) = scale_size(400, 200, 1.08, 160, 1600);
        assert_eq!((w as f64 / h as f64 * 10.0).round() as i32, 20);
        let (w, h) = scale_size(200, 200, 0.1, 160, 1600);
        assert_eq!((w, h), (160, 160));
        let (w, h) = scale_size(1500, 1500, 2.0, 160, 1600);
        assert_eq!((w, h), (1600, 1600));
    }
}
