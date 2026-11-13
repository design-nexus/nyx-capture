//! Virtualized history: a list of day headers and rows of cells, so only the rows on
//! screen exist. The rows are rebuilt when the width changes how many cells fit.
//!
//! Ctrl+click and Shift+click select; while anything is selected a plain click selects
//! too. The selection is kept here, by id.

use crate::paths;
use crate::widgets;
use crate::window;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use nyx_capture_core::store::{self, CaptureRow, THUMB_EDGE};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, VecDeque};

const CACHE_LIMIT: usize = 256;
/// The narrowest a cell gets before a column goes.
const CELL_MIN: i32 = 176;
const GAP: i32 = 8;
/// Left and right padding of the list (`.history-grid`).
const PAD: i32 = 24;

#[derive(Default)]
struct TextureCache {
    map: HashMap<String, gdk::Texture>,
    order: VecDeque<String>,
}

impl TextureCache {
    fn get(&mut self, key: &str) -> Option<gdk::Texture> {
        let tex = self.map.get(key)?.clone();
        if let Some(at) = self.order.iter().rposition(|k| k == key) {
            self.order.remove(at);
        }
        self.order.push_back(key.to_string());
        Some(tex)
    }

    fn insert(&mut self, key: String, tex: gdk::Texture) {
        if self.map.insert(key.clone(), tex).is_none() {
            self.order.push_back(key);
        }
        while self.map.len() > CACHE_LIMIT {
            let Some(old) = self.order.pop_front() else { break };
            self.map.remove(&old);
        }
    }
}

/// One row of the list.
#[derive(Clone, Debug)]
pub enum Line {
    Day(String),
    Cells(Vec<CaptureRow>),
}

/// Day headers, each followed by that day's captures `cols` to a row. `day_of` names
/// the day a timestamp falls on; rows come newest first.
pub fn lines(rows: &[CaptureRow], cols: usize, day_of: impl Fn(i64) -> String) -> Vec<Line> {
    let cols = cols.max(1);
    let mut out = Vec::new();
    let mut day: Option<String> = None;
    let mut run: Vec<CaptureRow> = Vec::new();
    for row in rows {
        let d = day_of(row.created_at);
        if day.as_ref() != Some(&d) {
            if !run.is_empty() {
                out.push(Line::Cells(std::mem::take(&mut run)));
            }
            out.push(Line::Day(d.clone()));
            day = Some(d);
        }
        run.push(row.clone());
        if run.len() == cols {
            out.push(Line::Cells(std::mem::take(&mut run)));
        }
    }
    if !run.is_empty() {
        out.push(Line::Cells(run));
    }
    out
}

/// "Today", "Yesterday", a weekday in the last week, then the date.
pub(crate) fn day_label(created: i64) -> String {
    let (Ok(now), Ok(at)) = (glib::DateTime::now_local(), glib::DateTime::from_unix_local(created)) else {
        return String::new();
    };
    let midnight = |d: &glib::DateTime| glib::DateTime::from_local(d.year(), d.month(), d.day_of_month(), 0, 0, 0.0).ok();
    let days = match (midnight(&now), midnight(&at)) {
        (Some(a), Some(b)) => (a.difference(&b).as_seconds() as f64 / 86_400.0).round() as i64,
        _ => i64::MAX,
    };
    let fmt = match days {
        0 => return "Today".into(),
        1 => return "Yesterday".into(),
        2..=6 => "%A",
        _ if at.year() == now.year() => "%A, %-d %B",
        _ => "%-d %B %Y",
    };
    at.format(fmt).map(|s| s.to_string()).unwrap_or_default()
}

struct Grid {
    store: gio::ListStore,
    view: gtk::ListView,
    rows: Vec<CaptureRow>,
    cols: usize,
    cell_height: i32,
    selected: BTreeSet<String>,
    /// Where a Shift+click range starts.
    anchor: Option<String>,
}

thread_local! {
    static TEXTURES: RefCell<TextureCache> = RefCell::default();
    static GRID: RefCell<Option<Grid>> = const { RefCell::new(None) };
    static WIDTH: Cell<i32> = const { Cell::new(0) };
}

fn with<R>(f: impl FnOnce(&mut Grid) -> R) -> Option<R> {
    GRID.with(|g| g.borrow_mut().as_mut().map(f))
}

pub fn build() -> gtk::Widget {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, obj| {
        let item = obj.downcast_ref::<gtk::ListItem>().expect("history line");
        item.set_activatable(false);
        item.set_selectable(false);
        item.set_focusable(false);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, GAP);
        line.set_homogeneous(true);
        item.set_child(Some(&line));
    });
    factory.connect_bind(|_, obj| {
        let item = obj.downcast_ref::<gtk::ListItem>().expect("history line");
        let Some(value) = item.item().and_downcast::<glib::BoxedAnyObject>() else { return };
        let Some(line) = item.child().and_downcast::<gtk::Box>() else { return };
        bind_line(&line, &value.borrow::<Line>());
    });
    factory.connect_unbind(|_, obj| {
        let item = obj.downcast_ref::<gtk::ListItem>().expect("history line");
        let Some(line) = item.child() else { return };
        let mut child = line.first_child();
        while let Some(c) = child {
            if let Some(picture) = cell_picture(&c) {
                picture.set_paintable(None::<&gdk::Paintable>);
            }
            child = c.next_sibling();
        }
    });
    let view = gtk::ListView::new(Some(gtk::NoSelection::new(Some(store.clone()))), Some(factory));
    view.add_css_class("history-grid");
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .hexpand(true)
        .child(&view)
        .build();
    // The width decides the columns.
    let probe = gtk::DrawingArea::new();
    probe.set_can_target(false);
    probe.set_can_focus(false);
    probe.connect_resize(|_, width, _| {
        if WIDTH.with(|w| w.replace(width)) != width {
            glib::idle_add_local_once(relayout);
        }
    });
    let root = gtk::Overlay::new();
    root.set_child(Some(&scroll));
    root.add_overlay(&probe);
    root.set_measure_overlay(&probe, false);
    GRID.with(|g| {
        *g.borrow_mut() = Some(Grid {
            store,
            view,
            rows: Vec::new(),
            cols: 0,
            cell_height: 132,
            selected: BTreeSet::new(),
            anchor: None,
        })
    });
    root.upcast()
}

/// Show `items`, newest first. The selection keeps what's still there.
pub fn fill(items: &[CaptureRow]) {
    with(|g| {
        g.rows = items.to_vec();
        let ids: BTreeSet<&str> = items.iter().map(|r| r.id.as_str()).collect();
        g.selected.retain(|id| ids.contains(id.as_str()));
        g.cols = 0;
    });
    relayout();
}

/// Rebuild the lines if the columns changed (or after `fill`).
fn relayout() {
    let width = WIDTH.with(|w| w.get());
    let rebuilt = with(|g| {
        let inner = (width - PAD).max(CELL_MIN);
        let cols = ((inner + GAP) / (CELL_MIN + GAP)).clamp(2, 8) as usize;
        if cols == g.cols {
            return None;
        }
        g.cols = cols;
        let cell_w = (inner - GAP * (cols as i32 - 1)) / cols as i32;
        g.cell_height = (cell_w * 3 / 4).max(96);
        let objs: Vec<glib::BoxedAnyObject> = lines(&g.rows, cols, day_label).into_iter().map(glib::BoxedAnyObject::new).collect();
        Some((g.store.clone(), objs))
    });
    if let Some(Some((store, objs))) = rebuilt {
        store.splice(0, store.n_items(), &objs);
    }
}

fn bind_line(line: &gtk::Box, value: &Line) {
    let cols = with(|g| g.cols).unwrap_or(1);
    match value {
        Line::Day(title) => {
            line.set_homogeneous(false);
            clear(line);
            let label = widgets::label(title, "history-day");
            label.set_xalign(0.0);
            label.set_hexpand(true);
            line.append(&label);
        }
        Line::Cells(rows) => {
            line.set_homogeneous(true);
            // Reuse the cells already there; a header leaves only its label.
            if line.first_child().is_some_and(|c| c.is::<gtk::Label>()) {
                clear(line);
            }
            let mut have = 0;
            let mut child = line.first_child();
            while let Some(c) = child {
                have += 1;
                child = c.next_sibling();
            }
            while have < cols {
                line.append(&new_cell());
                have += 1;
            }
            while have > cols {
                if let Some(last) = line.last_child() {
                    line.remove(&last);
                }
                have -= 1;
            }
            let mut child = line.first_child();
            let mut i = 0;
            while let Some(c) = child {
                if let Ok(cell) = c.clone().downcast::<gtk::Overlay>() {
                    bind_cell(&cell, rows.get(i));
                }
                i += 1;
                child = c.next_sibling();
            }
        }
    }
}

fn clear(line: &gtk::Box) {
    while let Some(c) = line.first_child() {
        line.remove(&c);
    }
}

fn new_cell() -> gtk::Overlay {
    // The base sets the size; the picture fills it whatever the shot's shape.
    let base = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let picture = gtk::Picture::new();
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_can_shrink(true);
    picture.add_css_class("history-picture");

    let check = gtk::Image::from_icon_name("object-select-symbolic");
    check.add_css_class("history-check");
    check.set_halign(gtk::Align::Start);
    check.set_valign(gtk::Align::Start);
    check.set_margin_top(8);
    check.set_margin_start(8);
    check.set_can_target(false);

    let star = widgets::icon_button("non-starred-symbolic", "Star");
    star.add_css_class("history-star");
    star.set_halign(gtk::Align::End);
    star.set_valign(gtk::Align::Start);
    star.set_margin_top(4);
    star.set_margin_end(4);
    let steal = gtk::GestureClick::new();
    steal.set_button(1);
    steal.set_propagation_phase(gtk::PropagationPhase::Capture);
    steal.connect_released(|g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        let Some(b) = g.widget().and_downcast::<gtk::Button>() else { return };
        let Some(root) = b.ancestor(gtk::Overlay::static_type()) else { return };
        let id = root.widget_name().to_string();
        if id.is_empty() {
            return;
        }
        let on = b.icon_name().as_deref() != Some("starred-symbolic");
        window::set_starred(&id, on);
    });
    star.add_controller(steal);

    let cell = gtk::Overlay::new();
    cell.add_css_class("history-cell");
    cell.set_child(Some(&base));
    cell.add_overlay(&picture);
    cell.set_measure_overlay(&picture, false);
    cell.add_overlay(&check);
    cell.add_overlay(&star);
    cell.set_focusable(true);

    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.connect_released(|g, n, _, _| {
        let Some(w) = g.widget() else { return };
        let id = w.widget_name().to_string();
        if id.is_empty() || n != 1 {
            return;
        }
        let mods = g.current_event_state();
        if mods.contains(gdk::ModifierType::SHIFT_MASK) {
            select_range(&id);
        } else if mods.contains(gdk::ModifierType::CONTROL_MASK) || has_selection() {
            toggle(&id);
        } else {
            open(&id);
        }
    });
    cell.add_controller(click);

    let right = gtk::GestureClick::new();
    right.set_button(3);
    right.connect_pressed(|g, _, x, y| {
        let Some(w) = g.widget() else { return };
        let id = w.widget_name().to_string();
        if id.is_empty() {
            return;
        }
        window::item_menu(&id, &w, x, y);
    });
    cell.add_controller(right);

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(|k, key, _, mods| {
        let Some(w) = k.widget() else { return glib::Propagation::Proceed };
        let id = w.widget_name().to_string();
        if id.is_empty() {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::Return | gdk::Key::KP_Enter if !has_selection() => open(&id),
            gdk::Key::space | gdk::Key::Return | gdk::Key::KP_Enter => {
                if mods.contains(gdk::ModifierType::SHIFT_MASK) {
                    select_range(&id)
                } else {
                    toggle(&id)
                }
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    cell.add_controller(keys);
    cell
}

fn cell_picture(cell: &gtk::Widget) -> Option<gtk::Picture> {
    cell_part(cell)
}

fn cell_part<T: IsA<gtk::Widget>>(cell: &gtk::Widget) -> Option<T> {
    let mut child = cell.first_child();
    while let Some(next) = child {
        if let Ok(w) = next.clone().downcast::<T>() {
            return Some(w);
        }
        child = next.next_sibling();
    }
    None
}

/// Fill a cell with `row`, or leave it as an empty space at the end of a short row.
fn bind_cell(cell: &gtk::Overlay, row: Option<&CaptureRow>) {
    let (height, selected) = with(|g| (g.cell_height, row.is_some_and(|r| g.selected.contains(&r.id)))).unwrap_or((132, false));
    if let Some(base) = cell.child() {
        base.set_size_request(-1, height);
    }
    let Some(row) = row else {
        cell.set_widget_name("");
        cell.set_tooltip_text(None);
        cell.set_opacity(0.0);
        cell.set_can_target(false);
        cell.set_focusable(false);
        if let Some(picture) = cell_picture(cell.upcast_ref()) {
            picture.set_paintable(None::<&gdk::Paintable>);
        }
        return;
    };
    cell.set_opacity(1.0);
    cell.set_can_target(true);
    cell.set_focusable(true);
    cell.set_widget_name(&row.id);
    cell.set_tooltip_text(Some(&tooltip(row)));
    mark_selected(cell.upcast_ref(), selected);
    if let Some(star) = cell_part::<gtk::Button>(cell.upcast_ref()) {
        let on = row.starred;
        star.set_icon_name(if on { "starred-symbolic" } else { "non-starred-symbolic" });
        star.set_tooltip_text(Some(if on { "Unstar" } else { "Star" }));
    }
    if let Some(picture) = cell_picture(cell.upcast_ref()) {
        fill_picture(&picture, &row.id, &row.path);
    }
}

fn mark_selected(cell: &gtk::Widget, on: bool) {
    if on {
        cell.add_css_class("selected");
    } else {
        cell.remove_css_class("selected");
    }
    if let Some(check) = cell_part::<gtk::Image>(cell) {
        check.set_visible(on);
    }
}

fn tooltip(row: &CaptureRow) -> String {
    let size = (row.width > 0 && row.height > 0).then(|| format!("{} × {}", row.width, row.height));
    match (row.title.trim(), size) {
        ("", Some(size)) => size,
        ("", None) => "Capture".into(),
        (title, Some(size)) => format!("{title}\n{size}"),
        (title, None) => title.into(),
    }
}

fn open(id: &str) {
    let row = with(|g| g.rows.iter().find(|r| r.id == id).cloned()).flatten();
    if let Some(row) = row {
        window::open_capture(row);
    }
}

// ----- Selection -----

pub fn has_selection() -> bool {
    with(|g| !g.selected.is_empty()).unwrap_or(false)
}

/// The selected ids, newest first.
pub fn selected() -> Vec<String> {
    with(|g| g.rows.iter().filter(|r| g.selected.contains(&r.id)).map(|r| r.id.clone()).collect()).unwrap_or_default()
}

pub fn is_selected(id: &str) -> bool {
    with(|g| g.selected.contains(id)).unwrap_or(false)
}

fn toggle(id: &str) {
    with(|g| {
        if !g.selected.remove(id) {
            g.selected.insert(id.to_string());
        }
        g.anchor = Some(id.to_string());
    });
    selection_changed();
}

fn select_range(id: &str) {
    with(|g| {
        let at = |x: &str| g.rows.iter().position(|r| r.id == x);
        let to = at(id);
        let from = g.anchor.as_deref().and_then(at).or(to);
        if let (Some(a), Some(b)) = (from, to) {
            let (a, b) = (a.min(b), a.max(b));
            let ids: Vec<String> = g.rows[a..=b].iter().map(|r| r.id.clone()).collect();
            g.selected.extend(ids);
        }
        g.anchor.get_or_insert_with(|| id.to_string());
    });
    selection_changed();
}

pub fn select_all() {
    with(|g| g.selected = g.rows.iter().map(|r| r.id.clone()).collect());
    selection_changed();
}

pub fn clear_selection() {
    let had = with(|g| {
        g.anchor = None;
        !std::mem::take(&mut g.selected).is_empty()
    });
    if had == Some(true) {
        selection_changed();
    }
}

/// Restyle the cells on screen and tell the window.
fn selection_changed() {
    let Some(view) = with(|g| g.view.clone()) else { return };
    fn walk(w: &gtk::Widget) {
        if w.has_css_class("history-cell") {
            let id = w.widget_name();
            if !id.is_empty() {
                mark_selected(w, is_selected(&id));
            }
            return;
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            walk(&c);
            child = c.next_sibling();
        }
    }
    walk(view.upcast_ref());
    window::selection_changed();
}

// ----- Thumbnails -----

/// A capture's thumbnail in `picture`, whose parent is named after the capture.
pub(crate) fn fill_picture(picture: &gtk::Picture, id: &str, path: &std::path::Path) {
    if let Some(tex) = TEXTURES.with(|c| c.borrow_mut().get(id)) {
        picture.set_paintable(Some(&tex));
        return;
    }
    picture.set_paintable(None::<&gdk::Paintable>);
    let picture = picture.clone();
    let id = id.to_string();
    let src = path.to_path_buf();
    let dest = paths::thumb_file(&id);
    crate::cmd::background(
        // Through the library, so an edited shot's thumbnail shows its edits (and its
        // redaction) and is recorded for deletion.
        move || {
            match store::open(&paths::library_db()) {
                Ok(db) => db.thumb(&id, &src, &dest, false)?,
                Err(_) => store::write_thumb(&src, &dest, THUMB_EDGE)?,
            }
            anyhow::Ok((id, dest))
        },
        move |result| {
            let Ok((id, dest)) = result else { return };
            let Ok(tex) = gdk::Texture::from_filename(&dest) else { return };
            TEXTURES.with(|c| c.borrow_mut().insert(id.clone(), tex.clone()));
            // The cell may have moved on to another capture meanwhile.
            if picture.parent().is_some_and(|p| p.widget_name() == id.as_str()) {
                picture.set_paintable(Some(&tex));
            }
        },
    );
}

pub fn forget_thumb(id: &str) {
    drop_texture(id);
    let _ = std::fs::remove_file(paths::thumb_file(id));
}

/// Forget the loaded texture only, so the grid reads the (new) file again.
pub fn drop_texture(id: &str) {
    TEXTURES.with(|c| {
        let mut cache = c.borrow_mut();
        cache.map.remove(id);
        cache.order.retain(|k| k != id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_capture_core::store::CaptureKind;

    fn row(id: &str, created_at: i64) -> CaptureRow {
        CaptureRow {
            id: id.into(),
            kind: CaptureKind::Screenshot,
            created_at,
            width: 0,
            height: 0,
            path: Default::default(),
            starred: false,
            ocr_status: String::new(),
            title: String::new(),
            sensitive: false,
        }
    }

    fn ids(line: &Line) -> String {
        match line {
            Line::Day(d) => format!("[{d}]"),
            Line::Cells(rows) => rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>().join(","),
        }
    }

    #[test]
    fn groups_days_into_rows() {
        let day = 86_400;
        let rows = [row("a", 3 * day + 5), row("b", 3 * day + 4), row("c", 3 * day + 3), row("d", 2 * day), row("e", day)];
        let out: Vec<String> = lines(&rows, 2, |t| (t / day).to_string()).iter().map(ids).collect();
        assert_eq!(out, ["[3]", "a,b", "c", "[2]", "d", "[1]", "e"]);
        assert!(lines(&[], 4, |t| t.to_string()).is_empty());
    }
}
