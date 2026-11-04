//! Scrolling capture: the chosen area stays live while you scroll the page under it;
//! Capture grabs it about ten times a second and stitches each new part below the last.
//! A dashed frame marks the area, a strip beside it shows the stitched image so far,
//! and a small bar has the count, Cancel and Done (or the capture key again).

use crate::overlay;
use gtk::cairo;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use image::RgbaImage;
use nyx_capture_core::capture::Shot;
use nyx_capture_core::geom::Rect;
use nyx_capture_core::scroll::Stitcher;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

const STRIP_W: i32 = 132;
const STRIP_INNER: i32 = 116;
const GAP: i32 = 16;
const EVERY: Duration = Duration::from_millis(90);

type Done = Box<dyn FnOnce(Option<Shot>)>;

/// What the grabbing thread reports.
enum Update {
    Grew { frames: usize, width: u32, height: u32, preview: RgbaImage },
    Finished(Option<RgbaImage>),
}

struct Session {
    windows: Vec<gtk::Window>,
    stop: Arc<AtomicBool>,
    keep: Arc<AtomicBool>,
    rx: mpsc::Receiver<Update>,
    count: gtk::Label,
    size: gtk::Label,
    preview: Rc<RefCell<Option<cairo::ImageSurface>>>,
    strip: Option<gtk::DrawingArea>,
    area: Rect,
    scale: f64,
    on_done: Option<Done>,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

pub fn is_active() -> bool {
    SESSION.with(|s| s.borrow().is_some())
}

/// Done: stitch what there is and hand it over.
pub fn done() {
    finish(true);
}

/// Cancel: throw it away.
pub fn cancel() {
    finish(false);
}

/// Start on `area` (layout coordinates). `on_done` gets the tall shot, or None.
pub fn start(area: Rect, on_done: impl FnOnce(Option<Shot>) + 'static) -> anyhow::Result<()> {
    if is_active() {
        finish(false);
    }
    let (monitor, layout) = overlay::output_for(area)?;
    let area = clip(area, layout);
    let scale = select_scale(&monitor);

    let mut windows = Vec::new();
    // The dim around the area, in four strips that let the pointer through, each with
    // the dashed edge on the side next to the area.
    let right = layout.x + layout.w;
    let bottom = layout.y + layout.h;
    for (r, edge) in [
        (Rect::new(layout.x, layout.y, layout.w, area.y - layout.y), Side::Below),
        (Rect::new(layout.x, area.y + area.h, layout.w, bottom - (area.y + area.h)), Side::Above),
        (Rect::new(layout.x, area.y, area.x - layout.x, area.h), Side::Right),
        (Rect::new(area.x + area.w, area.y, right - (area.x + area.w), area.h), Side::Left),
    ] {
        if r.w > 0 && r.h > 0 {
            windows.push(dim_strip(&monitor, layout, r, edge));
        }
    }

    // The bar: below the area when there's room, else above, else inside its foot.
    let count = gtk::Label::new(Some("Scroll the page"));
    count.add_css_class("scroll-count");
    let size = gtk::Label::new(Some(""));
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("scroll-cancel");
    let done_btn = gtk::Button::new();
    let done_row = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    done_row.append(&gtk::Image::from_icon_name("capture-check-symbolic"));
    done_row.append(&gtk::Label::new(Some("Done")));
    done_btn.set_child(Some(&done_row));
    done_btn.add_css_class("suggested-action");
    done_btn.add_css_class("scroll-done");
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("scroll-dot");
    dot.set_valign(gtk::Align::Center);
    let sep = gtk::Separator::new(gtk::Orientation::Vertical);
    sep.add_css_class("capture-bar-sep");
    sep.set_valign(gtk::Align::Center);
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    bar.add_css_class("capture-bar");
    bar.add_css_class("scroll-bar");
    bar.append(&dot);
    bar.append(&count);
    bar.append(&sep);
    bar.append(&cancel);
    bar.append(&done_btn);
    let (_, bar_w, _, _) = bar.measure(gtk::Orientation::Horizontal, -1);
    let (_, bar_h, _, _) = bar.measure(gtk::Orientation::Vertical, bar_w);
    let bar_x = (area.x + area.w / 2 - bar_w / 2).clamp(layout.x + 8, (right - bar_w - 8).max(layout.x + 8));
    // Below the area when there's room, else above it; else inside its foot, and then
    // that band is left out of the grabs so the bar is never in the shot.
    let mut grab_area = area;
    let bar_y = if area.y + area.h + GAP + bar_h <= bottom - 8 {
        area.y + area.h + GAP
    } else if area.y - GAP - bar_h >= layout.y + 8 {
        area.y - GAP - bar_h
    } else {
        let y = area.y + area.h - bar_h - GAP;
        grab_area.h = (y - GAP - area.y).max(1);
        y
    };
    let bar_win = layer(&monitor, layout, Rect::new(bar_x, bar_y, bar_w, bar_h), "nyx-capture-scroll");
    // No keyboard: the page under the area keeps all of its input while it scrolls.
    bar_win.set_keyboard_mode(KeyboardMode::None);
    bar_win.set_child(Some(&bar));
    cancel.connect_clicked(|_| finish(false));
    done_btn.connect_clicked(|_| finish(true));
    windows.push(bar_win);

    // The strip: to the right of the area when there's room, else to its left.
    let preview: Rc<RefCell<Option<cairo::ImageSurface>>> = Rc::new(RefCell::new(None));
    let strip_x = if area.x + area.w + GAP + STRIP_W <= right - 8 {
        Some(area.x + area.w + GAP)
    } else if area.x - GAP - STRIP_W >= layout.x + 8 {
        Some(area.x - GAP - STRIP_W)
    } else {
        None
    };
    let mut strip = None;
    if let Some(x) = strip_x {
        let label = gtk::Label::new(Some("SO FAR"));
        label.add_css_class("scroll-label");
        label.set_xalign(0.0);
        let canvas = gtk::DrawingArea::new();
        canvas.set_content_width(STRIP_INNER);
        canvas.set_vexpand(true);
        let surface = preview.clone();
        canvas.set_draw_func(move |_, cr, w, h| draw_strip(cr, w as f64, h as f64, surface.borrow().as_ref()));
        size.add_css_class("scroll-size");
        size.set_xalign(0.0);
        let col = gtk::Box::new(gtk::Orientation::Vertical, 8);
        col.add_css_class("capture-bar");
        col.add_css_class("scroll-strip");
        col.append(&label);
        col.append(&canvas);
        col.append(&size);
        let h = area.h.clamp(160, 460);
        let win = layer(&monitor, layout, Rect::new(x, area.y, STRIP_W, h), "nyx-capture-scroll");
        win.set_child(Some(&col));
        windows.push(win);
        strip = Some(canvas);
    }

    for w in &windows {
        w.present();
    }

    let stop = Arc::new(AtomicBool::new(false));
    let keep = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    {
        let (stop, keep) = (stop.clone(), keep.clone());
        std::thread::spawn(move || grab(grab_area, stop, keep, tx));
    }
    SESSION.with(|s| {
        *s.borrow_mut() = Some(Session {
            windows,
            stop,
            keep,
            rx,
            count,
            size,
            preview,
            strip,
            area: grab_area,
            scale,
            on_done: Some(Box::new(on_done)),
        })
    });
    glib::timeout_add_local(Duration::from_millis(100), poll);
    Ok(())
}

/// Grab the area until told to stop, stitching as it goes.
fn grab(area: Rect, stop: Arc<AtomicBool>, keep: Arc<AtomicBool>, tx: mpsc::Sender<Update>) {
    let mut stitcher = Stitcher::new();
    // The overlay fades out over about 150 ms after the pick: start after that.
    std::thread::sleep(Duration::from_millis(300));
    while !stop.load(Ordering::Relaxed) {
        if let Some(frame) = grab_frame(area)
            && stitcher.push(&frame) > 0
        {
            let image = stitcher.image();
            let preview = image::imageops::thumbnail(&image, STRIP_INNER as u32 * 2, ((image.height() as u64 * STRIP_INNER as u64 * 2) / image.width().max(1) as u64).max(1) as u32);
            let _ = tx.send(Update::Grew { frames: stitcher.frames(), width: image.width(), height: image.height(), preview });
        }
        std::thread::sleep(EVERY);
    }
    // One last look, so what's on screen when Done is pressed is in.
    if keep.load(Ordering::Relaxed)
        && let Some(frame) = grab_frame(area)
    {
        stitcher.push(&frame);
    }
    let image = (keep.load(Ordering::Relaxed) && stitcher.frames() > 0).then(|| stitcher.into_image());
    let _ = tx.send(Update::Finished(image));
}

fn grab_frame(area: Rect) -> Option<RgbaImage> {
    let out = std::process::Command::new("grim").args(["-t", "png", "-l", "0", "-g", &area.to_geometry(), "-"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Png).ok().map(|i| i.to_rgba8())
}

/// Take what the grabbing thread has said.
fn poll() -> glib::ControlFlow {
    let mut finished = None;
    let alive = SESSION.with(|s| {
        let s = s.borrow();
        let Some(sess) = s.as_ref() else { return false };
        while let Ok(update) = sess.rx.try_recv() {
            match update {
                Update::Grew { frames, width, height, preview } => {
                    sess.count.set_markup(&format!("<b>{frames}</b> <span alpha=\"70%\">frames · {} px tall</span>", thousands(height)));
                    sess.size.set_text(&format!("{} × {}", thousands(width), thousands(height)));
                    *sess.preview.borrow_mut() = surface_of(&preview);
                    if let Some(strip) = &sess.strip {
                        strip.queue_draw();
                    }
                }
                Update::Finished(image) => finished = Some(image),
            }
        }
        true
    });
    if let Some(image) = finished {
        hand_over(image);
        return glib::ControlFlow::Break;
    }
    if alive { glib::ControlFlow::Continue } else { glib::ControlFlow::Break }
}

/// Stop grabbing; the thread sends the result (or nothing, for Cancel) to `poll`.
fn finish(keep: bool) {
    SESSION.with(|s| {
        if let Some(sess) = s.borrow().as_ref() {
            sess.keep.store(keep, Ordering::Relaxed);
            sess.stop.store(true, Ordering::Relaxed);
            for w in &sess.windows {
                w.set_visible(false);
            }
        }
    });
}

fn hand_over(image: Option<RgbaImage>) {
    let Some(mut sess) = SESSION.with(|s| s.borrow_mut().take()) else { return };
    for w in std::mem::take(&mut sess.windows) {
        w.close();
    }
    let shot = image.map(|img| {
        let h = (img.height() as f64 / sess.scale).round() as i32;
        Shot::from_image(image::DynamicImage::ImageRgba8(img), Rect::new(sess.area.x, sess.area.y, sess.area.w, h), sess.scale, "scroll")
    });
    if let Some(done) = sess.on_done.take() {
        done(shot);
    }
}

#[derive(Clone, Copy)]
enum Side {
    /// The area is below this strip: the dashed edge runs along its bottom.
    Below,
    Above,
    Right,
    Left,
}

fn dim_strip(monitor: &gdk::Monitor, layout: Rect, r: Rect, side: Side) -> gtk::Window {
    let win = layer(monitor, layout, r, "nyx-capture-scroll");
    win.set_can_target(false);
    let canvas = gtk::DrawingArea::new();
    canvas.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (w as f64, h as f64);
        cr.set_source_rgba(9.0 / 255.0, 9.0 / 255.0, 16.0 / 255.0, 0.42);
        let _ = cr.paint();
        let p = crate::theme::palette();
        let Ok(accent) = gdk::RGBA::parse(&p.accent) else { return };
        cr.set_source_rgba(accent.red() as f64, accent.green() as f64, accent.blue() as f64, 1.0);
        cr.set_line_width(2.0);
        cr.set_dash(&[7.0, 5.0], 0.0);
        // Just outside the area, so the frame is never in a grab.
        match side {
            Side::Below => {
                cr.move_to(0.0, h - 2.0);
                cr.line_to(w, h - 2.0);
            }
            Side::Above => {
                cr.move_to(0.0, 2.0);
                cr.line_to(w, 2.0);
            }
            Side::Right => {
                cr.move_to(w - 2.0, 0.0);
                cr.line_to(w - 2.0, h);
            }
            Side::Left => {
                cr.move_to(2.0, 0.0);
                cr.line_to(2.0, h);
            }
        }
        let _ = cr.stroke();
    });
    win.set_child(Some(&canvas));
    // Nothing here takes the pointer: scrolling and clicking reach the page.
    win.connect_realize(|w| {
        if let Some(surface) = w.surface() {
            surface.set_input_region(Some(&cairo::Region::create()));
        }
    });
    win
}

fn layer(monitor: &gdk::Monitor, layout: Rect, r: Rect, namespace: &str) -> gtk::Window {
    let win = gtk::Window::new();
    win.init_layer_shell();
    win.set_layer(Layer::Overlay);
    win.set_namespace(Some(namespace));
    win.set_keyboard_mode(KeyboardMode::None);
    win.set_anchor(Edge::Top, true);
    win.set_anchor(Edge::Left, true);
    win.set_exclusive_zone(-1);
    win.set_margin(Edge::Left, r.x - layout.x);
    win.set_margin(Edge::Top, r.y - layout.y);
    win.set_monitor(Some(monitor));
    win.set_default_size(r.w, r.h);
    win.set_size_request(r.w, r.h);
    win.add_css_class("capture-window");
    win.add_css_class("capture-overlay");
    win.set_decorated(false);
    win.set_resizable(false);
    if let Some(app) = gtk::gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) {
        win.set_application(Some(&app));
    }
    win
}

fn draw_strip(cr: &cairo::Context, w: f64, h: f64, preview: Option<&cairo::ImageSurface>) {
    let p = crate::theme::palette();
    if let Ok(bg) = gdk::RGBA::parse(&p.bg) {
        cr.set_source_rgba(bg.red() as f64, bg.green() as f64, bg.blue() as f64, 1.0);
        rounded(cr, 0.0, 0.0, w, h, 8.0);
        let _ = cr.fill();
    }
    let Some(img) = preview else { return };
    let scale = w / img.width().max(1) as f64;
    let shown = img.height() as f64 * scale;
    // The newest part: the bottom of the stitched image when it's taller than the strip.
    let top = (shown - h).max(0.0);
    cr.save().ok();
    rounded(cr, 0.0, 0.0, w, h, 8.0);
    cr.clip();
    cr.translate(0.0, -top);
    cr.scale(scale, scale);
    if cr.set_source_surface(img, 0.0, 0.0).is_ok() {
        cr.source().set_filter(cairo::Filter::Good);
        let _ = cr.paint();
    }
    cr.restore().ok();
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let pi = std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -pi / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, pi / 2.0);
    cr.arc(x + r, y + h - r, r, pi / 2.0, pi);
    cr.arc(x + r, y + r, r, pi, 3.0 * pi / 2.0);
    cr.close_path();
}

fn surface_of(img: &RgbaImage) -> Option<cairo::ImageSurface> {
    let (w, h) = img.dimensions();
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w as i32, h as i32).ok()?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().ok()?;
        for (y, row) in img.rows().enumerate() {
            for (x, p) in row.enumerate() {
                let [r, g, b, a] = p.0;
                let m = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
                let px = u32::from_be_bytes([a, m(r), m(g), m(b)]);
                data[y * stride + x * 4..][..4].copy_from_slice(&px.to_ne_bytes());
            }
        }
    }
    Some(surface)
}

fn clip(r: Rect, bounds: Rect) -> Rect {
    let x0 = r.x.max(bounds.x);
    let y0 = r.y.max(bounds.y);
    let x1 = (r.x + r.w).min(bounds.x + bounds.w);
    let y1 = (r.y + r.h).min(bounds.y + bounds.h);
    Rect::new(x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
}

/// Image pixels per layout unit on this monitor (Hyprland's fractional scale).
fn select_scale(monitor: &gdk::Monitor) -> f64 {
    let name = monitor.connector().map(|c| c.to_string()).unwrap_or_default();
    nyx_capture_core::select::monitor_named(&name).ok().flatten().map(|m| m.scale).unwrap_or(monitor.scale())
}

/// 6240 → "6,240".
pub fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_numbers_get_commas() {
        assert_eq!(thousands(6240), "6,240");
        assert_eq!(thousands(1800), "1,800");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
