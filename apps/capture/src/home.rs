//! The Capture page, what the window opens on: every capture mode as a named card
//! with its key, the capture bar's options as switches, and the latest captures.
//! Under 980 px the modes go two to a row and the latest three show.

use crate::overlay::Mode;
use crate::paths;
use crate::prefs;
use crate::widgets;
use crate::window;
use gtk::glib;
use gtk::prelude::*;
use nyx_capture_core::search::View;
use nyx_capture_core::store::{self, CaptureKind, CaptureRow};
use std::cell::RefCell;

/// The page never grows wider than this: 900 px of cards and its padding.
const CLAMP: i32 = 932;
const GAP: i32 = 10;
const STRIP_WIDE: usize = 5;
const STRIP_NARROW: usize = 3;

struct Home {
    modes: gtk::Grid,
    cards: Vec<(Mode, gtk::Button, gtk::Label)>,
    edit: gtk::Switch,
    delay: gtk::Switch,
    after: gtk::DropDown,
    after_ids: Vec<&'static str>,
    options: gtk::Box,
    after_box: gtk::Box,
    see_all: gtk::Button,
    strip: gtk::Box,
    strip_note: gtk::Label,
    narrow: bool,
    /// The strip's width, which sets its pictures' height.
    width: i32,
    recent: Vec<CaptureRow>,
}

thread_local! {
    static HOME: RefCell<Option<Home>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Home) -> R) -> Option<R> {
    HOME.with(|h| h.borrow_mut().as_mut().map(f))
}

pub fn build() -> gtk::Widget {
    let body = widgets::vbox(0);
    body.add_css_class("capture-home");

    let modes = gtk::Grid::new();
    modes.add_css_class("home-modes");
    modes.set_column_spacing(GAP as u32);
    modes.set_row_spacing(GAP as u32);
    modes.set_column_homogeneous(true);
    let cards: Vec<_> = Mode::ALL.iter().map(|&m| mode_card(m)).collect();
    body.append(&modes);

    let p = prefs::get();
    let options = widgets::hbox(22);
    options.add_css_class("home-options");
    let (edit_row, edit) = switch_label("Edit after capture", p.capture_edit, |on| prefs::update(|p| p.capture_edit = on));
    let (delay_row, delay) = switch_label("Wait 3 seconds", p.capture_delay, |on| prefs::update(|p| p.capture_delay = on));
    options.append(&edit_row);
    options.append(&delay_row);
    let spacer = widgets::hbox(0);
    spacer.set_hexpand(true);
    options.append(&spacer);

    let choices = crate::settings_page::after_choices(&nyx_capture_core::detect::Tools::probe());
    let after_ids: Vec<&'static str> = choices.iter().map(|(id, _)| *id).collect();
    let after_labels: Vec<&str> = choices.iter().map(|(_, l)| *l).collect();
    let after = gtk::DropDown::from_strings(&after_labels);
    after.add_css_class("home-after");
    after.set_valign(gtk::Align::Center);
    after.set_tooltip_text(Some("What a screenshot does once you've chosen it"));
    let after_box = widgets::hbox(12);
    let after_label = widgets::label("After a screenshot", "home-option-label");
    after_box.append(&after_label);
    after_box.append(&after);
    options.append(&after_box);
    body.append(&options);

    let tip = widgets::hbox(8);
    tip.add_css_class("home-tip");
    tip.append(&gtk::Image::from_icon_name("input-keyboard-symbolic"));
    let tip_text = widgets::label("The Print key opens the same bar anywhere. Esc cancels.", "");
    tip_text.set_wrap(true);
    tip_text.set_xalign(0.0);
    tip.append(&tip_text);
    body.append(&tip);

    let head = widgets::hbox(8);
    head.add_css_class("home-section");
    let title = widgets::label("Recent", "home-section-title");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    head.append(&title);
    let see_all = gtk::Button::with_label("See all");
    see_all.add_css_class("flat");
    see_all.add_css_class("home-link");
    see_all.connect_clicked(|_| window::navigate(View::Recent));
    head.append(&see_all);
    body.append(&head);

    let strip = widgets::hbox(GAP);
    strip.set_homogeneous(true);
    strip.add_css_class("home-strip");
    let strip_note = widgets::label("", "home-strip-note");
    strip_note.set_xalign(0.0);
    strip_note.set_wrap(true);
    strip_note.set_visible(false);
    // The strip's width sets how tall its pictures are.
    let probe = gtk::DrawingArea::new();
    probe.set_can_target(false);
    probe.set_can_focus(false);
    probe.connect_resize(|_, width, _| {
        if with(|h| std::mem::replace(&mut h.width, width) != width).unwrap_or(false) {
            glib::idle_add_local_once(size_strip);
        }
    });
    let strip_host = gtk::Overlay::new();
    strip_host.set_child(Some(&strip));
    strip_host.add_overlay(&probe);
    strip_host.set_measure_overlay(&probe, false);
    body.append(&strip_host);
    body.append(&strip_note);

    let clamp = nyx_ui::clamp::Clamp::new(&body, CLAMP);
    let root = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .hexpand(true)
        .child(&clamp)
        .build();

    let at = after_ids.iter().position(|id| *id == p.after_capture).unwrap_or(0);
    after.set_selected(at as u32);
    after.connect_selected_notify(|d| {
        let id = with(|h| h.after_ids.get(d.selected() as usize).copied()).flatten();
        if let Some(id) = id
            && prefs::get().after_capture != id
        {
            prefs::update(|p| p.after_capture = id.to_string());
        }
    });

    HOME.with(|h| {
        *h.borrow_mut() = Some(Home {
            modes,
            cards,
            edit,
            delay,
            after,
            after_ids,
            options,
            after_box,
            see_all,
            strip,
            strip_note,
            narrow: false,
            width: 0,
            recent: Vec::new(),
        })
    });
    lay_out(false);
    refresh_options();
    root.upcast()
}

/// A mode's card: its icon in a tile, its key, its name and what it captures.
fn mode_card(mode: Mode) -> (Mode, gtk::Button, gtk::Label) {
    let top = widgets::hbox(0);
    let tile = widgets::hbox(0);
    tile.add_css_class("mode-tile");
    tile.set_halign(gtk::Align::Start);
    tile.set_valign(gtk::Align::Center);
    let icon = gtk::Image::from_icon_name(mode.icon());
    icon.set_halign(gtk::Align::Center);
    icon.set_hexpand(true);
    tile.append(&icon);
    top.append(&tile);
    let spacer = widgets::hbox(0);
    spacer.set_hexpand(true);
    top.append(&spacer);
    let key = widgets::label(&mode.key().to_string(), "mode-key");
    key.set_valign(gtk::Align::Center);
    top.append(&key);

    let name = widgets::label(mode.label(), "mode-name");
    name.set_xalign(0.0);
    let about = widgets::label(mode.about(), "mode-about");
    about.set_xalign(0.0);
    about.set_wrap(true);
    about.set_wrap_mode(gtk::pango::WrapMode::WordChar);

    let content = widgets::vbox(4);
    content.set_valign(gtk::Align::Start);
    content.append(&top);
    name.set_margin_top(6);
    content.append(&name);
    content.append(&about);

    let card = gtk::Button::new();
    card.set_child(Some(&content));
    card.add_css_class("capture-mode");
    if mode == Mode::Record {
        card.add_css_class("record");
    }
    let tip = format!("{} ({})", mode.label(), mode.key());
    card.set_tooltip_text(Some(&tip));
    card.update_property(&[gtk::accessible::Property::Label(mode.label()), gtk::accessible::Property::Description(mode.about())]);
    card.connect_clicked(move |_| window::start_mode(mode));
    (mode, card, about)
}

/// A switch with its name beside it; clicking the name flips it too.
fn switch_label(text: &str, on: bool, set: fn(bool)) -> (gtk::Box, gtk::Switch) {
    let row = widgets::hbox(8);
    let sw = gtk::Switch::new();
    sw.set_active(on);
    sw.set_valign(gtk::Align::Center);
    sw.connect_active_notify(move |s| set(s.is_active()));
    let label = widgets::label(text, "home-option");
    let click = gtk::GestureClick::new();
    let target = sw.clone();
    click.connect_released(move |_, _, _, _| target.set_active(!target.is_active()));
    label.add_controller(click);
    sw.update_property(&[gtk::accessible::Property::Label(text)]);
    row.append(&sw);
    row.append(&label);
    (row, sw)
}

/// Four modes to a row, or two under 980 px; the latest five captures, or three.
pub fn set_narrow(narrow: bool) {
    if with(|h| std::mem::replace(&mut h.narrow, narrow) != narrow) == Some(true) {
        lay_out(narrow);
        fill_strip();
    }
}

fn lay_out(narrow: bool) {
    with(|h| {
        let cols = if narrow { 2 } else { 4 };
        for (_, card, _) in &h.cards {
            if card.parent().is_some() {
                h.modes.remove(card);
            }
        }
        for (i, (_, card, _)) in h.cards.iter().enumerate() {
            h.modes.attach(card, (i % cols) as i32, (i / cols) as i32, 1, 1);
        }
        // Narrow, "After a screenshot" takes a line of its own under the switches.
        let body = h.options.parent().and_downcast::<gtk::Box>();
        if let Some(body) = body {
            if let Some(from) = h.after_box.parent().and_downcast::<gtk::Box>() {
                from.remove(&h.after_box);
            }
            if narrow {
                h.after_box.add_css_class("home-options");
                body.insert_child_after(&h.after_box, Some(&h.options));
            } else {
                h.after_box.remove_css_class("home-options");
                h.options.append(&h.after_box);
            }
        }
        if let Some(body) = h.modes.parent() {
            if narrow {
                body.add_css_class("narrow");
            } else {
                body.remove_css_class("narrow");
            }
        }
    });
}

/// The switches, After a screenshot and the mode Print starts on, from the settings.
pub fn refresh_options() {
    let p = prefs::get();
    let start = Mode::parse(&p.capture_start);
    with(|h| {
        h.edit.set_active(p.capture_edit);
        h.delay.set_active(p.capture_delay);
        if let Some(at) = h.after_ids.iter().position(|id| *id == p.after_capture)
            && h.after.selected() != at as u32
        {
            h.after.set_selected(at as u32);
        }
        for (mode, card, _) in &h.cards {
            if *mode == start {
                card.add_css_class("start");
            } else {
                card.remove_css_class("start");
            }
        }
    });
}

/// While a recording runs, the Record card stops it.
pub fn set_recording(on: bool) {
    with(|h| {
        if let Some((_, card, about)) = h.cards.iter().find(|(m, _, _)| *m == Mode::Record) {
            about.set_text(if on { "Recording now · click to stop" } else { Mode::Record.about() });
            card.set_tooltip_text(Some(if on { "Stop recording (5)" } else { "Record (5)" }));
        }
    });
}

/// The latest captures, as history has them (`hidden` are deleted but undoable).
pub fn refresh_recent(hidden: &[String]) {
    let history = prefs::get().history;
    let (recent, total) = if history {
        match store::open(&paths::library_db()).and_then(|db| Ok((db.list(View::Recent, "", (STRIP_WIDE + hidden.len()) as i64)?, db.count()?))) {
            Ok((mut rows, total)) => {
                rows.retain(|r| !hidden.contains(&r.id));
                rows.truncate(STRIP_WIDE);
                (rows, (total as usize).saturating_sub(hidden.len()))
            }
            Err(err) => {
                eprintln!("nyx-capture: {err}");
                (Vec::new(), 0)
            }
        }
    } else {
        (Vec::new(), 0)
    };
    with(|h| {
        h.see_all.set_visible(total > 0);
        h.see_all.set_label(&format!("See all {total}"));
        let note = if !history {
            "History is off, so captures aren't kept here. Turn it on in Settings → History."
        } else if recent.is_empty() {
            "Nothing captured yet. Pick a mode above, or press Print anywhere."
        } else {
            ""
        };
        h.strip_note.set_text(note);
        h.strip_note.set_visible(!note.is_empty());
        h.recent = recent;
    });
    fill_strip();
}

fn fill_strip() {
    let Some((strip, rows, count)) = with(|h| {
        let count = if h.narrow { STRIP_NARROW } else { STRIP_WIDE };
        (h.strip.clone(), h.recent.clone(), count)
    }) else {
        return;
    };
    while let Some(child) = strip.first_child() {
        strip.remove(&child);
    }
    strip.set_visible(!rows.is_empty());
    for i in 0..count {
        match rows.get(i) {
            Some(row) => strip.append(&shot(row)),
            // Keeps the cells their width when there are fewer captures than places.
            None => strip.append(&widgets::vbox(0)),
        }
    }
    size_strip();
}

/// One of the latest captures: its picture, title, and kind and time.
fn shot(row: &CaptureRow) -> gtk::Button {
    let kind = if row.kind == CaptureKind::Recording { "Recording" } else { "Screenshot" };
    let title = row.title.trim();
    let when = when(row.created_at);
    let (name, meta) = if title.is_empty() { (kind.to_string(), when) } else { (title.to_string(), format!("{kind} · {when}")) };

    let base = widgets::vbox(0);
    base.add_css_class("home-shot-base");
    let picture = gtk::Picture::new();
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_can_shrink(true);
    picture.add_css_class("history-picture");
    let frame = gtk::Overlay::new();
    frame.add_css_class("home-shot-picture");
    frame.set_overflow(gtk::Overflow::Hidden);
    frame.set_child(Some(&base));
    frame.add_overlay(&picture);
    frame.set_measure_overlay(&picture, false);
    frame.set_widget_name(&row.id);
    if row.starred {
        let star = gtk::Image::from_icon_name("starred-symbolic");
        star.add_css_class("home-shot-badge");
        star.add_css_class("star");
        star.set_halign(gtk::Align::End);
        star.set_valign(gtk::Align::Start);
        frame.add_overlay(&star);
    }
    if row.kind == CaptureKind::Recording {
        let play = gtk::Image::from_icon_name("capture-play-symbolic");
        play.add_css_class("home-shot-badge");
        play.set_halign(gtk::Align::Start);
        play.set_valign(gtk::Align::End);
        frame.add_overlay(&play);
    }
    crate::grid::fill_picture(&picture, &row.id, &row.path);

    let title = widgets::label(&name, "home-shot-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let meta = widgets::label(&meta, "home-shot-meta");
    meta.set_xalign(0.0);
    meta.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let content = widgets::vbox(0);
    content.append(&frame);
    content.append(&title);
    content.append(&meta);
    let b = gtk::Button::new();
    b.set_child(Some(&content));
    b.add_css_class("flat");
    b.add_css_class("home-shot");
    b.set_tooltip_text(Some(&name));
    let row = row.clone();
    b.connect_clicked(move |_| window::open_capture(row.clone()));
    b
}

/// The pictures at 16:10 of a cell's width.
fn size_strip() {
    let Some((strip, width, count)) = with(|h| (h.strip.clone(), h.width, if h.narrow { STRIP_NARROW } else { STRIP_WIDE })) else {
        return;
    };
    if width <= 0 {
        return;
    }
    let cell = (width - GAP * (count as i32 - 1)) / count as i32;
    let height = (cell * 10 / 16).max(60);
    let mut child = strip.first_child();
    while let Some(c) = child {
        if let Some(base) = c.first_child().and_then(|content| content.first_child()).and_then(|frame| frame.first_child()) {
            base.set_size_request(-1, height);
        }
        child = c.next_sibling();
    }
}

/// "14:02" today, else the day: "Yesterday", a weekday, a date.
fn when(created: i64) -> String {
    let day = crate::grid::day_label(created);
    if day == "Today" {
        glib::DateTime::from_unix_local(created).ok().and_then(|t| t.format("%H:%M").ok()).map(|s| s.to_string()).unwrap_or(day)
    } else {
        day
    }
}

/// The number keys on the Capture page: the mode with that key.
pub fn mode_for_key(key: gtk::gdk::Key) -> Option<Mode> {
    let n = key.to_unicode()?.to_digit(10)?;
    Mode::ALL.iter().copied().find(|m| m.key() == n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_keys_pick_modes_in_the_bars_order() {
        assert_eq!(mode_for_key(gtk::gdk::Key::_1), Some(Mode::Smart));
        assert_eq!(mode_for_key(gtk::gdk::Key::_5), Some(Mode::Record));
        assert_eq!(mode_for_key(gtk::gdk::Key::_8), Some(Mode::Colour));
        assert_eq!(mode_for_key(gtk::gdk::Key::_9), None);
        assert_eq!(mode_for_key(gtk::gdk::Key::_0), None);
        assert_eq!(mode_for_key(gtk::gdk::Key::a), None);
    }

    #[test]
    fn every_mode_has_its_own_key_and_name() {
        let keys: Vec<u32> = Mode::ALL.iter().map(|m| m.key()).collect();
        assert_eq!(keys, [1, 2, 3, 4, 5, 6, 7, 8]);
        let parsed: Vec<Mode> = ["smart", "window", "screen", "scroll", "record", "text", "qr", "colour"].iter().map(|s| Mode::parse(s)).collect();
        assert_eq!(parsed, Mode::ALL);
        for m in Mode::ALL {
            assert!(!m.label().is_empty() && !m.about().is_empty());
        }
    }
}
