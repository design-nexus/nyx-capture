//! Editable annotation objects. The original capture file is never overwritten.

pub mod backdrop;
mod raster;

pub use backdrop::{Backdrop, Fill as BackdropFill};
pub use raster::{draw_marks, draw_spotlight_preview, preview, render};

/// The finished picture: [`render`], then on its backdrop if it has one. Everything
/// handed out (copy, save, pin, share, thumbnails) is this.
pub fn flatten(src: &image::RgbaImage, st: &EditState) -> image::RgbaImage {
    let img = render(src, st);
    match &st.backdrop {
        Some(b) => backdrop::compose(&img, b),
        None => img,
    }
}

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl NormRect {
    pub const FULL: NormRect = NormRect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };

    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn from_corners(a: (f64, f64), b: (f64, f64)) -> Self {
        Self::new(a.0.min(b.0), a.1.min(b.1), (a.0 - b.0).abs(), (a.1 - b.1).abs())
    }

    pub fn right(self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(self) -> f64 {
        self.y + self.h
    }

    pub fn contains(self, px: f64, py: f64) -> bool {
        px >= self.x && py >= self.y && px <= self.right() && py <= self.bottom()
    }

    pub fn scaled(self, sx: f64, sy: f64) -> Self {
        Self::new(self.x * sx, self.y * sy, self.w * sx, self.h * sy)
    }

    pub fn is_full(self) -> bool {
        self.w >= 0.9999 && self.h >= 0.9999 && self.x <= 0.0001 && self.y <= 0.0001
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Colour {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Colour {
    pub const fn rgb(r: f64, g: f64, b: f64) -> Colour {
        Colour { r, g, b }
    }

    pub fn contrast(self) -> Colour {
        if 0.299 * self.r + 0.587 * self.g + 0.114 * self.b > 0.55 {
            Colour::rgb(0.0, 0.0, 0.0)
        } else {
            Colour::rgb(1.0, 1.0, 1.0)
        }
    }

    pub fn css(self) -> String {
        let b = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;
        format!("#{:02x}{:02x}{:02x}", b(self.r), b(self.g), b(self.b))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrowStyle {
    #[default]
    Standard,
    /// A shaft that narrows to its tail.
    Pointy,
    /// A shaft bent to one side.
    Curved,
    /// A head at both ends.
    Double,
}

impl ArrowStyle {
    pub fn next(self) -> ArrowStyle {
        match self {
            ArrowStyle::Standard => ArrowStyle::Pointy,
            ArrowStyle::Pointy => ArrowStyle::Curved,
            ArrowStyle::Curved => ArrowStyle::Double,
            ArrowStyle::Double => ArrowStyle::Standard,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ArrowStyle::Standard => "Standard",
            ArrowStyle::Pointy => "Pointy",
            ArrowStyle::Curved => "Curved",
            ArrowStyle::Double => "Double",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpotShape {
    #[default]
    Ellipse,
    Rect,
    Rounded,
}

impl SpotShape {
    pub fn next(self) -> SpotShape {
        match self {
            SpotShape::Ellipse => SpotShape::Rect,
            SpotShape::Rect => SpotShape::Rounded,
            SpotShape::Rounded => SpotShape::Ellipse,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SpotShape::Ellipse => "Ellipse",
            SpotShape::Rect => "Rectangle",
            SpotShape::Rounded => "Rounded",
        }
    }
}

fn one() -> f64 {
    1.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// A horizontal band: rows go, the parts above and below close up.
    Rows,
    /// A vertical band: columns go, the sides close up.
    Cols,
}

/// A band taken out of the picture: rows or columns from `from` to `to`, as a share
/// of the picture the cuts before it left. The two sides close up.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cut {
    pub axis: Axis,
    pub from: f64,
    pub to: f64,
}

impl Cut {
    /// Never the whole picture.
    fn span(self) -> f64 {
        (self.to - self.from).clamp(0.0, 0.95)
    }

    /// A coordinate on this cut's axis, from before the cut to after it. Inside the
    /// band, the seam.
    fn forward(self, v: f64) -> f64 {
        let d = self.span();
        let v = if v >= self.from + d {
            v - d
        } else if v > self.from {
            self.from
        } else {
            v
        };
        v / (1.0 - d)
    }

    /// A coordinate after the cut, back to before it.
    fn back(self, v: f64) -> f64 {
        let d = self.span();
        let pre = v * (1.0 - d);
        if pre >= self.from { pre + d } else { pre }
    }

    fn map(self, p: (f64, f64), f: impl Fn(Cut, f64) -> f64) -> (f64, f64) {
        match self.axis {
            Axis::Rows => (p.0, f(self, p.1)),
            Axis::Cols => (f(self, p.0), p.1),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mark {
    Stroke { points: Vec<(f64, f64)>, colour: Colour, size: f64, highlighter: bool },
    Line { from: (f64, f64), to: (f64, f64), colour: Colour, size: f64 },
    Arrow {
        from: (f64, f64),
        to: (f64, f64),
        colour: Colour,
        size: f64,
        #[serde(default)]
        style: ArrowStyle,
    },
    Rect {
        rect: NormRect,
        colour: Colour,
        size: f64,
        #[serde(default)]
        fill: bool,
        /// Corner radius, a share of the picture's shorter side.
        #[serde(default)]
        radius: f64,
    },
    Ellipse {
        rect: NormRect,
        colour: Colour,
        size: f64,
        #[serde(default)]
        fill: bool,
    },
    Text { at: (f64, f64), text: String, colour: Colour, size: f64 },
    Callout { at: (f64, f64), n: u32, colour: Colour },
    Pixelate { rect: NormRect, size: f64 },
    Blur { rect: NormRect, size: f64 },
    /// A solid box: redaction nothing can be read back from.
    Fill { rect: NormRect, colour: Colour },
    /// Everything outside the shape dimmed; with `zoom` above 1, what's inside is
    /// magnified about its centre (a loupe).
    Spotlight {
        rect: NormRect,
        #[serde(default)]
        shape: SpotShape,
        #[serde(default = "one")]
        zoom: f64,
    },
}

impl Mark {
    /// Every point the mark is placed by, for moving and mapping it.
    fn map_points(&mut self, f: impl Fn((f64, f64)) -> (f64, f64), g: impl Fn(NormRect) -> NormRect) {
        match self {
            Mark::Stroke { points, .. } => points.iter_mut().for_each(|p| *p = f(*p)),
            Mark::Line { from, to, .. } | Mark::Arrow { from, to, .. } => {
                *from = f(*from);
                *to = f(*to);
            }
            Mark::Text { at, .. } | Mark::Callout { at, .. } => *at = f(*at),
            Mark::Rect { rect, .. }
            | Mark::Ellipse { rect, .. }
            | Mark::Pixelate { rect, .. }
            | Mark::Blur { rect, .. }
            | Mark::Fill { rect, .. }
            | Mark::Spotlight { rect, .. } => *rect = g(*rect),
        }
    }

    fn size_mut(&mut self) -> Option<&mut f64> {
        match self {
            Mark::Stroke { size, .. }
            | Mark::Line { size, .. }
            | Mark::Arrow { size, .. }
            | Mark::Rect { size, .. }
            | Mark::Ellipse { size, .. }
            | Mark::Text { size, .. }
            | Mark::Pixelate { size, .. }
            | Mark::Blur { size, .. } => Some(size),
            Mark::Callout { .. } | Mark::Fill { .. } | Mark::Spotlight { .. } => None,
        }
    }

    /// The mark as drawn on a view cropped to `crop`, in the whole picture's terms. A
    /// picture is `w`×`h`; sizes are a share of the shorter side, so they scale too.
    pub fn uncropped(&self, crop: NormRect, w: f64, h: f64) -> Mark {
        let mut m = self.clone();
        m.map_points(
            |(x, y)| (crop.x + x * crop.w, crop.y + y * crop.h),
            |r| NormRect::new(crop.x + r.x * crop.w, crop.y + r.y * crop.h, r.w * crop.w, r.h * crop.h),
        );
        let ratio = (crop.w * w).min(crop.h * h) / w.min(h).max(1.0);
        if let Some(size) = m.size_mut() {
            *size *= ratio;
        }
        m
    }

    /// The mark as drawn on the picture with `st`'s cuts taken out, on the picture
    /// before them. A rect is placed by its corners, so one across a seam spans the band.
    pub fn uncut(&self, st: &EditState) -> Mark {
        if st.cuts.is_empty() {
            return self.clone();
        }
        let mut m = self.clone();
        m.map_points(
            |p| st.cut_to_picture(p),
            |r| NormRect::from_corners(st.cut_to_picture((r.x, r.y)), st.cut_to_picture((r.right(), r.bottom()))),
        );
        m
    }

    /// Moved by (`dx`, `dy`), in picture-normalized units.
    pub fn translated(&self, dx: f64, dy: f64) -> Mark {
        let mut m = self.clone();
        m.map_points(|(x, y)| (x + dx, y + dy), |r| NormRect::new(r.x + dx, r.y + dy, r.w, r.h));
        m
    }

    /// A box around the mark, with `pad` added each side (picture-normalized).
    pub fn bbox(&self, pad: f64) -> NormRect {
        let mut pts = Vec::new();
        let mut m = self.clone();
        let collect = std::cell::RefCell::new(&mut pts);
        m.map_points(
            |p| {
                collect.borrow_mut().push(p);
                p
            },
            |r| {
                collect.borrow_mut().push((r.x, r.y));
                collect.borrow_mut().push((r.right(), r.bottom()));
                r
            },
        );
        let (x0, y0, x1, y1) = pts.iter().fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |a, p| {
            (a.0.min(p.0), a.1.min(p.1), a.2.max(p.0), a.3.max(p.1))
        });
        if pts.is_empty() {
            return NormRect::new(0.0, 0.0, 0.0, 0.0);
        }
        NormRect::new(x0 - pad, y0 - pad, (x1 - x0) + pad * 2.0, (y1 - y0) + pad * 2.0)
    }
}

/// How marks relate to the crop. 0 (sidecars from before): marks are in the cropped
/// picture's terms and drawn after cropping. 1: marks are in the whole picture's terms
/// and drawn before it.
pub const STATE_VERSION: u32 = 1;

/// Marks are drawn on the whole picture, then the cuts are taken out in order, then
/// the crop (in the cut picture's terms) is applied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditState {
    pub crop: Option<NormRect>,
    pub marks: Vec<Mark>,
    #[serde(default)]
    pub cuts: Vec<Cut>,
    /// See [`flatten`]: drawn around the finished picture, never part of it.
    #[serde(default)]
    pub backdrop: Option<Backdrop>,
    /// See [`STATE_VERSION`]; missing (0) in sidecars written before it.
    #[serde(default)]
    pub version: u32,
}

impl Default for EditState {
    fn default() -> Self {
        Self { crop: None, marks: Vec::new(), cuts: Vec::new(), backdrop: None, version: STATE_VERSION }
    }
}

impl EditState {
    /// A point on the cut picture, on the whole picture.
    pub fn cut_to_picture(&self, p: (f64, f64)) -> (f64, f64) {
        self.cuts.iter().rev().fold(p, |p, c| c.map(p, Cut::back))
    }

    /// A point on the whole picture, on the cut picture.
    pub fn picture_to_cut(&self, p: (f64, f64)) -> (f64, f64) {
        self.cuts.iter().fold(p, |p, c| c.map(p, Cut::forward))
    }

    /// The cut picture's size, for a `w`×`h` picture.
    pub fn cut_size(&self, w: f64, h: f64) -> (f64, f64) {
        self.cuts.iter().fold((w, h), |(w, h), c| match c.axis {
            Axis::Rows => (w, h * (1.0 - c.span())),
            Axis::Cols => (w * (1.0 - c.span()), h),
        })
    }

    /// Take out the band from `a` to `b` (on the cut picture, before the crop). The
    /// crop moves with what it showed.
    pub fn add_cut(&mut self, axis: Axis, a: f64, b: f64) {
        let (from, to) = (a.min(b).clamp(0.0, 1.0), a.max(b).clamp(0.0, 1.0));
        if to - from < 0.002 {
            return;
        }
        let cut = Cut { axis, from, to };
        if let Some(c) = self.crop {
            let (x0, y0) = cut.map((c.x, c.y), Cut::forward);
            let (x1, y1) = cut.map((c.right(), c.bottom()), Cut::forward);
            self.crop = Some(NormRect::from_corners((x0, y0), (x1, y1))).filter(|r| r.w > 0.001 && r.h > 0.001 && !r.is_full());
        }
        self.cuts.push(cut);
    }
    pub fn next_callout(&self) -> u32 {
        self.marks
            .iter()
            .filter_map(|m| match m {
                Mark::Callout { n, .. } => Some(*n),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            + 1
    }

    pub fn set_crop(&mut self, crop: Option<NormRect>) {
        self.crop = crop.filter(|c| !c.is_full());
    }

    /// The state in the current version's terms, for a `w`×`h` picture: an older
    /// sidecar's marks are moved from the cropped picture onto the whole one.
    pub fn upgraded(mut self, w: f64, h: f64) -> Self {
        if self.version < STATE_VERSION {
            if let Some(crop) = self.crop {
                self.marks = self.marks.iter().map(|m| m.uncropped(crop, w, h)).collect();
            }
            self.version = STATE_VERSION;
        }
        self
    }

    pub fn is_edited(&self) -> bool {
        self.crop.is_some() || !self.marks.is_empty() || !self.cuts.is_empty() || self.backdrop.is_some()
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Ok(Self::from_json(&std::fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        crate::atomic_write(path, self.to_json()?.as_bytes())
    }
}

/// Undo and redo: a snapshot after each finished action.
pub struct History {
    states: Vec<EditState>,
    at: usize,
}

impl Default for History {
    fn default() -> Self {
        Self { states: vec![EditState::default()], at: 0 }
    }
}

impl History {
    pub fn from_state(state: EditState) -> Self {
        Self { states: vec![state], at: 0 }
    }

    pub fn current(&self) -> &EditState {
        &self.states[self.at]
    }

    pub fn push(&mut self, state: EditState) {
        if state == *self.current() {
            return;
        }
        self.states.truncate(self.at + 1);
        self.states.push(state);
        self.at += 1;
    }

    /// Change the current step instead of adding one (a slider still being dragged).
    /// Anything after it to redo is dropped.
    pub fn amend(&mut self, state: EditState) {
        self.states.truncate(self.at + 1);
        self.states[self.at] = state;
    }

    pub fn can_undo(&self) -> bool {
        self.at > 0
    }

    pub fn can_redo(&self) -> bool {
        self.at + 1 < self.states.len()
    }

    pub fn undo(&mut self) -> &EditState {
        self.at = self.at.saturating_sub(1);
        self.current()
    }

    pub fn redo(&mut self) -> &EditState {
        if self.can_redo() {
            self.at += 1;
        }
        self.current()
    }
}

pub fn sidecar_path(png: &Path) -> std::path::PathBuf {
    png.with_extension("ann.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_drawn_on_a_crop_land_on_the_whole_picture() {
        let crop = NormRect::new(0.5, 0.0, 0.5, 1.0);
        let view = Mark::Rect { rect: NormRect::new(0.0, 0.5, 1.0, 0.5), colour: Colour::rgb(1.0, 0.0, 0.0), size: 0.1, fill: false, radius: 0.0 };
        let Mark::Rect { rect, size, .. } = view.uncropped(crop, 200.0, 100.0) else { unreachable!() };
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (0.5, 0.5, 0.5, 0.5));
        // The view's shorter side is 100 of the picture's 100: same size.
        assert!((size - 0.1).abs() < 1e-9);
        let moved = view.translated(0.1, 0.0);
        assert!((moved.bbox(0.0).x - 0.1).abs() < 1e-9);
    }

    #[test]
    fn old_sidecars_keep_their_look_and_upgrade() {
        let old = EditState::from_json(r#"{"crop":{"x":0.5,"y":0.0,"w":0.5,"h":1.0},"marks":[{"kind":"fill","rect":{"x":0.0,"y":0.0,"w":0.5,"h":1.0},"colour":{"r":1.0,"g":0.0,"b":0.0}}]}"#).unwrap();
        assert_eq!(old.version, 0);
        let src = image::RgbaImage::from_pixel(8, 4, image::Rgba([0, 0, 0, 255]));
        // Old: the fill covers the left half of the cropped (right) half.
        let a = render(&src, &old);
        assert_eq!(a.dimensions(), (4, 4));
        assert_eq!(a.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(a.get_pixel(3, 0).0, [0, 0, 0, 255]);
        let new = old.upgraded(8.0, 4.0);
        assert_eq!(new.version, STATE_VERSION);
        assert_eq!(render(&src, &new), a);
    }

    #[test]
    fn sidecars_from_before_styles_still_load() {
        let old = EditState::from_json(
            r#"{"crop":null,"version":1,"marks":[
                {"kind":"rect","rect":{"x":0.1,"y":0.1,"w":0.2,"h":0.2},"colour":{"r":1.0,"g":0.0,"b":0.0},"size":0.01},
                {"kind":"ellipse","rect":{"x":0.1,"y":0.1,"w":0.2,"h":0.2},"colour":{"r":1.0,"g":0.0,"b":0.0},"size":0.01},
                {"kind":"arrow","from":[0.1,0.1],"to":[0.5,0.5],"colour":{"r":1.0,"g":0.0,"b":0.0},"size":0.01}]}"#,
        )
        .unwrap();
        assert!(matches!(old.marks[0], Mark::Rect { fill: false, radius, .. } if radius == 0.0));
        assert!(matches!(old.marks[1], Mark::Ellipse { fill: false, .. }));
        assert!(matches!(old.marks[2], Mark::Arrow { style: ArrowStyle::Standard, .. }));
        let spot = EditState::from_json(r#"{"crop":null,"version":1,"marks":[{"kind":"spotlight","rect":{"x":0.1,"y":0.1,"w":0.2,"h":0.2}}]}"#).unwrap();
        assert!(matches!(spot.marks[0], Mark::Spotlight { shape: SpotShape::Ellipse, zoom, .. } if zoom == 1.0));
    }

    #[test]
    fn points_map_through_cuts_and_back() {
        let mut st = EditState::default();
        // Take out the middle fifth of the rows, then a tenth of the columns.
        st.add_cut(Axis::Rows, 0.4, 0.6);
        st.add_cut(Axis::Cols, 0.0, 0.1);
        assert_eq!(st.cut_size(100.0, 100.0), (90.0, 80.0));
        for p in [(0.5, 0.2), (0.3, 0.7), (0.95, 0.99)] {
            let there = st.picture_to_cut(p);
            let back = st.cut_to_picture(there);
            assert!((back.0 - p.0).abs() < 1e-9 && (back.1 - p.1).abs() < 1e-9, "{p:?} → {there:?} → {back:?}");
        }
        // Below the band moves up by it: 0.7 of 100 rows is row 50 of 80.
        let (_, y) = st.picture_to_cut((0.5, 0.7));
        assert!((y - 0.625).abs() < 1e-9);
        // Inside the band is the seam.
        let (_, y) = st.picture_to_cut((0.5, 0.5));
        assert!((y - 0.5).abs() < 1e-9);
        // A mark drawn on the cut picture lands on the whole one.
        let m = Mark::Callout { at: (0.5, 0.625), n: 1, colour: Colour::rgb(1.0, 0.0, 0.0) }.uncut(&st);
        let Mark::Callout { at, .. } = m else { unreachable!() };
        assert!((at.1 - 0.7).abs() < 1e-9, "{at:?}");
    }

    #[test]
    fn a_cut_keeps_the_crop_on_what_it_showed() {
        let mut st = EditState::default();
        st.set_crop(Some(NormRect::new(0.0, 0.6, 1.0, 0.4)));
        st.add_cut(Axis::Rows, 0.2, 0.4);
        let c = st.crop.unwrap();
        assert!((c.y - 0.5).abs() < 1e-9 && (c.h - 0.5).abs() < 1e-9, "{c:?}");
        assert!(st.is_edited());
    }

    #[test]
    fn json_round_trip_and_callout_numbers() {
        let mut st = EditState::default();
        assert_eq!(st.next_callout(), 1);
        st.marks.push(Mark::Callout { at: (0.2, 0.3), n: 1, colour: Colour::rgb(1.0, 0.0, 0.0) });
        assert_eq!(st.next_callout(), 2);
        let json = st.to_json().unwrap();
        let back = EditState::from_json(&json).unwrap();
        assert_eq!(back, st);
    }

    #[test]
    fn history_undoes_and_drops_the_future() {
        let mut h = History::default();
        let mut a = EditState::default();
        a.marks.push(Mark::Line { from: (0.0, 0.0), to: (1.0, 1.0), colour: Colour::rgb(1.0, 0.0, 0.0), size: 0.01 });
        h.push(a.clone());
        let mut b = a.clone();
        b.crop = Some(NormRect::new(0.1, 0.1, 0.5, 0.5));
        h.push(b);
        assert!(h.current().crop.is_some());
        assert!(h.undo().crop.is_none());
        assert!(h.can_redo());
        h.redo();
        h.undo();
        let mut c = a.clone();
        c.marks.clear();
        h.push(c);
        assert!(!h.can_redo());
    }

    #[test]
    fn amending_changes_the_step_without_adding_one() {
        let mut h = History::default();
        let mut a = EditState::default();
        a.backdrop = Some(Backdrop::default());
        h.push(a.clone());
        a.backdrop = Some(Backdrop { padding: 0.1, ..Backdrop::default() });
        h.amend(a.clone());
        assert_eq!(h.current().backdrop.unwrap().padding, 0.1);
        assert!(h.current().is_edited());
        assert!(h.undo().backdrop.is_none());
        assert!(!h.can_undo());
    }

    #[test]
    fn sidecar_sits_beside_the_png() {
        assert_eq!(sidecar_path(Path::new("/tmp/abc.png")), Path::new("/tmp/abc.ann.json"));
    }
}
