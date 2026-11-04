//! Flatten an `EditState` onto pixels: blur/pixelate, then cairo marks, then the crop
//! (marks are in the whole picture's terms; see `STATE_VERSION`).

use super::{ArrowStyle, Axis, Colour, Cut, EditState, Mark, NormRect, SpotShape};
use cairo::{self, Context, Format, ImageSurface};
use image::imageops::{self, FilterType};
use image::RgbaImage;
use std::f64::consts::PI;

const HIGHLIGHTER_WIDTH: f64 = 4.0;
const HIGHLIGHTER_ALPHA: f64 = 0.4;

/// Apply `st` to a copy of `src`. `src` is never written.
pub fn render(src: &RgbaImage, st: &EditState) -> RgbaImage {
    // Older sidecars put their marks on the cropped picture.
    let crop_first = st.version < super::STATE_VERSION;
    let mut img = match st.crop {
        Some(c) if crop_first => crop(src, c),
        _ => src.clone(),
    };
    for m in &st.marks {
        match m {
            Mark::Pixelate { rect, size } => pixelate(&mut img, *rect, *size),
            Mark::Blur { rect, size } => blur(&mut img, *rect, *size),
            _ => {}
        }
    }
    spotlights(&mut img, &st.marks);
    draw_on_image(&mut img, &st.marks);
    if !crop_first {
        for c in &st.cuts {
            img = cut(&img, *c);
        }
    }
    match st.crop {
        Some(c) if !crop_first => crop(&img, c),
        _ => img,
    }
}

/// The weakest a blur or pixelate may be, as a share of the picture's shorter side:
/// gentler ones leave text that can be read back.
const MIN_OBSCURE: f64 = 1.0 / 60.0;

fn crop(img: &RgbaImage, c: NormRect) -> RgbaImage {
    let (w, h) = img.dimensions();
    let x = ((c.x * w as f64).round() as u32).min(w.saturating_sub(1));
    let y = ((c.y * h as f64).round() as u32).min(h.saturating_sub(1));
    let cw = ((c.w * w as f64).round() as u32).clamp(1, w - x);
    let ch = ((c.h * h as f64).round() as u32).clamp(1, h - y);
    imageops::crop_imm(img, x, y, cw, ch).to_image()
}

/// The picture without the cut's band, the two sides closed up.
fn cut(img: &RgbaImage, c: Cut) -> RgbaImage {
    let (w, h) = img.dimensions();
    let len = match c.axis {
        Axis::Rows => h,
        Axis::Cols => w,
    };
    let a = ((c.from * len as f64).round() as u32).min(len);
    let gone = (((c.to - c.from).clamp(0.0, 0.95) * len as f64).round() as u32).min(len - a).min(len.saturating_sub(1));
    if gone == 0 {
        return img.clone();
    }
    let keep = |v: u32| if v < a { v } else { v + gone };
    match c.axis {
        Axis::Rows => RgbaImage::from_fn(w, h - gone, |x, y| *img.get_pixel(x, keep(y))),
        Axis::Cols => RgbaImage::from_fn(w - gone, h, |x, y| *img.get_pixel(keep(x), y)),
    }
}

fn pixelate(img: &mut RgbaImage, rect: NormRect, size: f64) {
    let (w, h) = img.dimensions();
    let (x0, y0, x1, y1) = pixel_box(rect, w, h);
    let unit = w.min(h) as f64;
    let block = ((size.max(MIN_OBSCURE) * unit).round() as u32).max(6);
    let mut by = y0;
    while by < y1 {
        let bh = block.min(y1 - by);
        let mut bx = x0;
        while bx < x1 {
            let bw = block.min(x1 - bx);
            let mut sum = [0u64; 4];
            for y in by..by + bh {
                for x in bx..bx + bw {
                    for (s, c) in sum.iter_mut().zip(img.get_pixel(x, y).0) {
                        *s += c as u64;
                    }
                }
            }
            let n = (bw * bh) as u64;
            let avg = [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, (sum[3] / n) as u8];
            for y in by..by + bh {
                for x in bx..bx + bw {
                    img.get_pixel_mut(x, y).0 = avg;
                }
            }
            bx += bw;
        }
        by += bh;
    }
}

fn blur(img: &mut RgbaImage, rect: NormRect, size: f64) {
    let (w, h) = img.dimensions();
    let (x0, y0, x1, y1) = pixel_box(rect, w, h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let bw = x1 - x0;
    let bh = y1 - y0;
    let sub = imageops::crop_imm(img, x0, y0, bw, bh).to_image();
    let sigma = (size.max(MIN_OBSCURE) as f32 * w.min(h) as f32).clamp(4.0, 48.0);
    let blurred = imageops::blur(&sub, sigma);
    for y in 0..bh {
        for x in 0..bw {
            img.put_pixel(x0 + x, y0 + y, *blurred.get_pixel(x, y));
        }
    }
}

fn pixel_box(rect: NormRect, w: u32, h: u32) -> (u32, u32, u32, u32) {
    let x0 = (rect.x * w as f64).round().clamp(0.0, w as f64) as u32;
    let y0 = (rect.y * h as f64).round().clamp(0.0, h as f64) as u32;
    let x1 = (rect.right() * w as f64).round().clamp(0.0, w as f64) as u32;
    let y1 = (rect.bottom() * h as f64).round().clamp(0.0, h as f64) as u32;
    (x0, y0, x1.max(x0), y1.max(y0))
}

pub fn draw_marks(cr: &Context, w: f64, h: f64, marks: &[Mark]) {
    let unit = w.min(h);
    for mark in marks {
        let _ = cr.save();
        draw_one(cr, w, h, unit, mark);
        let _ = cr.restore();
    }
}

fn set_colour(cr: &Context, c: Colour, alpha: f64) {
    cr.set_source_rgba(c.r, c.g, c.b, alpha);
}

fn draw_one(cr: &Context, w: f64, h: f64, unit: f64, mark: &Mark) {
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    match mark {
        Mark::Stroke { points, colour, size, highlighter } => {
            let Some(first) = points.first() else { return };
            if *highlighter {
                set_colour(cr, *colour, HIGHLIGHTER_ALPHA);
                cr.set_line_width(size * unit * HIGHLIGHTER_WIDTH);
                cr.set_line_cap(cairo::LineCap::Butt);
            } else {
                set_colour(cr, *colour, 1.0);
                cr.set_line_width(size * unit);
            }
            cr.move_to(first.0 * w, first.1 * h);
            cr.line_to(first.0 * w, first.1 * h);
            for p in &points[1..] {
                cr.line_to(p.0 * w, p.1 * h);
            }
            let _ = cr.stroke();
        }
        Mark::Line { from, to, colour, size } => {
            set_colour(cr, *colour, 1.0);
            cr.set_line_width(size * unit);
            cr.move_to(from.0 * w, from.1 * h);
            cr.line_to(to.0 * w, to.1 * h);
            let _ = cr.stroke();
        }
        Mark::Arrow { from, to, colour, size, style } => {
            set_colour(cr, *colour, 1.0);
            draw_arrow(cr, (from.0 * w, from.1 * h), (to.0 * w, to.1 * h), size * unit, unit, *style);
        }
        Mark::Rect { rect, colour, size, fill, radius } => {
            let r = rect.scaled(w, h);
            set_colour(cr, *colour, 1.0);
            rounded_rect(cr, r.x, r.y, r.w, r.h, radius * unit);
            if *fill {
                let _ = cr.fill();
            } else {
                cr.set_line_width(size * unit);
                let _ = cr.stroke();
            }
        }
        Mark::Ellipse { rect, colour, size, fill } => {
            let r = rect.scaled(w, h);
            if r.w < 0.5 || r.h < 0.5 {
                return;
            }
            set_colour(cr, *colour, 1.0);
            ellipse_path(cr, r.x, r.y, r.w, r.h);
            if *fill {
                let _ = cr.fill();
            } else {
                cr.set_line_width(size * unit);
                let _ = cr.stroke();
            }
        }
        Mark::Text { at, text, colour, size } => {
            if text.is_empty() {
                return;
            }
            paint_label(cr, at.0 * w, at.1 * h, text, *colour, (size * unit).max(4.0));
        }
        Mark::Callout { at, n, colour } => {
            let cx = at.0 * w;
            let cy = at.1 * h;
            let r = (unit * 0.028).max(8.0);
            set_colour(cr, *colour, 1.0);
            cr.arc(cx, cy, r, 0.0, 2.0 * PI);
            let _ = cr.fill_preserve();
            set_colour(cr, colour.contrast(), 0.85);
            cr.set_line_width(1.5);
            let _ = cr.stroke();
            paint_label(cr, cx - r * 0.45, cy - r * 0.55, &n.to_string(), colour.contrast(), r * 0.95);
        }
        Mark::Fill { rect, colour } => {
            let r = rect.scaled(w, h);
            set_colour(cr, *colour, 1.0);
            cr.rectangle(r.x, r.y, r.w, r.h);
            let _ = cr.fill();
        }
        // Drawn on the pixels before the marks (see `spotlights`).
        Mark::Pixelate { .. } | Mark::Blur { .. } | Mark::Spotlight { .. } => {}
    }
}

/// An arrow from `a` to `b` with a shaft `width` wide, in the current colour.
fn draw_arrow(cr: &Context, a: (f64, f64), b: (f64, f64), width: f64, unit: f64, style: ArrowStyle) {
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    if len < 0.5 {
        return;
    }
    let head = width * 4.0 + unit * 0.01;
    let back = (head * 0.8).min(len * 0.6);
    let wing = 25f64.to_radians();
    let tip = |cr: &Context, at: (f64, f64), angle: f64| {
        cr.move_to(at.0, at.1);
        cr.line_to(at.0 - head * (angle - wing).cos(), at.1 - head * (angle - wing).sin());
        cr.line_to(at.0 - head * (angle + wing).cos(), at.1 - head * (angle + wing).sin());
        cr.close_path();
        let _ = cr.fill();
    };
    let angle = (b.1 - a.1).atan2(b.0 - a.0);
    let (ux, uy) = (angle.cos(), angle.sin());
    let base = (b.0 - back * ux, b.1 - back * uy);
    cr.set_line_width(width);
    match style {
        ArrowStyle::Standard => {
            cr.move_to(a.0, a.1);
            cr.line_to(base.0, base.1);
            let _ = cr.stroke();
            tip(cr, b, angle);
        }
        ArrowStyle::Double => {
            let start = (a.0 + back * ux, a.1 + back * uy);
            cr.move_to(start.0, start.1);
            cr.line_to(base.0, base.1);
            let _ = cr.stroke();
            tip(cr, b, angle);
            tip(cr, a, angle + PI);
        }
        ArrowStyle::Pointy => {
            // A shaft that widens from a point at the tail to the head.
            let (nx, ny) = (-uy, ux);
            let half = width * 0.75;
            cr.move_to(a.0, a.1);
            cr.line_to(base.0 + nx * half, base.1 + ny * half);
            cr.line_to(base.0 - nx * half, base.1 - ny * half);
            cr.close_path();
            let _ = cr.fill();
            tip(cr, b, angle);
        }
        ArrowStyle::Curved => {
            // A quadratic bent a quarter of its length to the left, cut where the head
            // begins so the shaft doesn't show past it.
            let (nx, ny) = (uy, -ux);
            let c = ((a.0 + b.0) / 2.0 + nx * len * 0.25, (a.1 + b.1) / 2.0 + ny * len * 0.25);
            let t = (1.0 - back / (len * 1.08)).clamp(0.3, 1.0);
            let lerp = |p: (f64, f64), q: (f64, f64), t: f64| (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t);
            let q1 = lerp(a, c, t);
            let end = lerp(q1, lerp(c, b, t), t);
            let c1 = lerp(a, q1, 2.0 / 3.0);
            let c2 = lerp(end, q1, 2.0 / 3.0);
            cr.set_line_cap(cairo::LineCap::Butt);
            cr.move_to(a.0, a.1);
            cr.curve_to(c1.0, c1.1, c2.0, c2.1, end.0, end.1);
            let _ = cr.stroke();
            tip(cr, b, (b.1 - c.1).atan2(b.0 - c.0));
        }
    }
}

fn rounded_rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.max(0.0).min(w / 2.0).min(h / 2.0);
    if r < 0.5 {
        cr.rectangle(x, y, w, h);
        return;
    }
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

fn ellipse_path(cr: &Context, x: f64, y: f64, w: f64, h: f64) {
    cr.save().ok();
    cr.translate(x + w / 2.0, y + h / 2.0);
    cr.scale((w / 2.0).max(0.01), (h / 2.0).max(0.01));
    cr.new_sub_path();
    cr.arc(0.0, 0.0, 1.0, 0.0, 2.0 * PI);
    cr.restore().ok();
}

/// A spotlight's outline (pixels), as a path.
fn spot_path(cr: &Context, r: NormRect, shape: SpotShape, unit: f64) {
    match shape {
        SpotShape::Ellipse => ellipse_path(cr, r.x, r.y, r.w, r.h),
        SpotShape::Rect => cr.rectangle(r.x, r.y, r.w, r.h),
        SpotShape::Rounded => rounded_rect(cr, r.x, r.y, r.w, r.h, (unit * 0.03).min(r.w.min(r.h) / 3.0)),
    }
}

/// How dark a spotlight leaves the rest of the picture.
const SPOT_DIM: f64 = 0.5;

/// The editor's preview of a spotlight being drawn: the dimming only.
pub fn draw_spotlight_preview(cr: &Context, w: f64, h: f64, rect: NormRect, shape: SpotShape) {
    cr.save().ok();
    cr.set_fill_rule(cairo::FillRule::EvenOdd);
    cr.rectangle(0.0, 0.0, w, h);
    spot_path(cr, rect.scaled(w, h), shape, w.min(h));
    cr.set_source_rgba(0.0, 0.0, 0.0, SPOT_DIM);
    let _ = cr.fill();
    cr.restore().ok();
}

/// Dim everything outside the spotlights, and magnify inside those with a zoom.
fn spotlights(img: &mut RgbaImage, marks: &[Mark]) {
    let spots: Vec<(NormRect, SpotShape, f64)> = marks
        .iter()
        .filter_map(|m| match m {
            Mark::Spotlight { rect, shape, zoom } => Some((*rect, *shape, *zoom)),
            _ => None,
        })
        .collect();
    if spots.is_empty() {
        return;
    }
    let (w, h) = (img.width() as f64, img.height() as f64);
    let unit = w.min(h);
    let (Some(source), Some(mut surface)) = (surface_from_image(img), surface_from_image(img)) else { return };
    {
        let Ok(cr) = Context::new(&surface) else { return };
        cr.set_fill_rule(cairo::FillRule::EvenOdd);
        cr.rectangle(0.0, 0.0, w, h);
        for (rect, shape, _) in &spots {
            spot_path(&cr, rect.scaled(w, h), *shape, unit);
        }
        cr.set_source_rgba(0.0, 0.0, 0.0, SPOT_DIM);
        let _ = cr.fill();
        cr.set_fill_rule(cairo::FillRule::Winding);
        for (rect, shape, zoom) in spots.iter().filter(|s| s.2 > 1.01) {
            let r = rect.scaled(w, h);
            let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
            cr.save().ok();
            spot_path(&cr, r, *shape, unit);
            cr.clip();
            cr.translate(cx, cy);
            cr.scale(*zoom, *zoom);
            cr.translate(-cx, -cy);
            if cr.set_source_surface(&source, 0.0, 0.0).is_ok() {
                cr.source().set_filter(cairo::Filter::Good);
                let _ = cr.paint();
            }
            cr.restore().ok();
            spot_path(&cr, r, *shape, unit);
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
            cr.set_line_width((unit * 0.004).max(1.5));
            let _ = cr.stroke();
        }
    }
    copy_surface_into(&mut surface, img);
}

fn paint_label(cr: &Context, x: f64, y: f64, text: &str, colour: Colour, px: f64) {
    let layout = pangocairo::functions::create_layout(cr);
    let mut font = pango::FontDescription::new();
    font.set_family("Sans");
    font.set_weight(pango::Weight::Bold);
    font.set_absolute_size(px * pango::SCALE as f64);
    layout.set_font_description(Some(&font));
    layout.set_text(text);
    cr.move_to(x, y);
    pangocairo::functions::layout_path(cr, &layout);
    set_colour(cr, colour.contrast(), 0.6);
    cr.set_line_width(px * 0.14);
    let _ = cr.stroke_preserve();
    set_colour(cr, colour, 1.0);
    let _ = cr.fill();
}

fn surface_from_image(img: &RgbaImage) -> Option<ImageSurface> {
    let (w, h) = img.dimensions();
    let mut surface = ImageSurface::create(Format::ARgb32, w as i32, h as i32).ok()?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().ok()?;
        for (y, row) in img.rows().enumerate() {
            for (x, p) in row.enumerate() {
                data[y * stride + x * 4..][..4].copy_from_slice(&premultiply(p.0).to_ne_bytes());
            }
        }
    }
    Some(surface)
}

/// A `w`×`h` surface's pixels as an image.
pub(super) fn image_from_surface(surface: &mut ImageSurface, w: u32, h: u32) -> RgbaImage {
    let mut img = RgbaImage::new(w, h);
    copy_surface_into(surface, &mut img);
    img
}

fn copy_surface_into(surface: &mut ImageSurface, img: &mut RgbaImage) {
    surface.flush();
    let stride = surface.stride() as usize;
    if let Ok(data) = surface.data() {
        for (y, row) in img.rows_mut().enumerate() {
            for (x, p) in row.enumerate() {
                let bytes: [u8; 4] = data[y * stride + x * 4..][..4].try_into().unwrap();
                p.0 = unpremultiply(u32::from_ne_bytes(bytes));
            }
        }
    }
}

fn draw_on_image(img: &mut RgbaImage, marks: &[Mark]) {
    if marks.iter().all(|m| matches!(m, Mark::Pixelate { .. } | Mark::Blur { .. } | Mark::Spotlight { .. })) {
        return;
    }
    let (w, h) = img.dimensions();
    let Some(mut surface) = surface_from_image(img) else { return };
    {
        let Ok(cr) = Context::new(&surface) else { return };
        draw_marks(&cr, w as f64, h as f64, marks);
    }
    copy_surface_into(&mut surface, img);
}

fn premultiply([r, g, b, a]: [u8; 4]) -> u32 {
    let m = |c: u8| (c as u32 * a as u32 + 127) / 255;
    ((a as u32) << 24) | (m(r) << 16) | (m(g) << 8) | m(b)
}

fn unpremultiply(px: u32) -> [u8; 4] {
    let a = (px >> 24) & 0xff;
    let (r, g, b) = ((px >> 16) & 0xff, (px >> 8) & 0xff, px & 0xff);
    if a == 0 {
        return [0, 0, 0, 0];
    }
    let un = |c: u32| ((c * 255 + a / 2) / a).min(255) as u8;
    [un(r), un(g), un(b), a as u8]
}

/// Downscale for the live canvas; copy/save still use the full picture.
pub fn preview(src: &RgbaImage, max_edge: u32) -> RgbaImage {
    let (w, h) = src.dimensions();
    if w.max(h) <= max_edge {
        return src.clone();
    }
    let k = max_edge as f64 / w.max(h) as f64;
    imageops::resize(src, ((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1), FilterType::Triangle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annot::Mark;
    use image::Rgba;

    const RED: Colour = Colour::rgb(1.0, 0.0, 0.0);

    fn sample() -> RgbaImage {
        let mut img = RgbaImage::from_pixel(40, 20, Rgba([100, 100, 100, 255]));
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        img.put_pixel(39, 0, Rgba([0, 0, 255, 255]));
        img
    }

    #[test]
    fn render_leaves_the_source_alone() {
        let src = sample();
        let before = src.clone();
        let mut st = EditState::default();
        st.marks.push(Mark::Rect { rect: NormRect::new(0.2, 0.2, 0.5, 0.5), colour: RED, size: 0.04, fill: false, radius: 0.0 });
        let _ = render(&src, &st);
        assert_eq!(src, before);
    }

    #[test]
    fn a_rectangle_is_drawn_where_asked() {
        let mut st = EditState::default();
        st.marks.push(Mark::Rect { rect: NormRect::new(0.2, 0.2, 0.6, 0.6), colour: RED, size: 0.04, fill: false, radius: 0.0 });
        let img = render(&RgbaImage::from_pixel(100, 100, Rgba([10, 20, 30, 255])), &st);
        assert_eq!(img.get_pixel(20, 50).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(50, 50).0, [10, 20, 30, 255]);
        assert_eq!(img.get_pixel(2, 2).0, [10, 20, 30, 255]);
    }

    #[test]
    fn filled_shapes_cover_their_middle() {
        let bg = Rgba([10, 20, 30, 255]);
        let mut st = EditState::default();
        st.marks.push(Mark::Rect { rect: NormRect::new(0.1, 0.1, 0.4, 0.4), colour: RED, size: 0.01, fill: true, radius: 0.1 });
        st.marks.push(Mark::Ellipse { rect: NormRect::new(0.55, 0.55, 0.4, 0.4), colour: RED, size: 0.01, fill: true });
        let img = render(&RgbaImage::from_pixel(100, 100, bg), &st);
        assert_eq!(img.get_pixel(30, 30).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(75, 75).0, [255, 0, 0, 255]);
        // The rounded corner leaves the very corner alone.
        assert_eq!(img.get_pixel(11, 11).0, bg.0);
    }

    #[test]
    fn every_arrow_style_reaches_its_tip() {
        for style in [ArrowStyle::Standard, ArrowStyle::Pointy, ArrowStyle::Curved, ArrowStyle::Double] {
            let mut st = EditState::default();
            st.marks.push(Mark::Arrow { from: (0.1, 0.5), to: (0.9, 0.5), colour: RED, size: 0.02, style });
            let img = render(&RgbaImage::from_pixel(200, 200, Rgba([0, 0, 0, 255])), &st);
            // Behind the tip along the head: a curved arrow arrives at an angle.
            let a: f64 = if style == ArrowStyle::Curved { (40f64).atan2(80.0) } else { 0.0 };
            let (x, y) = ((180.0 - 6.0 * a.cos()).round() as u32, (100.0 - 6.0 * a.sin()).round() as u32);
            assert_eq!(img.get_pixel(x, y).0[0], 255, "{style:?} head");
            let tail = img.get_pixel(44, 100).0[0];
            assert!(tail > 100 || style == ArrowStyle::Curved, "{style:?} tail");
        }
    }

    #[test]
    fn a_spotlight_dims_outside_and_a_loupe_magnifies_inside() {
        let mut src = RgbaImage::from_pixel(100, 100, Rgba([200, 200, 200, 255]));
        // A dark dot at the middle grows under the loupe.
        for y in 48..52 {
            for x in 48..52 {
                src.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let mut st = EditState::default();
        st.marks.push(Mark::Spotlight { rect: NormRect::new(0.3, 0.3, 0.4, 0.4), shape: SpotShape::Rect, zoom: 1.0 });
        let out = render(&src, &st);
        assert!(out.get_pixel(5, 5).0[0] < 120);
        assert_eq!(out.get_pixel(40, 40).0, [200, 200, 200, 255]);
        let mut st = EditState::default();
        st.marks.push(Mark::Spotlight { rect: NormRect::new(0.3, 0.3, 0.4, 0.4), shape: SpotShape::Ellipse, zoom: 3.0 });
        let out = render(&src, &st);
        assert!(out.get_pixel(45, 50).0[0] < 60, "magnified dot");
        assert!(src.get_pixel(45, 50).0[0] > 150);
    }

    #[test]
    fn a_cut_closes_up_the_band_and_takes_marks_along() {
        // Rows 0–9 grey, 10–19 green (to go), 20–39 grey with a red mark at row 30.
        let mut src = RgbaImage::from_pixel(20, 40, Rgba([100, 100, 100, 255]));
        for y in 10..20 {
            for x in 0..20 {
                src.put_pixel(x, y, Rgba([0, 255, 0, 255]));
            }
        }
        let mut st = EditState::default();
        st.marks.push(Mark::Fill { rect: NormRect::new(0.0, 0.75, 1.0, 0.025), colour: RED });
        st.add_cut(Axis::Rows, 0.25, 0.5);
        let out = render(&src, &st);
        assert_eq!(out.dimensions(), (20, 30));
        assert!(out.pixels().all(|p| p.0 != [0, 255, 0, 255]), "the band is gone");
        assert_eq!(out.get_pixel(5, 20).0, [255, 0, 0, 255], "the mark moved up with its rows");
        let mut cols = EditState::default();
        cols.add_cut(Axis::Cols, 0.0, 0.5);
        assert_eq!(render(&src, &cols).dimensions(), (10, 40));
    }

    #[test]
    fn crop_takes_the_asked_part() {
        let mut st = EditState::default();
        st.set_crop(Some(NormRect::new(0.5, 0.0, 0.5, 0.5)));
        let out = render(&sample(), &st);
        assert_eq!(out.dimensions(), (20, 10));
        assert_eq!(out.get_pixel(19, 0).0, [0, 0, 255, 255]);
    }

    #[test]
    fn pixelate_hides_the_region() {
        let mut src = RgbaImage::from_pixel(40, 40, Rgba([10, 10, 10, 255]));
        src.put_pixel(4, 4, Rgba([250, 0, 0, 255]));
        src.put_pixel(5, 5, Rgba([0, 250, 0, 255]));
        let mut st = EditState::default();
        st.marks.push(Mark::Pixelate { rect: NormRect::new(0.0, 0.0, 0.5, 0.5), size: 0.2 });
        let out = render(&src, &st);
        assert_eq!(out.get_pixel(4, 4), out.get_pixel(5, 5));
        assert_eq!(src.get_pixel(4, 4).0, [250, 0, 0, 255]);
    }

    #[test]
    fn a_flattened_copy_leaves_the_original_file_unchanged() {
        let dir = std::env::temp_dir().join("nyx-capture-annot-file");
        let _ = std::fs::create_dir_all(&dir);
        let src_path = dir.join("orig.png");
        sample().save(&src_path).unwrap();
        let before = std::fs::read(&src_path).unwrap();
        let mut st = EditState::default();
        st.marks.push(Mark::Rect { rect: NormRect::new(0.1, 0.1, 0.5, 0.5), colour: RED, size: 0.05, fill: false, radius: 0.0 });
        let src = image::open(&src_path).unwrap().to_rgba8();
        let out = render(&src, &st);
        let dest = dir.join("flat.png");
        out.save(&dest).unwrap();
        assert_eq!(std::fs::read(&src_path).unwrap(), before);
        assert_ne!(std::fs::read(&dest).unwrap(), before);
    }
}
