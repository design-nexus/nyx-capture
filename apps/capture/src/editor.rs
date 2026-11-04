//! Annotation editor: marks on the still, copy flattened PNG, original file unchanged.

use crate::paths;
use crate::prefs;
use crate::widgets;
use gtk::cairo;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use image::RgbaImage;
use nyx_capture_core::annot::{self, ArrowStyle, Axis, Colour, EditState, History, Mark, NormRect, SpotShape};
use nyx_capture_core::capture::Shot;
use nyx_capture_core::export;
use nyx_capture_core::ocr::{OcrMode, recognize};
use nyx_capture_core::store;
use nyx_capture_core::clipboard;
use nyx_ui::chrome;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

const WORK_EDGE: u32 = 2048;
const PAD: f64 = 16.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Arrow,
    Line,
    Rect,
    Ellipse,
    Pen,
    Highlighter,
    Text,
    Callout,
    Blur,
    Pixelate,
    Fill,
    Crop,
    Spotlight,
    /// Drag across a band to take it out and close the gap.
    Cut,
    /// Click to read the whole picture's text, drag to read an area's.
    Ocr,
    /// One click takes the picture's colour, then the tool before it comes back.
    Eyedropper,
}

/// Each tool: its icon, name, and key (pressed on its own, outside a text field).
const TOOLS: &[(Tool, &str, &str, gdk::Key)] = &[
    (Tool::Select, "object-select-symbolic", "Select (V)", gdk::Key::v),
    (Tool::Arrow, "capture-tool-arrow-symbolic", "Arrow (A)", gdk::Key::a),
    (Tool::Line, "capture-tool-line-symbolic", "Line (L)", gdk::Key::l),
    (Tool::Rect, "capture-tool-rect-symbolic", "Rectangle (R)", gdk::Key::r),
    (Tool::Ellipse, "capture-tool-ellipse-symbolic", "Ellipse (E)", gdk::Key::e),
    (Tool::Pen, "document-edit-symbolic", "Freehand (P)", gdk::Key::p),
    (Tool::Highlighter, "capture-tool-highlighter-symbolic", "Highlighter (H)", gdk::Key::h),
    (Tool::Text, "insert-text-symbolic", "Text (T)", gdk::Key::t),
    (Tool::Callout, "capture-tool-callout-symbolic", "Numbered step (N)", gdk::Key::n),
    (Tool::Spotlight, "capture-tool-spotlight-symbolic", "Spotlight (S)", gdk::Key::s),
    (Tool::Blur, "view-conceal-symbolic", "Blur (B)", gdk::Key::b),
    (Tool::Pixelate, "view-grid-symbolic", "Pixelate (X)", gdk::Key::x),
    (Tool::Fill, "media-playback-stop-symbolic", "Solid redaction (D)", gdk::Key::d),
    (Tool::Cut, "capture-tool-cut-symbolic", "Cut out a band (K)", gdk::Key::k),
    (Tool::Crop, "zoom-fit-best-symbolic", "Crop (C)", gdk::Key::c),
    (Tool::Ocr, "capture-text-symbolic", "Read text (O)", gdk::Key::o),
];

enum Draft {
    Stroke(Vec<(f64, f64)>),
    Shape { from: (f64, f64), to: (f64, f64) },
    /// Moving the selected mark: where the drag began, and where it is now (view space).
    Move { from: (f64, f64), to: (f64, f64) },
}

struct Session {
    original: RgbaImage,
    preview_src: RgbaImage,
    work: RgbaImage,
    /// `work` as a cairo surface, made when `work` changes rather than every frame.
    surface: Option<cairo::ImageSurface>,
    /// The mark the Select tool picked (an index into the current state's marks).
    selected: Option<usize>,
    /// Canvas zoom (1 fits the window) and pan in canvas pixels.
    zoom: f64,
    pan: (f64, f64),
    history: History,
    tool: Tool,
    /// The tool the eyedropper goes back to.
    before_eyedropper: Tool,
    colour: Colour,
    size: f64,
    /// A again cycles it; S again cycles the spotlight's shape.
    arrow_style: ArrowStyle,
    spot_shape: SpotShape,
    /// Rectangles and ellipses filled, and rectangles' corner radius (slider units).
    fill: bool,
    corners: f64,
    draft: Option<Draft>,
    /// The picture on the canvas, without its backdrop: pointer events map to this.
    image_rect: (f64, f64, f64, f64),
    /// The backdrop's mat around the picture in `work`, in its pixels (0 without one).
    inset: f64,
    path: Option<PathBuf>,
    id: Option<String>,
}

/// The canvas picture for `st`: rendered small, on its backdrop; and the backdrop's
/// mat in those pixels.
fn work_image(preview_src: &RgbaImage, st: &EditState) -> (RgbaImage, f64) {
    let img = annot::render(preview_src, st);
    match &st.backdrop {
        Some(b) => (annot::backdrop::compose(&img, b), b.inset(img.width(), img.height()) as f64),
        None => (img, 0.0),
    }
}

impl Session {
    fn new(original: RgbaImage, history: History, path: Option<PathBuf>, id: Option<String>) -> Session {
        let preview_src = annot::preview(&original, WORK_EDGE);
        let (work, inset) = work_image(&preview_src, history.current());
        Session {
            original,
            preview_src,
            surface: rgba_surface(&work),
            work,
            inset,
            selected: None,
            zoom: 1.0,
            pan: (0.0, 0.0),
            history,
            tool: Tool::Arrow,
            before_eyedropper: Tool::Arrow,
            colour: theme_swatches().into_iter().nth(2).unwrap_or(Colour::rgb(0.4, 0.6, 1.0)),
            size: 6.0,
            arrow_style: ArrowStyle::Standard,
            spot_shape: SpotShape::Ellipse,
            fill: false,
            corners: 0.0,
            draft: None,
            image_rect: (0.0, 0.0, 1.0, 1.0),
            path,
            id,
        }
    }
}

/// The Backdrop popover's controls, kept in step with the current state.
struct BackdropUi {
    button: gtk::MenuButton,
    off: gtk::ToggleButton,
    fills: Vec<(annot::BackdropFill, gtk::ToggleButton)>,
    padding: gtk::Scale,
    corners: gtk::Scale,
    shadow: gtk::CheckButton,
    /// Set while the controls are being set from the state, so they don't commit.
    syncing: std::cell::Cell<bool>,
    /// When a backdrop change last went into history: changes soon after amend it.
    last: std::cell::Cell<Option<std::time::Instant>>,
}

impl BackdropUi {
    fn build() -> (BackdropUi, gtk::Popover) {
        let pop = gtk::Popover::new();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
        body.set_margin_top(10);
        body.set_margin_bottom(10);
        body.set_margin_start(10);
        body.set_margin_end(10);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let off = gtk::ToggleButton::with_label("Off");
        off.add_css_class("flat");
        off.set_tooltip_text(Some("No backdrop"));
        row.append(&off);
        let mut fills = Vec::new();
        for fill in annot::BackdropFill::ALL {
            let b = gtk::ToggleButton::new();
            b.add_css_class("flat");
            b.add_css_class("swatch-button");
            b.set_tooltip_text(Some(fill.name()));
            b.set_group(Some(&off));
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.add_css_class("swatch-dot");
            let css = gtk::CssProvider::new();
            css.load_from_string(&format!("* {{ background: linear-gradient(135deg, {}); }}", fill.stops().join(", ")));
            #[allow(deprecated)]
            dot.style_context().add_provider(&css, gtk::STYLE_PROVIDER_PRIORITY_USER);
            b.set_child(Some(&dot));
            row.append(&b);
            fills.push((fill, b));
        }
        body.append(&row);
        let slider = |label: &str, max: f64, step: f64| {
            let s = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, max, step);
            s.set_draw_value(false);
            s.set_hexpand(true);
            s.set_size_request(160, -1);
            let r = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let l = gtk::Label::new(Some(label));
            l.add_css_class("dim");
            l.set_width_chars(8);
            l.set_xalign(0.0);
            r.append(&l);
            r.append(&s);
            body.append(&r);
            s
        };
        let padding = slider("Padding", 0.15, 0.005);
        let corners = slider("Corners", 0.05, 0.001);
        let shadow = gtk::CheckButton::with_label("Shadow");
        body.append(&shadow);
        pop.set_child(Some(&body));
        let button = gtk::MenuButton::new();
        button.set_icon_name("capture-backdrop-symbolic");
        button.set_tooltip_text(Some("Backdrop (Shift+B)"));
        button.add_css_class("flat");
        button.set_valign(gtk::Align::Center);
        button.set_popover(Some(&pop));
        let ui = BackdropUi {
            button,
            off,
            fills,
            padding,
            corners,
            shadow,
            syncing: std::cell::Cell::new(false),
            last: std::cell::Cell::new(None),
        };
        (ui, pop)
    }

    /// The controls' backdrop: None when Off is chosen.
    fn value(&self) -> Option<annot::Backdrop> {
        let fill = self.fills.iter().find(|(_, b)| b.is_active())?.0;
        Some(annot::Backdrop { fill, padding: self.padding.value(), radius: self.corners.value(), shadow: self.shadow.is_active() })
    }

    /// Show `b` (the state's backdrop) on the controls.
    fn show(&self, b: Option<annot::Backdrop>) {
        self.syncing.set(true);
        let shown = b.unwrap_or_default();
        match b {
            Some(b) => {
                if let Some((_, button)) = self.fills.iter().find(|(f, _)| *f == b.fill) {
                    button.set_active(true);
                }
            }
            None => self.off.set_active(true),
        }
        self.padding.set_value(shown.padding);
        self.corners.set_value(shown.radius);
        self.shadow.set_active(shown.shadow);
        for w in [self.padding.upcast_ref::<gtk::Widget>(), self.corners.upcast_ref(), self.shadow.upcast_ref()] {
            w.set_sensitive(b.is_some());
        }
        self.syncing.set(false);
    }
}

struct Inner {
    window: gtk::ApplicationWindow,
    header: chrome::AppHeader,
    canvas: gtk::DrawingArea,
    session: RefCell<Option<Session>>,
    tool_buttons: Vec<(Tool, gtk::ToggleButton)>,
    toast_host: gtk::Overlay,
    /// The custom colour's dot shows the colour picked last, through this stylesheet.
    custom_css: gtk::CssProvider,
    style_pop: gtk::Popover,
    backdrop: BackdropUi,
}

#[derive(Clone)]
struct Editor(Rc<Inner>);

pub fn open(shot: Shot) {
    let Some(app) = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) else {
        return;
    };
    Editor::show(&app).load_shot(shot);
}

/// An image that isn't a capture (a file, the clipboard): edited and copied or saved
/// like one, but not put into history, and nothing written beside it.
pub fn open_image(img: RgbaImage, title: &str) {
    let Some(app) = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) else {
        return;
    };
    Editor::show(&app).load_image(img, title);
}

pub fn open_saved(img: RgbaImage, state: annot::EditState, id: String, path: PathBuf) {
    let Some(app) = gio::Application::default().and_then(|a| a.downcast::<gtk::Application>().ok()) else {
        return;
    };
    Editor::show(&app).load_saved(img, state, id, path);
}

impl Editor {
    fn show(app: &gtk::Application) -> Editor {
        let window = gtk::ApplicationWindow::builder().application(app).title("Capture").default_width(960).default_height(720).build();
        window.add_css_class("capture-window");
        window.add_css_class("capture-editor");
        window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));

        let canvas = gtk::DrawingArea::new();
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        canvas.set_cursor_from_name(Some("crosshair"));

        // Tools wrap onto a second row when the window is narrow (a half-screen tile).
        let tools = gtk::FlowBox::new();
        tools.add_css_class("editor-strip");
        tools.set_hexpand(true);
        tools.set_selection_mode(gtk::SelectionMode::None);
        tools.set_max_children_per_line(TOOLS.len() as u32);
        tools.set_min_children_per_line(6);
        tools.set_column_spacing(2);
        tools.set_row_spacing(2);
        tools.set_homogeneous(true);
        let mut tool_buttons = Vec::new();
        let mut first: Option<gtk::ToggleButton> = None;
        for (tool, icon, tip, _) in TOOLS {
            let b = gtk::ToggleButton::new();
            b.set_icon_name(icon);
            b.set_tooltip_text(Some(tip));
            b.add_css_class("flat");
            b.add_css_class("icon-button");
            b.add_css_class("editor-tool");
            match &first {
                Some(f) => b.set_group(Some(f)),
                None => first = Some(b.clone()),
            }
            tools.append(&b);
            tool_buttons.push((*tool, b));
        }
        tool_buttons[1].1.set_active(true);

        let swatches = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        for c in theme_swatches() {
            let b = gtk::Button::new();
            b.add_css_class("swatch-button");
            b.set_tooltip_text(Some(&c.css()));
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.add_css_class("swatch-dot");
            widgets::paint(&dot, &c.css());
            b.set_child(Some(&dot));
            swatches.append(&b);
            // Wired once the editor exists, by this name.
            b.set_widget_name(&c.css());
        }
        // A colour of your own: chosen in a dialog, or taken from the picture.
        let custom = gtk::Button::new();
        custom.add_css_class("swatch-button");
        custom.set_tooltip_text(Some("Custom colour"));
        let custom_dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        custom_dot.add_css_class("swatch-dot");
        custom_dot.add_css_class("swatch-custom");
        let custom_css = gtk::CssProvider::new();
        #[allow(deprecated)]
        custom_dot.style_context().add_provider(&custom_css, gtk::STYLE_PROVIDER_PRIORITY_USER);
        custom.set_child(Some(&custom_dot));
        let eyedropper = gtk::Button::from_icon_name("capture-mode-colour-symbolic");
        eyedropper.add_css_class("flat");
        eyedropper.add_css_class("swatch-button");
        eyedropper.set_tooltip_text(Some("Pick a colour from the image (I)"));
        let own = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        own.append(&custom);
        own.append(&eyedropper);

        let size = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 20.0, 1.0);
        size.set_value(6.0);
        size.set_draw_value(false);
        size.set_size_request(160, -1);
        size.set_tooltip_text(Some("Size"));
        // Rectangles and ellipses: filled or outlined, and rectangles' corners.
        let fill = gtk::CheckButton::with_label("Filled shapes");
        let corners = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 20.0, 1.0);
        corners.set_draw_value(false);
        corners.set_tooltip_text(Some("Corner radius"));
        let corners_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let corners_label = gtk::Label::new(Some("Corners"));
        corners_label.add_css_class("dim");
        corners.set_hexpand(true);
        corners_row.append(&corners_label);
        corners_row.append(&corners);
        // Colour and size share one button, so the strip fits an 800 px window.
        let style_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        style_box.set_margin_top(8);
        style_box.set_margin_bottom(8);
        style_box.set_margin_start(8);
        style_box.set_margin_end(8);
        style_box.append(&swatches);
        style_box.append(&own);
        style_box.append(&size);
        style_box.append(&fill);
        style_box.append(&corners_row);
        let style_pop = gtk::Popover::new();
        style_pop.set_child(Some(&style_box));
        let style = gtk::MenuButton::new();
        style.set_icon_name("applications-graphics-symbolic");
        style.set_tooltip_text(Some("Colour and size"));
        style.add_css_class("flat");
        style.set_valign(gtk::Align::Center);
        style.set_popover(Some(&style_pop));

        let undo = widgets::bar_button("edit-undo-symbolic", "Undo (Ctrl+Z)");
        let redo = widgets::bar_button("edit-redo-symbolic", "Redo (Ctrl+Shift+Z)");
        let copy = widgets::bar_button("edit-copy-symbolic", "Copy (Ctrl+C)");
        let save = widgets::bar_button("document-save-symbolic", "Save (Ctrl+S)");
        let share = widgets::bar_button("send-to-symbolic", "Share");
        let pin = widgets::bar_button("view-pin-symbolic", "Pin to the screen");
        let redact = widgets::bar_button("channel-secure-symbolic", "Redact sensitive text");

        // The app and the shot; the edit actions, then Save and Copy, at the end.
        let header = chrome::app_header(&window, "capture-region-symbolic", "Capture");
        header.end.set_spacing(4);
        for b in [undo.upcast_ref::<gtk::Widget>(), redo.upcast_ref(), redact.upcast_ref(), pin.upcast_ref(), share.upcast_ref(), save.upcast_ref(), copy.upcast_ref()] {
            header.end.append(b);
        }

        // The tools and their colour and size, centred under the shot.
        let (backdrop, backdrop_pop) = BackdropUi::build();
        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        strip.add_css_class("editor-bar");
        strip.set_halign(gtk::Align::Center);
        strip.append(&tools);
        strip.append(&style);
        strip.append(&backdrop.button);

        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.add_css_class("window-frame");
        frame.append(&header.root);
        frame.append(&canvas);
        frame.append(&strip);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&frame));
        window.set_child(Some(&overlay));

        let editor = Editor(Rc::new(Inner {
            window: window.clone(),
            header,
            canvas: canvas.clone(),
            session: RefCell::new(None),
            tool_buttons,
            toast_host: overlay,
            custom_css,
            style_pop,
            backdrop,
        }));
        // The popover shows the state as it is now (after an undo, say), and its
        // changes go into history.
        let me = editor.clone();
        backdrop_pop.connect_show(move |_| {
            let b = me.0.session.borrow().as_ref().and_then(|s| s.history.current().backdrop);
            me.0.backdrop.show(b);
        });
        let ui = &editor.0.backdrop;
        for b in std::iter::once(&ui.off).chain(ui.fills.iter().map(|(_, b)| b)) {
            let me = editor.clone();
            b.connect_toggled(move |b| {
                if b.is_active() {
                    me.backdrop_changed();
                }
            });
        }
        for s in [&ui.padding, &ui.corners] {
            let me = editor.clone();
            s.connect_value_changed(move |_| me.backdrop_changed());
        }
        let me = editor.clone();
        ui.shadow.connect_toggled(move |_| me.backdrop_changed());

        let me = editor.clone();
        canvas.set_draw_func(move |_, cr, w, h| me.draw(cr, w as f64, h as f64));
        editor.wire_pointer();
        editor.wire_keys();
        editor.wire_zoom();

        for (tool, btn) in &editor.0.tool_buttons {
            let me = editor.clone();
            let tool = *tool;
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    me.set_tool(tool);
                }
            });
        }
        // Swatch buttons are children of `swatches`; connect by walking strip.
        let me = editor.clone();
        walk_swatches(&swatches, move |c| me.set_colour(c));

        let me = editor.clone();
        size.connect_value_changed(move |s| {
            if let Some(sess) = me.0.session.borrow_mut().as_mut() {
                sess.size = s.value();
            }
        });
        let me = editor.clone();
        fill.connect_toggled(move |b| {
            if let Some(sess) = me.0.session.borrow_mut().as_mut() {
                sess.fill = b.is_active();
            }
        });
        let me = editor.clone();
        corners.connect_value_changed(move |s| {
            if let Some(sess) = me.0.session.borrow_mut().as_mut() {
                sess.corners = s.value();
            }
        });
        let me = editor.clone();
        undo.connect_clicked(move |_| me.undo());
        let me = editor.clone();
        redo.connect_clicked(move |_| me.redo());
        let me = editor.clone();
        copy.connect_clicked(move |_| me.copy_flat());
        let me = editor.clone();
        save.connect_clicked(move |_| me.save_flat());
        let me = editor.clone();
        share.connect_clicked(move |_| me.share_flat());
        let me = editor.clone();
        pin.connect_clicked(move |_| me.pin());
        let me = editor.clone();
        custom.connect_clicked(move |_| me.choose_colour());
        let me = editor.clone();
        eyedropper.connect_clicked(move |_| me.start_eyedropper());
        let me = editor.clone();
        redact.connect_clicked(move |_| me.redact());

        window.connect_close_request(|_| {
            crate::window::editor_closed();
            glib::Propagation::Proceed
        });
        window.present();
        editor
    }

    fn load_image(&self, original: RgbaImage, title: &str) {
        self.0.header.set_subtitle(title);
        #[allow(unused_mut)]
        let mut history = History::default();
        // Debug builds: `CAPTURE_TEST_EDIT_STATE=<json>` opens with those edits, for layout checks.
        #[cfg(debug_assertions)]
        if let Some(st) = std::env::var("CAPTURE_TEST_EDIT_STATE").ok().and_then(|p| EditState::load(std::path::Path::new(&p)).ok()) {
            history = History::from_state(st);
        }
        self.0.session.replace(Some(Session::new(original, history, None, None)));
        self.0.canvas.queue_draw();
    }

    fn load_shot(&self, shot: Shot) {
        let original = shot.image.to_rgba8();
        let png = shot.png().unwrap_or_default();
        let (w, h) = (original.width() as i32, original.height() as i32);
        let persist = prefs::get().history;
        let title = crate::window::capture_title();
        self.0.header.set_subtitle(if title.is_empty() { "New shot" } else { &title });
        self.0.session.replace(Some(Session::new(original, History::default(), None, None)));
        if persist && !png.is_empty() {
            let me = self.clone();
            crate::cmd::background(
                move || {
                    let db = store::open(&paths::library_db())?;
                    let (id, path) = db.insert_png(&paths::captures_dir(), &png, w, h, "grim")?;
                    let _ = db.set_title(&id, &title);
                    let _ = db.thumb(&id, &path, &paths::thumb_file(&id), false);
                    anyhow::Ok((id, path))
                },
                move |result| match result {
                    Ok((id, path)) => {
                        if let Some(s) = me.0.session.borrow_mut().as_mut() {
                            s.id = Some(id);
                            s.path = Some(path);
                        }
                        // Marks made before the row existed go into its sidecar now.
                        me.write_sidecar();
                        crate::window::reload();
                        me.after_insert();
                    }
                    Err(err) => me.toast(&err.to_string()),
                },
            );
        } else if prefs::get().auto_redact {
            self.redact();
        }
        self.0.canvas.queue_draw();
    }

    /// Once the shot is in history (it has an id): redact it if that's on, which also
    /// marks it sensitive so its text isn't indexed; otherwise read its text if that's on.
    fn after_insert(&self) {
        let p = prefs::get();
        if p.auto_redact {
            self.redact();
            return;
        }
        if !p.ocr_on_capture {
            return;
        }
        let Some((original, id)) = self.0.session.borrow().as_ref().and_then(|s| Some((s.original.clone(), s.id.clone()?))) else { return };
        let mode = OcrMode::parse(&p.ocr_mode);
        crate::cmd::background(
            move || {
                let r = recognize(&image::DynamicImage::ImageRgba8(original), mode, &p.ocr_langs)?;
                store::open(&paths::library_db())?.set_ocr(&id, &r.engine, &r.text)
            },
            |_| crate::window::reload(),
        );
    }

    fn load_saved(&self, original: RgbaImage, state: EditState, id: String, path: PathBuf) {
        // An older sidecar's marks are moved onto the whole picture before editing.
        let state = state.upgraded(original.width() as f64, original.height() as f64);
        let title = store::open(&paths::library_db())
            .ok()
            .and_then(|db| db.get(&id).ok().flatten())
            .map(|row| row.title)
            .filter(|t| !t.is_empty())
            .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_default();
        self.0.header.set_subtitle(&title);
        self.0.session.replace(Some(Session::new(original, History::from_state(state), Some(path), Some(id))));
        self.0.canvas.queue_draw();
    }

    fn set_tool(&self, tool: Tool) {
        if let Some(s) = self.0.session.borrow_mut().as_mut() {
            s.tool = tool;
            s.draft = None;
            if tool != Tool::Select {
                s.selected = None;
            }
        }
        let cursor = match tool {
            Tool::Text => "text",
            Tool::Select => "default",
            Tool::Eyedropper => "cell",
            _ => "crosshair",
        };
        self.0.canvas.set_cursor_from_name(Some(cursor));
        self.0.canvas.queue_draw();
    }

    fn set_colour(&self, colour: Colour) {
        if let Some(s) = self.0.session.borrow_mut().as_mut() {
            s.colour = colour;
        }
    }

    /// A custom colour: the custom swatch shows it from then on.
    fn set_custom_colour(&self, colour: Colour) {
        self.set_colour(colour);
        self.0.custom_css.load_from_string(&format!("* {{ background: {}; }}", colour.css()));
    }

    fn choose_colour(&self) {
        self.0.style_pop.popdown();
        let current = self.0.session.borrow().as_ref().map(|s| s.colour).unwrap_or(Colour::rgb(1.0, 0.0, 0.0));
        let initial = gdk::RGBA::new(current.r as f32, current.g as f32, current.b as f32, 1.0);
        let dialog = gtk::ColorDialog::builder().title("Custom colour").with_alpha(false).modal(true).build();
        let me = self.clone();
        dialog.choose_rgba(Some(&self.0.window), Some(&initial), gio::Cancellable::NONE, move |result| {
            if let Ok(c) = result {
                me.set_custom_colour(Colour::rgb(c.red() as f64, c.green() as f64, c.blue() as f64));
            }
        });
    }

    /// The Backdrop popover changed: into history, a slider's run of changes as one step.
    fn backdrop_changed(&self) {
        let ui = &self.0.backdrop;
        if ui.syncing.get() {
            return;
        }
        let value = ui.value();
        for w in [ui.padding.upcast_ref::<gtk::Widget>(), ui.corners.upcast_ref(), ui.shadow.upcast_ref()] {
            w.set_sensitive(value.is_some());
        }
        let now = std::time::Instant::now();
        let amend = ui.last.get().is_some_and(|t| now.duration_since(t) < std::time::Duration::from_millis(700));
        ui.last.set(Some(now));
        {
            let mut slot = self.0.session.borrow_mut();
            let Some(s) = slot.as_mut() else { return };
            let mut st = s.history.current().clone();
            if st.backdrop == value {
                return;
            }
            st.backdrop = value;
            if amend {
                s.history.amend(st);
            } else {
                s.history.push(st);
            }
            rerender(s);
        }
        self.0.canvas.queue_draw();
        self.write_sidecar();
    }

    /// The Arrow's or Spotlight's next style, named on its button and in a toast.
    fn next_style(&self, tool: Tool) {
        let name = {
            let mut slot = self.0.session.borrow_mut();
            let Some(s) = slot.as_mut() else { return };
            match tool {
                Tool::Arrow => {
                    s.arrow_style = s.arrow_style.next();
                    format!("Arrow: {}", s.arrow_style.name())
                }
                Tool::Spotlight => {
                    s.spot_shape = s.spot_shape.next();
                    format!("Spotlight: {}", s.spot_shape.name())
                }
                _ => return,
            }
        };
        let key = if tool == Tool::Arrow { "A" } else { "S" };
        if let Some((_, b)) = self.0.tool_buttons.iter().find(|(t, _)| *t == tool) {
            b.set_tooltip_text(Some(&format!("{name} ({key})")));
        }
        self.toast(&name);
    }

    /// The wheel over the selected spotlight: magnify inside it (a loupe), or less.
    fn zoom_spotlight(&self, up: bool) -> bool {
        let Some(i) = self.0.session.borrow().as_ref().and_then(|s| s.selected) else { return false };
        let is_spot = self.0.session.borrow().as_ref().is_some_and(|s| matches!(s.history.current().marks.get(i), Some(Mark::Spotlight { .. })));
        if !is_spot {
            return false;
        }
        self.commit(|st| {
            if let Some(Mark::Spotlight { zoom, .. }) = st.marks.get_mut(i) {
                *zoom = (*zoom + if up { 0.25 } else { -0.25 }).clamp(1.0, 4.0);
            }
        });
        true
    }

    /// I, or the eyedropper button: the next click takes the picture's colour there.
    fn start_eyedropper(&self) {
        self.0.style_pop.popdown();
        if let Some(s) = self.0.session.borrow_mut().as_mut()
            && s.tool != Tool::Eyedropper
        {
            s.before_eyedropper = s.tool;
        }
        self.set_tool(Tool::Eyedropper);
    }

    /// The original's colour at a canvas point, as the colour; then back to the tool before.
    fn sample_colour(&self, at: (f64, f64)) {
        let p = self.picture_point(at);
        let sampled = self.0.session.borrow().as_ref().map(|s| {
            let (w, h) = s.original.dimensions();
            let x = ((p.0 * w as f64) as u32).min(w.saturating_sub(1));
            let y = ((p.1 * h as f64) as u32).min(h.saturating_sub(1));
            let px = s.original.get_pixel(x, y).0;
            (Colour::rgb(px[0] as f64 / 255.0, px[1] as f64 / 255.0, px[2] as f64 / 255.0), s.before_eyedropper)
        });
        let Some((colour, back)) = sampled else { return };
        self.set_custom_colour(colour);
        self.set_tool(back);
        self.toast(&format!("Colour {}", colour.css()));
    }

    fn commit(&self, change: impl FnOnce(&mut EditState)) {
        let mut slot = self.0.session.borrow_mut();
        let Some(s) = slot.as_mut() else { return };
        let mut st = s.history.current().clone();
        change(&mut st);
        s.history.push(st);
        rerender(s);
        drop(slot);
        self.0.canvas.queue_draw();
        self.write_sidecar();
    }

    /// `m`, drawn on the canvas (which shows the cut and cropped picture), placed on the
    /// whole picture.
    fn placed(&self, m: Mark) -> Mark {
        let slot = self.0.session.borrow();
        let Some(s) = slot.as_ref() else { return m };
        let st = s.history.current();
        let m = match st.crop {
            Some(crop) => {
                let (w, h) = st.cut_size(s.original.width() as f64, s.original.height() as f64);
                m.uncropped(crop, w, h)
            }
            None => m,
        };
        m.uncut(st)
    }

    /// A canvas point (normalized to the cropped view) on the cut picture, before the
    /// crop: where crops and cuts are given.
    fn cut_point(&self, p: (f64, f64)) -> (f64, f64) {
        let slot = self.0.session.borrow();
        match slot.as_ref().and_then(|s| s.history.current().crop) {
            Some(c) => (c.x + p.0 * c.w, c.y + p.1 * c.h),
            None => p,
        }
    }

    /// A canvas point on the whole picture, where marks are kept.
    fn picture_point(&self, p: (f64, f64)) -> (f64, f64) {
        let p = self.cut_point(p);
        let slot = self.0.session.borrow();
        match slot.as_ref() {
            Some(s) => s.history.current().cut_to_picture(p),
            None => p,
        }
    }

    fn undo(&self) {
        if let Some(s) = self.0.session.borrow_mut().as_mut() {
            if !s.history.can_undo() {
                return;
            }
            s.history.undo();
            s.selected = None;
            rerender(s);
        }
        self.0.canvas.queue_draw();
        self.write_sidecar();
    }

    fn redo(&self) {
        if let Some(s) = self.0.session.borrow_mut().as_mut() {
            if !s.history.can_redo() {
                return;
            }
            s.history.redo();
            s.selected = None;
            rerender(s);
        }
        self.0.canvas.queue_draw();
        self.write_sidecar();
    }

    /// Remove the mark the Select tool picked.
    fn delete_selected(&self) {
        let Some(i) = self.0.session.borrow_mut().as_mut().and_then(|s| s.selected.take()) else { return };
        self.commit(|st| {
            if i < st.marks.len() {
                st.marks.remove(i);
            }
        });
    }

    fn copy_flat(&self) {
        let Some(sess) = self.0.session.borrow().as_ref().map(|s| (s.original.clone(), s.history.current().clone())) else { return };
        let me = self.clone();
        crate::cmd::background(
            move || {
                let img = annot::flatten(&sess.0, &sess.1);
                let dynimg = image::DynamicImage::ImageRgba8(img);
                let bytes = export::png_bytes(&dynimg)?;
                clipboard::copy_png(&bytes)?;
                anyhow::Ok(())
            },
            move |result| match result {
                Ok(()) => me.toast("Copied"),
                Err(err) => me.toast(&err.to_string()),
            },
        );
        self.write_sidecar();
    }

    fn save_flat(&self) {
        let Some((original, state)) = self.0.session.borrow().as_ref().map(|s| (s.original.clone(), s.history.current().clone())) else { return };
        let path = crate::naming::next_path("");
        let me = self.clone();
        crate::cmd::background(
            move || {
                let img = annot::flatten(&original, &state);
                export::write_png(&image::DynamicImage::ImageRgba8(img), &path)?;
                anyhow::Ok(path)
            },
            move |result| match result {
                Ok(path) => me.toast(&format!("Saved {}", path.display())),
                Err(err) => me.toast(&err.to_string()),
            },
        );
        self.write_sidecar();
    }

    fn share_flat(&self) {
        let Some((original, state)) = self.0.session.borrow().as_ref().map(|s| (s.original.clone(), s.history.current().clone())) else { return };
        let me = self.clone();
        crate::cmd::background(
            move || {
                let img = annot::flatten(&original, &state);
                export::png_bytes(&image::DynamicImage::ImageRgba8(img))
            },
            move |result| match result {
                Ok(bytes) => {
                    let name = format!("Screenshot {}.png", glib::DateTime::now_local().and_then(|d| d.format("%Y-%m-%d %H.%M.%S")).map(|s| s.to_string()).unwrap_or_default());
                    let me2 = me.clone();
                    crate::share::png(bytes, name, move |r| {
                        if let Err(e) = r {
                            me2.toast(&e);
                        }
                    });
                }
                Err(err) => me.toast(&err.to_string()),
            },
        );
        self.write_sidecar();
    }

    fn pin(&self) {
        let img = {
            let slot = self.0.session.borrow();
            let Some(s) = slot.as_ref() else { return };
            annot::flatten(&s.original, s.history.current())
        };
        crate::pin::open_rgba(img);
        self.write_sidecar();
    }

    fn redact(&self) {
        let Some((original, id)) = self.0.session.borrow().as_ref().map(|s| (s.original.clone(), s.id.clone())) else { return };
        let langs = prefs::get().ocr_langs;
        let mode = OcrMode::parse(&prefs::get().ocr_mode);
        let me = self.clone();
        crate::cmd::background(
            move || {
                let img = image::DynamicImage::ImageRgba8(original);
                let r = recognize(&img, mode, &langs)?;
                let added = nyx_capture_core::redact::marks(&r, img.width(), img.height());
                anyhow::Ok((added, id, r.text))
            },
            move |result| match result {
                Ok((added, _, _)) if added.is_empty() => me.toast("Nothing to redact"),
                Ok((added, id, _)) => {
                    me.commit(|st| st.marks.extend(added));
                    if let Some(id) = id {
                        crate::cmd::background(
                            move || store::open(&paths::library_db())?.set_sensitive(&id, true),
                            |_| crate::window::reload(),
                        );
                    }
                    me.toast("Redacted");
                }
                Err(err) => me.toast(&err.to_string()),
            },
        );
    }

    /// Read the picture's text, or (`area`, a canvas rect) an area's, and show it.
    fn ocr(&self, area: Option<NormRect>) {
        let Some(mut original) = self.0.session.borrow().as_ref().map(|s| s.original.clone()) else { return };
        let whole = area.is_none();
        if let Some(r) = area {
            let (a, b) = (self.picture_point((r.x, r.y)), self.picture_point((r.right(), r.bottom())));
            let (w, h) = (original.width() as f64, original.height() as f64);
            let x = (a.0 * w).floor().clamp(0.0, w - 1.0) as u32;
            let y = (a.1 * h).floor().clamp(0.0, h - 1.0) as u32;
            let cw = ((b.0 * w).ceil() as u32).saturating_sub(x).clamp(1, original.width() - x);
            let ch = ((b.1 * h).ceil() as u32).saturating_sub(y).clamp(1, original.height() - y);
            original = image::imageops::crop_imm(&original, x, y, cw, ch).to_image();
        }
        let shown = original.clone();
        // Only the whole picture's text goes into history's search.
        let id = self.0.session.borrow().as_ref().and_then(|s| s.id.clone()).filter(|_| whole);
        let langs = prefs::get().ocr_langs;
        let mode = OcrMode::parse(&prefs::get().ocr_mode);
        let me = self.clone();
        crate::cmd::background(
            move || {
                let r = recognize(&image::DynamicImage::ImageRgba8(original), mode, &langs)?;
                if let Some(id) = id.filter(|_| !r.text.trim().is_empty()) {
                    let db = store::open(&paths::library_db())?;
                    db.set_ocr(&id, &r.engine, &r.text)?;
                }
                anyhow::Ok(r)
            },
            move |result| match result {
                Ok(r) => {
                    if r.text.trim().is_empty() {
                        me.toast("No text found");
                    } else {
                        crate::window::reload();
                        crate::ocr_view::show(Some(me.0.window.upcast_ref()), &shown, &r);
                    }
                }
                Err(err) => me.toast(&err.to_string()),
            },
        );
    }

    /// Save the marks beside the original, then remake the history thumbnail from the
    /// edited picture (so a redaction shows, and hides, there too).
    fn write_sidecar(&self) {
        let Some((path, state, id)) =
            self.0.session.borrow().as_ref().and_then(|s| Some((s.path.clone()?, s.history.current().clone(), s.id.clone())))
        else {
            return;
        };
        crate::cmd::background(
            move || {
                state.save(&annot::sidecar_path(&path))?;
                if let Some(id) = &id {
                    store::open(&paths::library_db())?.thumb(id, &path, &paths::thumb_file(id), true)?;
                }
                anyhow::Ok(id)
            },
            |result| {
                if let Ok(Some(id)) = result {
                    crate::grid::drop_texture(&id);
                    crate::window::reload();
                }
            },
        );
    }

    fn draw(&self, cr: &cairo::Context, ww: f64, wh: f64) {
        let prepared = {
            let s = self.0.session.borrow();
            let Some(s) = s.as_ref() else { return };
            // The backdrop's mat fits the window too; the picture sits inside it.
            let (outer_w, outer_h) = (s.work.width() as f64, s.work.height() as f64);
            let (iw, ih) = (outer_w - s.inset * 2.0, outer_h - s.inset * 2.0);
            if iw < 1.0 || ih < 1.0 {
                return;
            }
            let scale = ((ww - PAD * 2.0) / outer_w).min((wh - PAD * 2.0) / outer_h).max(0.01) * s.zoom;
            let dw = iw * scale;
            let dh = ih * scale;
            let ox = (ww - outer_w * scale) / 2.0 + s.pan.0 + s.inset * scale;
            let oy = (wh - outer_h * scale) / 2.0 + s.pan.1 + s.inset * scale;
            let inset = s.inset;
            let tool = s.tool;
            let style = Style::of(s);
            let draft = s.draft.as_ref().and_then(|d| draft_mark(d, tool, &style));
            let spot_draft = match (&s.draft, tool) {
                (Some(Draft::Shape { from, to }), Tool::Spotlight) => Some((NormRect::from_corners(*from, *to), style.shape)),
                _ => None,
            };
            let crop_draft = match (&s.draft, tool) {
                (Some(Draft::Shape { from, to }), Tool::Crop) => Some((*from, *to)),
                _ => None,
            };
            let area_draft = match (&s.draft, tool) {
                (Some(Draft::Shape { from, to }), Tool::Ocr) => Some(NormRect::from_corners(*from, *to)),
                // The band a Cut drag would take out, across the whole picture.
                (Some(Draft::Shape { from, to }), Tool::Cut) => Some(match cut_band(*from, *to) {
                    (Axis::Rows, a, b) => NormRect::new(0.0, a, 1.0, b - a),
                    (Axis::Cols, a, b) => NormRect::new(a, 0.0, b - a, 1.0),
                }),
                _ => None,
            };
            let surface = s.surface.clone();
            // The picked mark's box, in view terms (and moved along while dragging).
            let st = s.history.current();
            let crop = st.crop.unwrap_or(NormRect::FULL);
            let selection = s.selected.and_then(|i| st.marks.get(i)).map(|m| {
                let b = m.bbox(0.01);
                let b = NormRect::from_corners(st.picture_to_cut((b.x, b.y)), st.picture_to_cut((b.right(), b.bottom())));
                let mut r = NormRect::new((b.x - crop.x) / crop.w, (b.y - crop.y) / crop.h, b.w / crop.w, b.h / crop.h);
                if let Some(Draft::Move { from, to }) = &s.draft {
                    r = NormRect::new(r.x + to.0 - from.0, r.y + to.1 - from.1, r.w, r.h);
                }
                r
            });
            (iw, ih, scale, dw, dh, ox, oy, inset, surface, draft, spot_draft, crop_draft, area_draft, selection)
        };
        let (iw, ih, scale, dw, dh, ox, oy, inset, surface, draft, spot_draft, crop_draft, area_draft, selection) = prepared;
        let accent = parse_colour(&crate::theme::palette().accent);
        if let Some(sess) = self.0.session.borrow_mut().as_mut() {
            sess.image_rect = (ox, oy, dw, dh);
        }
        if let Some(surface) = surface {
            let _ = cr.save();
            cr.translate(ox, oy);
            cr.scale(scale, scale);
            if cr.set_source_surface(&surface, -inset, -inset).is_ok() {
                cr.source().set_filter(cairo::Filter::Nearest);
                let _ = cr.paint();
            }
            if let Some(m) = draft {
                annot::draw_marks(cr, iw, ih, &[m]);
            }
            if let Some((rect, shape)) = spot_draft {
                annot::draw_spotlight_preview(cr, iw, ih, rect, shape);
            }
            let _ = cr.restore();
        }
        if let Some((from, to)) = crop_draft {
            let r = NormRect::from_corners(from, to).scaled(dw, dh);
            // The theme's shadow tone dims what the crop leaves out.
            let shade = parse_colour(&crate::theme::palette().shadow).unwrap_or(Colour::rgb(0.0, 0.0, 0.0));
            cr.set_source_rgba(shade.r, shade.g, shade.b, 0.55);
            cr.rectangle(ox, oy, dw, dh);
            cr.rectangle(ox + r.x, oy + r.y, r.w, r.h);
            cr.set_fill_rule(cairo::FillRule::EvenOdd);
            let _ = cr.fill();
            if let Some(accent) = accent {
                cr.set_source_rgba(accent.r, accent.g, accent.b, 1.0);
                cr.set_line_width(1.0);
                cr.rectangle(ox + r.x + 0.5, oy + r.y + 0.5, r.w - 1.0, r.h - 1.0);
                let _ = cr.stroke();
            }
        }
        // The area Read text will read, or the band Cut will take out (in the danger tone).
        let band = (area_draft.is_some() && self.0.session.borrow().as_ref().is_some_and(|s| s.tool == Tool::Cut))
            .then(|| parse_colour(&crate::theme::palette().danger))
            .flatten();
        if let (Some(r), Some(a)) = (area_draft, band.or(accent)) {
            let r = r.scaled(dw, dh);
            cr.set_source_rgba(a.r, a.g, a.b, if band.is_some() { 0.3 } else { 0.12 });
            cr.rectangle(ox + r.x, oy + r.y, r.w, r.h);
            let _ = cr.fill();
            cr.set_source_rgba(a.r, a.g, a.b, 1.0);
            cr.set_line_width(1.5);
            cr.set_dash(&[5.0, 4.0], 0.0);
            cr.rectangle(ox + r.x + 0.5, oy + r.y + 0.5, r.w, r.h);
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
        }
        if let (Some(r), Some(a)) = (selection, accent) {
            let r = r.scaled(dw, dh);
            cr.set_source_rgba(a.r, a.g, a.b, 1.0);
            cr.set_line_width(1.5);
            cr.set_dash(&[5.0, 4.0], 0.0);
            cr.rectangle(ox + r.x + 0.5, oy + r.y + 0.5, r.w, r.h);
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
        }
    }

    fn wire_pointer(&self) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let me = self.clone();
        drag.connect_drag_begin(move |_, x, y| me.drag_begin(x, y));
        let me = self.clone();
        drag.connect_drag_update(move |g, dx, dy| {
            if let Some((x, y)) = g.start_point() {
                me.drag_update(x + dx, y + dy);
            }
        });
        let me = self.clone();
        drag.connect_drag_end(move |g, dx, dy| {
            if let Some((x, y)) = g.start_point() {
                me.drag_end(x, y, dx, dy);
            }
        });
        self.0.canvas.add_controller(drag);
    }

    fn wire_keys(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let me = self.clone();
        keys.connect_key_pressed(move |_, key, _, mods| {
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
            let typing = gtk::prelude::GtkWindowExt::focus(&me.0.window).is_some_and(|f| f.dynamic_cast_ref::<gtk::Editable>().is_some());
            if typing {
                return glib::Propagation::Proceed;
            }
            let plain = !ctrl && !mods.contains(gdk::ModifierType::ALT_MASK);
            if plain && key.to_lower() == gdk::Key::i {
                me.start_eyedropper();
                return glib::Propagation::Stop;
            }
            if shift && !ctrl && key == gdk::Key::B {
                me.0.backdrop.button.popup();
                return glib::Propagation::Stop;
            }
            if plain && let Some((tool, ..)) = TOOLS.iter().find(|t| t.3 == key.to_lower()) {
                // Arrow's and Spotlight's key again: their next style.
                let current = me.0.session.borrow().as_ref().map(|s| s.tool);
                if current == Some(*tool) && matches!(tool, Tool::Arrow | Tool::Spotlight) {
                    me.next_style(*tool);
                } else if let Some((_, b)) = me.0.tool_buttons.iter().find(|(t, _)| t == tool) {
                    b.set_active(true);
                }
                return glib::Propagation::Stop;
            }
            match key {
                gdk::Key::Escape => {
                    me.0.window.close();
                    glib::Propagation::Stop
                }
                gdk::Key::w | gdk::Key::q if ctrl => {
                    me.0.window.close();
                    glib::Propagation::Stop
                }
                gdk::Key::Delete | gdk::Key::BackSpace => {
                    me.delete_selected();
                    glib::Propagation::Stop
                }
                gdk::Key::_0 if ctrl => {
                    me.set_zoom(1.0, None);
                    glib::Propagation::Stop
                }
                gdk::Key::plus | gdk::Key::equal if ctrl => {
                    me.zoom_by(1.25, None);
                    glib::Propagation::Stop
                }
                gdk::Key::minus if ctrl => {
                    me.zoom_by(0.8, None);
                    glib::Propagation::Stop
                }
                gdk::Key::F1 => {
                    show_shortcuts(&me.0.window);
                    glib::Propagation::Stop
                }
                gdk::Key::z if ctrl && shift => {
                    me.redo();
                    glib::Propagation::Stop
                }
                gdk::Key::z if ctrl => {
                    me.undo();
                    glib::Propagation::Stop
                }
                gdk::Key::y if ctrl => {
                    me.redo();
                    glib::Propagation::Stop
                }
                gdk::Key::c if ctrl => {
                    me.copy_flat();
                    glib::Propagation::Stop
                }
                gdk::Key::s if ctrl => {
                    me.save_flat();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.0.window.add_controller(keys);
    }

    fn drag_begin(&self, x: f64, y: f64) {
        let picked = {
            let slot = self.0.session.borrow();
            let Some(s) = slot.as_ref() else { return };
            let Some(p) = norm(s.image_rect, x, y) else { return };
            (s.tool == Tool::Select).then_some(p)
        };
        // Select: the topmost mark under the pointer (in picture terms).
        let hit = picked.map(|p| {
            let at = self.picture_point(p);
            self.0
                .session
                .borrow()
                .as_ref()
                .and_then(|s| s.history.current().marks.iter().rposition(|m| m.bbox(0.01).contains(at.0, at.1)))
        });
        let mut slot = self.0.session.borrow_mut();
        let Some(s) = slot.as_mut() else { return };
        let Some(p) = norm(s.image_rect, x, y) else { return };
        if let Some(hit) = hit {
            s.selected = hit;
            s.draft = hit.map(|_| Draft::Move { from: p, to: p });
            drop(slot);
            self.0.canvas.queue_draw();
            return;
        }
        s.draft = Some(match s.tool {
            Tool::Pen | Tool::Highlighter => Draft::Stroke(vec![p]),
            _ => Draft::Shape { from: p, to: p },
        });
        drop(slot);
        self.0.canvas.queue_draw();
    }

    fn drag_update(&self, x: f64, y: f64) {
        let mut slot = self.0.session.borrow_mut();
        let Some(s) = slot.as_mut() else { return };
        let Some(p) = norm(s.image_rect, x, y) else { return };
        match &mut s.draft {
            Some(Draft::Stroke(points)) => {
                if points.last().is_none_or(|l| (l.0 - p.0).hypot(l.1 - p.1) > 0.002) {
                    points.push(p);
                }
            }
            Some(Draft::Shape { to, .. }) | Some(Draft::Move { to, .. }) => *to = p,
            None => {}
        }
        drop(slot);
        self.0.canvas.queue_draw();
    }

    fn drag_end(&self, x: f64, y: f64, dx: f64, dy: f64) {
        let (draft, tool, style) = {
            let mut slot = self.0.session.borrow_mut();
            let Some(s) = slot.as_mut() else { return };
            let draft = s.draft.take();
            (draft, s.tool, Style::of(s))
        };
        let (colour, size) = (style.colour, style.size);
        // Marks are drawn on the canvas (the cropped view) and kept on the whole picture.
        let add = |m: Mark| {
            let m = self.placed(m);
            self.commit(|st| st.marks.push(m));
        };
        match (draft, tool) {
            (Some(Draft::Move { from, to }), Tool::Select) => {
                let Some(i) = self.0.session.borrow().as_ref().and_then(|s| s.selected) else { return };
                let (a, b) = (self.picture_point(from), self.picture_point(to));
                if (a.0 - b.0).hypot(a.1 - b.1) > 0.001 {
                    self.commit(|st| {
                        if let Some(m) = st.marks.get_mut(i) {
                            *m = m.translated(b.0 - a.0, b.1 - a.1);
                        }
                    });
                } else {
                    self.0.canvas.queue_draw();
                }
            }
            (Some(Draft::Stroke(points)), Tool::Pen | Tool::Highlighter) => {
                add(Mark::Stroke { points, colour, size, highlighter: tool == Tool::Highlighter });
            }
            (Some(Draft::Shape { from, to }), Tool::Arrow | Tool::Line | Tool::Rect | Tool::Ellipse)
                if (from.0 - to.0).hypot(from.1 - to.1) > 0.01 =>
            {
                match draft_mark(&Draft::Shape { from, to }, tool, &style) {
                    Some(m) => add(m),
                    None => self.0.canvas.queue_draw(),
                }
            }
            (Some(Draft::Shape { from, to }), Tool::Spotlight | Tool::Blur | Tool::Pixelate | Tool::Fill | Tool::Crop) => {
                let r = NormRect::from_corners(from, to);
                if r.w > 0.005 && r.h > 0.005 {
                    match tool {
                        Tool::Spotlight => add(Mark::Spotlight { rect: r, shape: style.shape, zoom: 1.0 }),
                        Tool::Blur => add(Mark::Blur { rect: r, size }),
                        Tool::Pixelate => add(Mark::Pixelate { rect: r, size }),
                        Tool::Fill => add(Mark::Fill { rect: r, colour: Colour::rgb(0.0, 0.0, 0.0) }),
                        // A crop of the cropped view is a crop of that crop.
                        Tool::Crop => {
                            let (a, b) = (self.cut_point((r.x, r.y)), self.cut_point((r.right(), r.bottom())));
                            self.commit(|st| st.set_crop(Some(NormRect::from_corners(a, b))));
                        }
                        _ => self.0.canvas.queue_draw(),
                    }
                } else {
                    self.0.canvas.queue_draw();
                }
            }
            (Some(Draft::Shape { from, to }), Tool::Cut) => {
                let (axis, a, b) = cut_band(from, to);
                if (b - a).abs() > 0.005 {
                    // The band on the cut picture, before the crop.
                    let (pa, pb) = match axis {
                        Axis::Rows => (self.cut_point((0.0, a)).1, self.cut_point((0.0, b)).1),
                        Axis::Cols => (self.cut_point((a, 0.0)).0, self.cut_point((b, 0.0)).0),
                    };
                    self.commit(|st| st.add_cut(axis, pa, pb));
                } else {
                    self.0.canvas.queue_draw();
                }
            }
            (Some(Draft::Shape { to, .. }), Tool::Eyedropper) => self.sample_colour(to),
            (Some(Draft::Shape { from, to }), Tool::Ocr) => {
                self.0.canvas.queue_draw();
                let r = NormRect::from_corners(from, to);
                self.ocr((r.w > 0.01 && r.h > 0.01).then_some(r));
            }
            (Some(Draft::Shape { from, .. }), Tool::Text) if dx.hypot(dy) < 6.0 => {
                self.ask_text(x, y, from);
            }
            (Some(Draft::Shape { from, .. }), Tool::Callout) if dx.hypot(dy) < 8.0 => {
                let at = self.picture_point(from);
                self.commit(|st| {
                    let n = st.next_callout();
                    st.marks.push(Mark::Callout { at, n, colour });
                });
            }
            _ => self.0.canvas.queue_draw(),
        }
    }

    fn ask_text(&self, x: f64, y: f64, at: (f64, f64)) {
        let popover = gtk::Popover::new();
        popover.set_parent(&self.0.canvas);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("Type, then press Enter"));
        entry.set_width_chars(24);
        popover.set_child(Some(&entry));
        let (me, pop) = (self.clone(), popover.clone());
        entry.connect_activate(move |e| {
            let text = e.text().trim().to_string();
            pop.popdown();
            if text.is_empty() {
                return;
            }
            let Some((colour, slider)) = me.0.session.borrow().as_ref().map(|s| (s.colour, s.size)) else { return };
            let size = mark_size(Tool::Text, slider);
            let m = me.placed(Mark::Text { at, text, colour, size });
            me.commit(|st| st.marks.push(m));
        });
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
        entry.grab_focus();
    }

    /// Ctrl+scroll zooms around the pointer; scrolling pans a zoomed picture.
    fn wire_zoom(&self) {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let me = self.clone();
        scroll.connect_scroll(move |c, dx, dy| {
            let at = c.current_event().and_then(|e| e.position());
            if c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                me.zoom_by(if dy < 0.0 { 1.15 } else { 1.0 / 1.15 }, at);
                return glib::Propagation::Stop;
            }
            if dy != 0.0 && me.zoom_spotlight(dy < 0.0) {
                return glib::Propagation::Stop;
            }
            let zoomed = me.0.session.borrow().as_ref().is_some_and(|s| s.zoom > 1.0);
            if zoomed {
                if let Some(s) = me.0.session.borrow_mut().as_mut() {
                    s.pan.0 -= dx * 40.0;
                    s.pan.1 -= dy * 40.0;
                }
                me.0.canvas.queue_draw();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.0.canvas.add_controller(scroll);
    }

    fn zoom_by(&self, factor: f64, at: Option<(f64, f64)>) {
        let z = self.0.session.borrow().as_ref().map(|s| s.zoom).unwrap_or(1.0);
        self.set_zoom(z * factor, at);
    }

    /// Zoom to `zoom` (1 fits), keeping the point under `at` (canvas pixels) in place.
    fn set_zoom(&self, zoom: f64, at: Option<(f64, f64)>) {
        let zoom = zoom.clamp(1.0, 8.0);
        let (cw, ch) = (self.0.canvas.width() as f64, self.0.canvas.height() as f64);
        if let Some(s) = self.0.session.borrow_mut().as_mut() {
            if zoom == 1.0 {
                s.pan = (0.0, 0.0);
            } else {
                let (px, py) = at.unwrap_or((cw / 2.0, ch / 2.0));
                // The picture point under the pointer stays under it.
                let k = zoom / s.zoom;
                let (cx, cy) = (cw / 2.0 + s.pan.0, ch / 2.0 + s.pan.1);
                s.pan.0 += (px - cx) * (1.0 - k);
                s.pan.1 += (py - cy) * (1.0 - k);
            }
            s.zoom = zoom;
        }
        self.0.canvas.queue_draw();
    }

    fn toast(&self, message: &str) {
        let overlay = self.0.toast_host.clone();
        // One at a time: a new message replaces the last.
        let mut child = overlay.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            if c.has_css_class("toast") {
                overlay.remove_overlay(&c);
            }
        }
        let label = gtk::Label::new(Some(message));
        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bx.add_css_class("toast");
        bx.set_halign(gtk::Align::Center);
        bx.set_valign(gtk::Align::End);
        bx.set_margin_bottom(56);
        bx.append(&label);
        overlay.add_overlay(&bx);
        glib::timeout_add_local_once(std::time::Duration::from_millis(3500), move || {
            if bx.parent().is_some() {
                overlay.remove_overlay(&bx);
            }
        });
    }
}

/// Re-render the canvas picture (and its surface) from the current state.
fn rerender(s: &mut Session) {
    (s.work, s.inset) = work_image(&s.preview_src, s.history.current());
    s.surface = rgba_surface(&s.work);
}

/// The editor's keys, in a dialog over it.
fn show_shortcuts(parent: &gtk::ApplicationWindow) {
    let (dialog, card) = widgets::dialog("Editor shortcuts", 460);
    dialog.set_transient_for(Some(parent));
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.add_css_class("group-list");
    let mut rows: Vec<(String, String)> = TOOLS.iter().map(|(_, _, tip, key)| {
        let name = tip.split(" (").next().unwrap_or(tip).to_string();
        (key.to_upper().name().map(|n| n.to_string()).unwrap_or_default(), name)
    }).collect();
    for (k, what) in [
        ("A", "Arrow again: next arrow style"),
        ("S", "Spotlight again: next shape"),
        ("Scroll", "Magnify inside the selected spotlight"),
        ("I", "Pick a colour from the image"),
        ("Shift+B", "Backdrop"),
        ("Delete", "Remove the selected mark"),
        ("Ctrl+Z", "Undo"),
        ("Ctrl+Shift+Z", "Redo"),
        ("Ctrl+C", "Copy"),
        ("Ctrl+S", "Save"),
        ("Ctrl+Scroll", "Zoom (Ctrl+0 fits)"),
        ("Esc", "Close"),
    ] {
        rows.push((k.to_string(), what.to_string()));
    }
    for (keys, what) in rows {
        let caps: Vec<&str> = keys.split('+').collect();
        list.append(&widgets::row(&what, "", Some(widgets::key_caps(&caps).upcast_ref())));
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .propagate_natural_height(true)
        .max_content_height(520)
        .child(&list)
        .build();
    card.append(&scroll);
    dialog.present();
}

fn theme_swatches() -> Vec<Colour> {
    let p = crate::theme::palette();
    let accent = parse_colour(&p.accent).unwrap_or(Colour::rgb(0.4, 0.6, 1.0));
    let danger = parse_colour(&p.danger).unwrap_or(accent);
    let text = parse_colour(&p.text).unwrap_or(Colour::rgb(1.0, 1.0, 1.0));
    let bg = parse_colour(&p.bg).unwrap_or(Colour::rgb(0.1, 0.1, 0.12));
    let mix = |a: Colour, b: Colour, t: f64| Colour::rgb(a.r + (b.r - a.r) * t, a.g + (b.g - a.g) * t, a.b + (b.b - a.b) * t);
    vec![text, bg, accent, danger, mix(accent, text, 0.4), mix(danger, accent, 0.45), mix(accent, bg, 0.3), mix(danger, text, 0.25)]
}

fn parse_colour(hex: &str) -> Option<Colour> {
    let h = hex.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Colour::rgb(((v >> 16) & 0xff) as f64 / 255.0, ((v >> 8) & 0xff) as f64 / 255.0, (v & 0xff) as f64 / 255.0))
}

fn walk_swatches(row: &gtk::Box, on: impl Fn(Colour) + Clone + 'static) {
    let mut child = row.first_child();
    while let Some(next) = child {
        if let Some(btn) = next.downcast_ref::<gtk::Button>() {
            let name = btn.widget_name().to_string();
            if let Some(c) = parse_colour(&name) {
                let on = on.clone();
                btn.connect_clicked(move |_| on(c));
            }
        }
        child = next.next_sibling();
    }
}

fn mark_size(tool: Tool, slider: f64) -> f64 {
    match tool {
        Tool::Pen | Tool::Highlighter | Tool::Arrow | Tool::Line | Tool::Rect | Tool::Ellipse => slider * 0.002,
        Tool::Text => slider * 0.008,
        Tool::Blur
        | Tool::Pixelate
        | Tool::Fill
        | Tool::Crop
        | Tool::Callout
        | Tool::Select
        | Tool::Ocr
        | Tool::Eyedropper
        | Tool::Spotlight
        | Tool::Cut => slider * 0.006,
    }
}

/// How the next mark is drawn: the popover's choices and the tools' styles.
struct Style {
    colour: Colour,
    size: f64,
    arrow: ArrowStyle,
    shape: SpotShape,
    fill: bool,
    /// Corner radius, a share of the picture's shorter side.
    radius: f64,
}

impl Style {
    fn of(s: &Session) -> Style {
        Style {
            colour: s.colour,
            size: mark_size(s.tool, s.size),
            arrow: s.arrow_style,
            shape: s.spot_shape,
            fill: s.fill,
            radius: s.corners * 0.0025,
        }
    }
}

/// A Cut drag's band: rows when it went further down than across, else columns; and
/// its two edges on that axis (canvas-normalized).
fn cut_band(from: (f64, f64), to: (f64, f64)) -> (Axis, f64, f64) {
    if (to.1 - from.1).abs() >= (to.0 - from.0).abs() {
        (Axis::Rows, from.1.min(to.1), from.1.max(to.1))
    } else {
        (Axis::Cols, from.0.min(to.0), from.0.max(to.0))
    }
}

fn draft_mark(draft: &Draft, tool: Tool, style: &Style) -> Option<Mark> {
    let (colour, size) = (style.colour, style.size);
    match (draft, tool) {
        (Draft::Stroke(points), Tool::Pen | Tool::Highlighter) => {
            Some(Mark::Stroke { points: points.clone(), colour, size, highlighter: tool == Tool::Highlighter })
        }
        (Draft::Shape { from, to }, Tool::Arrow) => Some(Mark::Arrow { from: *from, to: *to, colour, size, style: style.arrow }),
        (Draft::Shape { from, to }, Tool::Line) => Some(Mark::Line { from: *from, to: *to, colour, size }),
        (Draft::Shape { from, to }, Tool::Rect) => {
            Some(Mark::Rect { rect: NormRect::from_corners(*from, *to), colour, size, fill: style.fill, radius: style.radius })
        }
        (Draft::Shape { from, to }, Tool::Ellipse) => Some(Mark::Ellipse { rect: NormRect::from_corners(*from, *to), colour, size, fill: style.fill }),
        _ => None,
    }
}

fn norm(rect: (f64, f64, f64, f64), x: f64, y: f64) -> Option<(f64, f64)> {
    let (ox, oy, w, h) = rect;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some((((x - ox) / w).clamp(0.0, 1.0), ((y - oy) / h).clamp(0.0, 1.0)))
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
