//! A short note at the bottom of the screen, for results that have no shot card: a
//! copied colour, a QR code. It never takes the keyboard and goes by itself.

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use nyx_capture_core::geom::Rect;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

const STAYS: Duration = Duration::from_millis(3500);
const STAYS_WITH_ACTION: Duration = Duration::from_millis(6000);

thread_local! {
    static SHOWN: RefCell<Option<gtk::Window>> = const { RefCell::new(None) };
}

/// One button on the note, which closes it once clicked.
pub struct Action {
    pub label: &'static str,
    pub run: Rc<dyn Fn()>,
}

/// Show `message` on the monitor holding `area`, with a colour swatch and a button
/// when given. A newer note replaces this one.
pub fn show(area: Rect, message: &str, swatch: Option<[u8; 3]>, action: Option<Action>) {
    close();
    let monitor = match crate::overlay::output_for(area) {
        Ok((monitor, _)) => monitor,
        Err(err) => {
            crate::naming::log_error("notice", &err);
            return;
        }
    };
    let window = gtk::Window::new();
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("nyx-capture-notice"));
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_anchor(Edge::Bottom, true);
    window.set_margin(Edge::Bottom, 36);
    window.set_exclusive_zone(0);
    window.set_monitor(Some(&monitor));
    window.add_css_class("capture-window");
    window.add_css_class("capture-card");
    window.set_decorated(false);
    window.set_resizable(false);
    if let Some(app) = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) {
        window.set_application(Some(&app));
    }

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    body.add_css_class("toast");
    body.add_css_class("capture-notice");
    if let Some([r, g, b]) = swatch {
        let dot = gtk::DrawingArea::new();
        dot.set_content_width(18);
        dot.set_content_height(18);
        dot.set_valign(gtk::Align::Center);
        dot.set_draw_func(move |_, cr, w, h| {
            let (w, h) = (w as f64, h as f64);
            cr.arc(w / 2.0, h / 2.0, w.min(h) / 2.0 - 0.5, 0.0, std::f64::consts::TAU);
            cr.set_source_rgb(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
            let _ = cr.fill_preserve();
            cr.set_source_rgba(0.5, 0.5, 0.5, 0.5);
            cr.set_line_width(1.0);
            let _ = cr.stroke();
        });
        body.append(&dot);
    }
    let label = gtk::Label::new(Some(message));
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    label.set_max_width_chars(60);
    body.append(&label);
    let stays = if action.is_some() { STAYS_WITH_ACTION } else { STAYS };
    // Only a note with a button takes clicks; others let them through.
    window.set_can_target(action.is_some());
    if let Some(action) = action {
        let b = gtk::Button::with_label(action.label);
        b.add_css_class("flat");
        b.add_css_class("toast-action");
        b.connect_clicked(move |_| {
            (action.run)();
            close();
        });
        body.append(&b);
    }
    window.set_child(Some(&body));
    SHOWN.with(|s| *s.borrow_mut() = Some(window.clone()));
    window.present();
    glib::timeout_add_local_once(stays, move || {
        let current = SHOWN.with(|s| s.borrow().as_ref() == Some(&window));
        if current {
            close();
        }
    });
}

fn close() {
    if let Some(w) = SHOWN.with(|s| s.borrow_mut().take()) {
        w.set_child(None::<&gtk::Widget>);
        w.close();
    }
}
