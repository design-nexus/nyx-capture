//! The shot card: after a capture, the shot in the corner of its monitor with its
//! what happened to it and its size, and Edit, Copy, Text, Folder, Keep and Close.
//! It goes after the Card stays setting; pointing at it stops the clock, Keep stops it
//! for good. New cards stack above kept ones.

use crate::overlay;
use crate::paths;
use crate::prefs;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use image::RgbaImage;
use nyx_capture_core::capture::Shot;
use nyx_capture_core::clipboard;
use nyx_capture_core::ocr::{OcrMode, recognize};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

const INNER: i32 = 340;
const MIN_THUMB: i32 = 110;
const MAX_THUMB: i32 = 240;
/// The card's shadow is drawn inside its surface, this far from the visible edge.
const SHADOW: i32 = 12;
/// `.shot-card` padding.
const PADDING: i32 = 10;
const EDGE: i32 = 16;
const GAP: i32 = 10;
const TICK: Duration = Duration::from_millis(50);

/// What happened to the shot, for the card's status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Done {
    CopiedSaved,
    Copied,
    Saved,
    TextCopied,
    /// A recording, with its format ("MP4").
    Video(&'static str),
}

impl Done {
    fn text(self, folder: &str) -> String {
        match self {
            Done::CopiedSaved => format!("Copied · saved to {folder}"),
            Done::Copied => "Copied".into(),
            Done::Saved => format!("Saved to {folder}"),
            Done::TextCopied => "Text copied".into(),
            Done::Video(format) => format!("Saved to {folder} · {format}"),
        }
    }
}

struct Card {
    window: gtk::Window,
    body: gtk::Box,
    /// None for a recording.
    shot: Option<Shot>,
    file: RefCell<Option<PathBuf>>,
    connector: Option<String>,
    done: Cell<Done>,
    taken: String,
    kept: Cell<bool>,
    hovered: Cell<bool>,
    closed: Cell<bool>,
    elapsed: Cell<Duration>,
    tick: RefCell<Option<glib::SourceId>>,
    status: gtk::Label,
    life: gtk::DrawingArea,
    badge: gtk::Box,
    pin: gtk::ToggleButton,
}

thread_local! {
    static CARDS: RefCell<Vec<Rc<Card>>> = const { RefCell::new(Vec::new()) };
}

/// Show a card for `shot`. `file` is where it was saved (None when only copied);
/// `tag` marks the picture's corner (a scrolled shot).
pub fn show(shot: Shot, file: Option<PathBuf>, done: Done, tag: Option<&'static str>) {
    if !prefs::get().shot_card {
        return;
    }
    // Cards nobody kept make way for the new one.
    let stale: Vec<Rc<Card>> = CARDS.with(|c| c.borrow().iter().filter(|c| !c.kept.get()).cloned().collect());
    for card in stale {
        close(&card);
    }
    let preview = shot.image.to_rgba8();
    let (area, size) = (shot.area, (shot.image.width(), shot.image.height()));
    if let Err(err) = build(Some(shot), preview, area, size, None, file, done, tag) {
        crate::naming::log_error("card", &err);
    }
}

/// Hide every card (so none ends up in the next shot); true if any was showing.
pub fn hide_all() -> bool {
    let cards: Vec<Rc<Card>> = CARDS.with(|c| c.borrow().clone());
    let mut any = false;
    for card in cards.iter().filter(|c| c.window.is_visible()) {
        card.window.set_visible(false);
        any = true;
    }
    any
}

/// Bring back the cards [`hide_all`] hid.
pub fn unhide_all() {
    let cards: Vec<Rc<Card>> = CARDS.with(|c| c.borrow().clone());
    for card in cards.iter().filter(|c| !c.closed.get()) {
        card.window.set_visible(true);
    }
    restack();
}

/// A card for a recording: a frame from it, its length, Play.
pub fn show_video(path: PathBuf) {
    if !prefs::get().shot_card {
        return;
    }
    // Reading the frame takes a moment: the app mustn't quit before the card is up.
    let hold = gio::Application::default().map(|a| a.hold());
    crate::cmd::background(
        {
            let path = path.clone();
            move || video_info(&path)
        },
        move |info| {
            let (frame, size, length) = info.unwrap_or_else(|| (RgbaImage::from_pixel(16, 9, image::Rgba([30, 30, 46, 255])), (0, 0), None));
            let format = match path.extension().and_then(|e| e.to_str()) {
                Some("webm") => "WebM",
                Some("gif") => "GIF",
                _ => "MP4",
            };
            let stale: Vec<Rc<Card>> = CARDS.with(|c| c.borrow().iter().filter(|c| !c.kept.get()).cloned().collect());
            for card in stale {
                close(&card);
            }
            let area = overlay::focused().map(|(_, r)| r).unwrap_or(nyx_capture_core::geom::Rect::new(0, 0, 1, 1));
            if let Err(err) = build(None, frame, area, size, length, Some(path), Done::Video(format), None) {
                crate::naming::log_error("card", &err);
            }
            drop(hold);
        },
    );
}

/// A frame from half a second in, the video's size, and its length ("0:42").
fn video_info(path: &Path) -> Option<(RgbaImage, (u32, u32), Option<String>)> {
    let p = path.to_string_lossy();
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-ss", "0.5", "-i", &p, "-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"])
        .output()
        .ok()?;
    let frame = image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Png).ok()?.to_rgba8();
    let size = frame.dimensions();
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", &p])
        .output()
        .ok();
    let length = probe
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok())
        .map(|secs| crate::recording::clock(secs.round() as u64));
    Some((frame, size, length))
}

#[allow(clippy::too_many_arguments)]
fn build(
    shot: Option<Shot>,
    rgba: RgbaImage,
    area: nyx_capture_core::geom::Rect,
    size: (u32, u32),
    length: Option<String>,
    file: Option<PathBuf>,
    done: Done,
    tag: Option<&str>,
) -> anyhow::Result<()> {
    let video = shot.is_none();
    let (monitor, _) = overlay::output_for(area)?;
    let connector = monitor.connector().map(|c| c.to_string());
    let thumb_h = thumb_height(rgba.width(), rgba.height());
    let thumb = cover(&rgba, INNER, thumb_h, monitor.scale_factor().max(1));
    let (tw, th) = thumb.dimensions();
    let surface = cairo_surface(&thumb);
    let texture = gdk::MemoryTexture::new(tw as i32, th as i32, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from(&thumb.into_raw()), tw as usize * 4);

    let window = gtk::Window::new();
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("nyx-capture-card"));
    // Never the keyboard: what you type after a capture stays where you were typing.
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Right, true);
    window.set_margin(Edge::Right, EDGE - SHADOW);
    window.set_margin(Edge::Bottom, EDGE - SHADOW);
    window.set_exclusive_zone(0);
    window.set_monitor(Some(&monitor));
    window.add_css_class("capture-window");
    window.add_css_class("capture-card");
    window.set_decorated(false);
    window.set_resizable(false);
    if let Some(app) = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) {
        window.set_application(Some(&app));
    }

    // The picture (drawn at its card size whatever the screen scale), with the Kept
    // badge over its corner.
    let picture = gtk::DrawingArea::new();
    picture.set_content_width(INNER);
    picture.set_content_height(thumb_h);
    picture.set_draw_func(move |_, cr, w, h| {
        let Some(surface) = &surface else { return };
        let (sw, sh) = (surface.width().max(1) as f64, surface.height().max(1) as f64);
        cr.scale(w as f64 / sw, h as f64 / sh);
        if cr.set_source_surface(surface, 0.0, 0.0).is_ok() {
            cr.source().set_filter(gtk::cairo::Filter::Good);
            let _ = cr.paint();
        }
    });
    let badge = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    badge.add_css_class("shot-badge");
    badge.append(&gtk::Image::from_icon_name("capture-pin-symbolic"));
    badge.append(&gtk::Label::new(Some("Kept")));
    badge.set_halign(gtk::Align::Start);
    badge.set_valign(gtk::Align::Start);
    badge.set_visible(false);
    let thumb_box = gtk::Overlay::new();
    thumb_box.add_css_class("shot-thumb");
    thumb_box.set_overflow(gtk::Overflow::Hidden);
    thumb_box.set_child(Some(&picture));
    thumb_box.add_overlay(&badge);
    if video {
        let play = gtk::Image::from_icon_name("capture-play-symbolic");
        play.add_css_class("shot-play");
        play.set_halign(gtk::Align::Center);
        play.set_valign(gtk::Align::Center);
        thumb_box.add_overlay(&play);
        if let Some(length) = &length {
            let dur = gtk::Label::new(Some(length));
            dur.add_css_class("shot-badge");
            dur.add_css_class("shot-length");
            dur.set_halign(gtk::Align::End);
            dur.set_valign(gtk::Align::End);
            thumb_box.add_overlay(&dur);
        }
    }
    if let Some(tag) = tag {
        let mark = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        mark.add_css_class("shot-badge");
        mark.append(&gtk::Image::from_icon_name("capture-mode-scroll-symbolic"));
        mark.append(&gtk::Label::new(Some(tag)));
        mark.set_halign(gtk::Align::End);
        mark.set_valign(gtk::Align::Start);
        thumb_box.add_overlay(&mark);
    }
    thumb_box.set_tooltip_text(Some("Drag to another app"));

    // What happened to it, and its size.
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let size = gtk::Label::new(Some(&format!("{} × {}", crate::scrolling::thousands(size.0), crate::scrolling::thousands(size.1))));
    size.add_css_class("shot-size");
    let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    status_row.add_css_class("shot-status");
    status_row.set_margin_start(4);
    status_row.set_margin_end(4);
    status_row.set_margin_top(10);
    status_row.set_margin_bottom(8);
    status_row.append(&status);
    status_row.append(&size);

    // Actions: Edit, Copy, Text for a shot; Play and Copy file for a recording.
    let edit = if video { action_button("capture-play-symbolic", "Play") } else { action_button("capture-edit-symbolic", "Edit") };
    edit.add_css_class("suggested-action");
    let copy = action_button("capture-copy-symbolic", if video { "Copy file" } else { "Copy image" });
    let text = action_button("capture-text-symbolic", "Copy text");
    text.set_visible(!video);
    let folder = action_button("capture-folder-symbolic", "Show in folder");
    let pin = gtk::ToggleButton::new();
    pin.set_icon_name("capture-pin-symbolic");
    pin.add_css_class("flat");
    pin.add_css_class("icon-button");
    pin.set_tooltip_text(Some("Keep on screen"));
    let close_btn = action_button("capture-close-symbolic", "Close");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let acts = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    for w in [edit.upcast_ref::<gtk::Widget>(), copy.upcast_ref(), text.upcast_ref(), folder.upcast_ref(), pin.upcast_ref(), spacer.upcast_ref(), close_btn.upcast_ref()] {
        acts.append(w);
    }

    // How long it stays.
    let life = gtk::DrawingArea::new();
    life.set_content_height(3);
    life.set_margin_top(8);
    life.set_margin_start(4);
    life.set_margin_end(4);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.add_css_class("shot-card");
    body.set_size_request(INNER, -1);
    body.append(&thumb_box);
    body.append(&status_row);
    body.append(&acts);
    body.append(&life);
    window.set_child(Some(&body));

    let folder_name = file.as_deref().and_then(Path::parent).and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let taken = glib::DateTime::now_local().ok().and_then(|t| t.format("%-I:%M %p").ok()).map(|s| s.to_string()).unwrap_or_default();
    let taken = if folder_name.is_empty() { taken } else { format!("{taken} · {folder_name}") };
    let card = Rc::new(Card {
        window: window.clone(),
        body,
        shot,
        file: RefCell::new(file),
        connector,
        done: Cell::new(done),
        taken,
        kept: Cell::new(false),
        hovered: Cell::new(false),
        closed: Cell::new(false),
        elapsed: Cell::new(Duration::ZERO),
        tick: RefCell::new(None),
        status,
        life: life.clone(),
        badge,
        pin: pin.clone(),
    });
    sync_status(&card);

    let weak = Rc::downgrade(&card);
    life.set_draw_func(move |_, cr, w, h| {
        if let Some(card) = weak.upgrade() {
            draw_life(&card, cr, w as f64, h as f64);
        }
    });

    let weak = Rc::downgrade(&card);
    let with = move |f: fn(&Rc<Card>)| {
        let weak = weak.clone();
        move || {
            if let Some(card) = weak.upgrade() {
                f(&card);
            }
        }
    };
    let on = with(open_editor);
    edit.connect_clicked(move |_| on());
    let on = with(copy_image);
    copy.connect_clicked(move |_| on());
    let _ = video;
    let on = with(copy_text);
    text.connect_clicked(move |_| on());
    let on = with(show_in_folder);
    folder.connect_clicked(move |_| on());
    let on = with(|c| set_kept(c, c.pin.is_active()));
    pin.connect_toggled(move |_| on());
    let on = with(close);
    close_btn.connect_clicked(move |_| on());

    let motion = gtk::EventControllerMotion::new();
    let on = with(|c| {
        c.hovered.set(true);
        c.elapsed.set(Duration::ZERO);
        c.life.queue_draw();
    });
    motion.connect_enter(move |_, _, _| on());
    let on = with(|c| {
        c.hovered.set(false);
        c.elapsed.set(Duration::ZERO);
        c.life.queue_draw();
    });
    motion.connect_leave(move |_| on());
    window.add_controller(motion);

    let drag = gtk::DragSource::new();
    drag.set_actions(gdk::DragAction::COPY);
    let weak = Rc::downgrade(&card);
    drag.connect_prepare(move |_, _, _| {
        let card = weak.upgrade()?;
        let path = ensure_file(&card)?;
        let files = gdk::FileList::from_array(&[gio::File::for_path(path)]);
        Some(gdk::ContentProvider::for_value(&files.to_value()))
    });
    let weak = Rc::downgrade(&card);
    drag.connect_drag_begin(move |source, _| {
        source.set_icon(Some(&texture), 0, 0);
        // Dragging a card keeps it.
        if let Some(card) = weak.upgrade() {
            card.pin.set_active(true);
        }
    });
    thumb_box.add_controller(drag);

    let on = with(finish);
    window.connect_close_request(move |_| {
        on();
        glib::Propagation::Proceed
    });

    // Debug builds: show a card as kept, for checking the look.
    #[cfg(debug_assertions)]
    if std::env::var_os("CAPTURE_TEST_KEEP").is_some() {
        card.pin.set_active(true);
    }

    CARDS.with(|c| c.borrow_mut().push(card.clone()));
    restack();
    window.present();
    start_clock(&card);
    Ok(())
}

fn action_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.add_css_class("icon-button");
    b.set_tooltip_text(Some(tooltip));
    b
}

/// Oldest at the bottom, each card above the one before, per monitor.
fn restack() {
    let cards: Vec<Rc<Card>> = CARDS.with(|c| c.borrow().clone());
    let mut tops: Vec<(Option<String>, i32)> = Vec::new();
    for card in cards.iter().filter(|c| !c.closed.get()) {
        let bottom = tops.iter().find(|(m, _)| *m == card.connector).map(|(_, b)| *b).unwrap_or(EDGE - SHADOW);
        card.window.set_margin(Edge::Bottom, bottom);
        // The measured height includes the shadow margin above and below.
        let (_, natural, _, _) = card.body.measure(gtk::Orientation::Vertical, INNER + 2 * PADDING + 2 * SHADOW);
        let next = bottom + natural - 2 * SHADOW + GAP;
        match tops.iter_mut().find(|(m, _)| *m == card.connector) {
            Some(slot) => slot.1 = next,
            None => tops.push((card.connector.clone(), next)),
        }
    }
}

fn card_secs() -> Option<Duration> {
    match prefs::get().card_secs {
        0 => None,
        n => Some(Duration::from_secs(n as u64)),
    }
}

fn start_clock(card: &Rc<Card>) {
    if card_secs().is_none() {
        card.life.set_visible(false);
        return;
    }
    let weak = Rc::downgrade(card);
    let id = glib::timeout_add_local(TICK, move || {
        let Some(card) = weak.upgrade() else { return glib::ControlFlow::Break };
        let Some(limit) = card_secs() else { return glib::ControlFlow::Continue };
        if card.kept.get() || card.hovered.get() {
            return glib::ControlFlow::Continue;
        }
        let elapsed = card.elapsed.get() + TICK;
        card.elapsed.set(elapsed);
        card.life.queue_draw();
        if elapsed >= limit {
            card.tick.replace(None);
            close(&card);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
    card.tick.replace(Some(id));
}

fn draw_life(card: &Card, cr: &gtk::cairo::Context, w: f64, h: f64) {
    let Some(limit) = card_secs() else { return };
    let left = 1.0 - card.elapsed.get().as_secs_f64() / limit.as_secs_f64().max(0.001);
    let p = crate::theme::palette();
    let (Ok(track), Ok(accent)) = (gdk::RGBA::parse(&p.text), gdk::RGBA::parse(&p.accent)) else { return };
    let bar = |cr: &gtk::cairo::Context, width: f64| {
        let r = h / 2.0;
        cr.new_sub_path();
        cr.arc(r, r, r, std::f64::consts::FRAC_PI_2, 3.0 * std::f64::consts::FRAC_PI_2);
        cr.arc((width - r).max(r), r, r, -std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2);
        cr.close_path();
        let _ = cr.fill();
    };
    cr.set_source_rgba(track.red() as f64, track.green() as f64, track.blue() as f64, 0.1);
    bar(cr, w);
    let alpha = if card.hovered.get() { 0.45 } else { 1.0 };
    cr.set_source_rgba(accent.red() as f64, accent.green() as f64, accent.blue() as f64, alpha);
    bar(cr, (w * left.clamp(0.0, 1.0)).max(h));
}

fn set_kept(card: &Rc<Card>, kept: bool) {
    card.kept.set(kept);
    card.elapsed.set(Duration::ZERO);
    card.badge.set_visible(kept);
    card.life.set_visible(!kept && card_secs().is_some());
    card.pin.set_tooltip_text(Some(if kept { "Stop keeping on screen" } else { "Keep on screen" }));
    sync_status(card);
    restack();
}

fn sync_status(card: &Card) {
    let row = card.status.parent();
    if card.kept.get() {
        card.status.set_text(&card.taken);
        if let Some(row) = row {
            row.add_css_class("quiet");
        }
    } else {
        let folder = card.file.borrow().as_deref().and_then(Path::parent).and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        card.status.set_text(&card.done.get().text(&folder));
        if let Some(row) = row {
            row.remove_css_class("quiet");
        }
    }
}

fn close(card: &Rc<Card>) {
    if card.closed.get() {
        return;
    }
    card.window.close();
    finish(card);
}

fn finish(card: &Rc<Card>) {
    if card.closed.replace(true) {
        return;
    }
    if let Some(id) = card.tick.take() {
        id.remove();
    }
    CARDS.with(|c| c.borrow_mut().retain(|c| !Rc::ptr_eq(c, card)));
    restack();
}

/// Edit a shot; play a recording.
fn open_editor(card: &Rc<Card>) {
    match &card.shot {
        Some(shot) => crate::editor::open(shot.clone()),
        None => {
            if let Some(path) = card.file.borrow().as_ref() {
                crate::cmd::spawn(&["xdg-open", &path.to_string_lossy()]);
            }
        }
    }
    if !card.kept.get() {
        close(card);
    }
}

/// Copy a shot's picture again; for a recording, the file (to paste into a chat or a folder).
fn copy_image(card: &Rc<Card>) {
    let shot = card.shot.clone();
    let file = card.file.borrow().clone();
    let weak = Rc::downgrade(card);
    crate::cmd::background(
        move || match (shot, file) {
            (Some(shot), _) => clipboard::copy_png(&shot.png()?),
            (None, Some(path)) => clipboard::copy_uri(&gio::File::for_path(&path).uri()),
            (None, None) => Ok(()),
        },
        move |result| {
            let Some(card) = weak.upgrade() else { return };
            match result {
                Ok(()) => flash(&card, if card.shot.is_some() { "Copied again" } else { "File copied" }),
                Err(err) => flash(&card, &err.to_string()),
            }
        },
    );
}

fn copy_text(card: &Rc<Card>) {
    let Some(image) = card.shot.as_ref().map(|s| s.image.clone()) else { return };
    let p = prefs::get();
    let (mode, langs) = (OcrMode::parse(&p.ocr_mode), p.ocr_langs);
    flash(card, "Reading text…");
    let weak = Rc::downgrade(card);
    crate::cmd::background(
        move || {
            let text = recognize(&image, mode, &langs)?.text;
            clipboard::copy_text(&text)?;
            anyhow::Ok(text)
        },
        move |result| {
            let Some(card) = weak.upgrade() else { return };
            match result {
                Ok(text) if text.trim().is_empty() => flash(&card, "No text found"),
                Ok(_) => flash(&card, "Text copied"),
                Err(err) => flash(&card, &err.to_string()),
            }
        },
    );
}

/// Say something in the status line for a moment, then go back to what it said.
fn flash(card: &Rc<Card>, text: &str) {
    card.status.set_text(text);
    let weak = Rc::downgrade(card);
    glib::timeout_add_local_once(Duration::from_millis(1800), move || {
        if let Some(card) = weak.upgrade() {
            sync_status(&card);
        }
    });
}

/// The saved file, or (for a copy-only shot) a PNG written to the cache.
fn ensure_file(card: &Card) -> Option<PathBuf> {
    if let Some(path) = card.file.borrow().clone() {
        return Some(path);
    }
    let dir = paths::cache_dir().join("cards");
    let path = nyx_capture_core::action::unique_png(&dir, "screenshot");
    nyx_capture_core::atomic_write(&path, &card.shot.as_ref()?.png().ok()?).ok()?;
    card.file.replace(Some(path.clone()));
    Some(path)
}

/// The file manager selects it (FileManager1), or the folder opens.
fn show_in_folder(card: &Rc<Card>) {
    let Some(path) = ensure_file(card) else { return };
    let folder = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let fallback = move || crate::cmd::spawn(&["xdg-open", &folder]);
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        fallback();
        return;
    };
    let uri = gio::File::for_path(&path).uri().to_string();
    bus.call(
        Some("org.freedesktop.FileManager1"),
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
        "ShowItems",
        Some(&(vec![uri], "").to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        3000,
        gio::Cancellable::NONE,
        move |result| {
            if result.is_err() {
                fallback();
            }
        },
    );
}

/// The picture as a cairo surface (premultiplied BGRA).
fn cairo_surface(img: &RgbaImage) -> Option<gtk::cairo::ImageSurface> {
    let (w, h) = img.dimensions();
    let mut surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w as i32, h as i32).ok()?;
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

/// The picture's height in the card: the shot's shape, within limits.
pub fn thumb_height(img_w: u32, img_h: u32) -> i32 {
    if img_w == 0 || img_h == 0 {
        return MIN_THUMB;
    }
    ((INNER as f64 * img_h as f64 / img_w as f64).round() as i32).clamp(MIN_THUMB, MAX_THUMB)
}

/// The part of the shot a `w`×`h` picture shows: the top of a tall shot, the middle of
/// a wide one.
pub fn cover_rect(img_w: u32, img_h: u32, w: i32, h: i32) -> (u32, u32, u32, u32) {
    let (iw, ih) = (img_w as f64, img_h as f64);
    let aspect = w as f64 / h.max(1) as f64;
    if iw / ih.max(1.0) > aspect {
        let cw = (ih * aspect).round().clamp(1.0, iw) as u32;
        ((img_w - cw) / 2, 0, cw, img_h)
    } else {
        let ch = (iw / aspect).round().clamp(1.0, ih) as u32;
        (0, 0, img_w, ch)
    }
}

/// The picture at the monitor's scale; never larger than the shot.
fn cover(image: &RgbaImage, w: i32, h: i32, scale: i32) -> RgbaImage {
    let (x, y, cw, ch) = cover_rect(image.width(), image.height(), w, h);
    let crop = image::imageops::crop_imm(image, x, y, cw, ch).to_image();
    let (tw, th) = ((w * scale) as u32, (h * scale) as u32);
    if cw <= tw || ch <= th {
        return crop;
    }
    image::imageops::thumbnail(&crop, tw, th)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_keeps_the_shape_within_limits() {
        assert_eq!(thumb_height(1600, 1000), 213);
        assert_eq!(thumb_height(100, 1000), 240);
        assert_eq!(thumb_height(4000, 100), 110);
        assert_eq!(thumb_height(0, 0), 110);
    }

    #[test]
    fn tall_shots_show_their_top_and_wide_ones_their_middle() {
        assert_eq!(cover_rect(100, 1000, 280, 200), (0, 0, 100, 71));
        assert_eq!(cover_rect(4000, 100, 280, 90), (1844, 0, 311, 100));
        assert_eq!(cover_rect(1600, 1000, 280, 175), (0, 0, 1600, 1000));
    }
}
