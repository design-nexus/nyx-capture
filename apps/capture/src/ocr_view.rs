//! Recognized lines over the still, with copy.

use crate::widgets;
use gtk::cairo;
use gtk::prelude::*;
use image::RgbaImage;
use nyx_capture_core::ocr::OcrResult;
use nyx_capture_core::clipboard;

/// The text over the picture it came from. Without a parent (Text mode's review, with
/// the history window hidden) it stands alone.
pub fn show(parent: Option<&gtk::Window>, img: &RgbaImage, result: &OcrResult) {
    let (dialog, card) = widgets::dialog("Text", 640);
    dialog.set_transient_for(parent);
    dialog.add_css_class("capture-window");
    dialog.set_default_height(520);

    let canvas = gtk::DrawingArea::new();
    canvas.set_content_height(220);
    canvas.set_hexpand(true);
    canvas.set_vexpand(true);
    let surface = rgba_surface(img);
    let lines = result.lines.clone();
    let (iw, ih) = (img.width() as f64, img.height() as f64);
    canvas.set_draw_func(move |_, cr, w, h| {
        let ww = w as f64;
        let wh = h as f64;
        if iw < 1.0 || ih < 1.0 {
            return;
        }
        let scale = (ww / iw).min(wh / ih).max(0.01);
        let dw = iw * scale;
        let dh = ih * scale;
        let ox = (ww - dw) / 2.0;
        let oy = (wh - dh) / 2.0;
        if let Some(surface) = &surface {
            let _ = cr.save();
            cr.translate(ox, oy);
            cr.scale(scale, scale);
            if cr.set_source_surface(surface, 0.0, 0.0).is_ok() {
                cr.source().set_filter(cairo::Filter::Nearest);
                let _ = cr.paint();
            }
            let _ = cr.restore();
        }
        // Text boxes in the theme's accent.
        let accent = gtk::gdk::RGBA::parse(&crate::theme::palette().accent).unwrap_or(gtk::gdk::RGBA::BLUE);
        cr.set_source_rgba(accent.red() as f64, accent.green() as f64, accent.blue() as f64, 0.9);
        cr.set_line_width(1.0);
        for line in &lines {
            cr.rectangle(ox + line.x as f64 * scale, oy + line.y as f64 * scale, line.w as f64 * scale, line.h as f64 * scale);
            let _ = cr.stroke();
        }
    });
    card.append(&canvas);

    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_monospace(true);
    view.buffer().set_text(&result.text);
    let scroll = gtk::ScrolledWindow::builder()
        .min_content_height(140)
        .vexpand(true)
        .child(&view)
        .build();
    card.append(&scroll);

    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let copy = gtk::Button::with_label("Copy");
    copy.add_css_class("suggested-action");
    let close = gtk::Button::with_label("Close");
    buttons.append(&copy);
    buttons.append(&close);
    card.append(&buttons);

    let text = result.text.clone();
    let d = dialog.clone();
    copy.connect_clicked(move |_| {
        if clipboard::copy_text(&text).is_ok() {
            d.close();
        }
    });
    let d = dialog.clone();
    close.connect_clicked(move |_| d.close());
    dialog.present();
}

fn rgba_surface(img: &RgbaImage) -> Option<cairo::ImageSurface> {
    let (w, h) = img.dimensions();
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w as i32, h as i32).ok()?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().ok()?;
        for (y, row) in img.rows().enumerate() {
            for (x, p) in row.enumerate() {
                let a = p.0[3] as u32;
                let m = |c: u8| (c as u32 * a + 127) / 255;
                let px = (a << 24) | (m(p.0[0]) << 16) | (m(p.0[1]) << 8) | m(p.0[2]);
                data[y * stride + x * 4..][..4].copy_from_slice(&px.to_ne_bytes());
            }
        }
    }
    Some(surface)
}