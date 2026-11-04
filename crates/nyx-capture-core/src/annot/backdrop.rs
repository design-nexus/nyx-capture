//! A backdrop behind the finished picture: a mat of colour around it, its corners
//! rounded, and a drop shadow. Its colours are its own, not the theme's: they go out
//! with the image.

use cairo::{Context, Format, ImageSurface, LinearGradient};
use image::RgbaImage;
use image::imageops::{self, FilterType};
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fill {
    #[default]
    Grey,
    Slate,
    Aurora,
    Sunset,
    Lagoon,
    Violet,
}

impl Fill {
    pub const ALL: [Fill; 6] = [Fill::Grey, Fill::Slate, Fill::Aurora, Fill::Sunset, Fill::Lagoon, Fill::Violet];

    pub fn name(self) -> &'static str {
        match self {
            Fill::Grey => "Grey",
            Fill::Slate => "Slate",
            Fill::Aurora => "Aurora",
            Fill::Sunset => "Sunset",
            Fill::Lagoon => "Lagoon",
            Fill::Violet => "Violet",
        }
    }

    /// Its colour stops, top-left to bottom-right.
    pub fn stops(self) -> &'static [&'static str] {
        match self {
            Fill::Grey => &["#d4d4d8", "#c4c4ca"],
            Fill::Slate => &["#475569", "#0f172a"],
            Fill::Aurora => &["#22d3ee", "#6366f1", "#a855f7"],
            Fill::Sunset => &["#fb923c", "#ec4899"],
            Fill::Lagoon => &["#2dd4bf", "#0284c7"],
            Fill::Violet => &["#a78bfa", "#c026d3"],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Backdrop {
    pub fill: Fill,
    /// The mat around the picture, a share of its shorter side.
    pub padding: f64,
    /// The picture's corner radius, a share of its shorter side.
    pub radius: f64,
    pub shadow: bool,
}

impl Default for Backdrop {
    fn default() -> Self {
        Self { fill: Fill::Grey, padding: 0.06, radius: 0.012, shadow: true }
    }
}

impl Backdrop {
    /// The mat's width in pixels around a `w`×`h` picture.
    pub fn inset(&self, w: u32, h: u32) -> u32 {
        (self.padding.clamp(0.0, 0.5) * w.min(h) as f64).round() as u32
    }
}

fn hex(s: &str) -> (f64, f64, f64) {
    let v = u32::from_str_radix(s.trim_start_matches('#'), 16).unwrap_or(0);
    (((v >> 16) & 0xff) as f64 / 255.0, ((v >> 8) & 0xff) as f64 / 255.0, (v & 0xff) as f64 / 255.0)
}

fn rounded(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
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

/// `img` on its backdrop.
pub fn compose(img: &RgbaImage, b: &Backdrop) -> RgbaImage {
    let (w, h) = img.dimensions();
    let pad = b.inset(w, h);
    let (ow, oh) = (w + pad * 2, h + pad * 2);
    let unit = w.min(h) as f64;
    let radius = b.radius.clamp(0.0, 0.5) * unit;
    let Ok(mut surface) = ImageSurface::create(Format::ARgb32, ow as i32, oh as i32) else { return img.clone() };
    {
        let Ok(cr) = Context::new(&surface) else { return img.clone() };
        let stops = b.fill.stops();
        let grad = LinearGradient::new(0.0, 0.0, ow as f64, oh as f64);
        for (i, s) in stops.iter().enumerate() {
            let (r, g, bl) = hex(s);
            grad.add_color_stop_rgb(i as f64 / (stops.len() - 1).max(1) as f64, r, g, bl);
        }
        let _ = cr.set_source(&grad);
        let _ = cr.paint();
    }
    let mut out = super::raster::image_from_surface(&mut surface, ow, oh);
    if b.shadow && pad > 0 {
        shadow(&mut out, pad, w, h, radius, unit);
    }
    // The picture, its corners rounded, over the mat and shadow.
    let mut card = img.clone();
    if radius >= 0.5 {
        round_corners(&mut card, radius);
    }
    imageops::overlay(&mut out, &card, pad as i64, pad as i64);
    out
}

/// A soft shadow under where the picture goes, drawn small, blurred, then enlarged so
/// a large picture stays quick.
fn shadow(out: &mut RgbaImage, pad: u32, w: u32, h: u32, radius: f64, unit: f64) {
    let (ow, oh) = out.dimensions();
    let k = 4u32;
    let (sw, sh) = ((ow / k).max(1), (oh / k).max(1));
    let Ok(surface) = ImageSurface::create(Format::ARgb32, sw as i32, sh as i32) else { return };
    {
        let Ok(cr) = Context::new(&surface) else { return };
        let s = 1.0 / k as f64;
        let drop = unit * 0.012;
        rounded(&cr, pad as f64 * s, (pad as f64 + drop) * s, w as f64 * s, h as f64 * s, radius * s);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);
        let _ = cr.fill();
    }
    let mut surface = surface;
    let small = super::raster::image_from_surface(&mut surface, sw, sh);
    let sigma = ((unit * 0.025) / k as f64).max(1.0) as f32;
    let blurred = imageops::resize(&imageops::blur(&small, sigma), ow, oh, FilterType::Triangle);
    imageops::overlay(out, &blurred, 0, 0);
}

/// Clear the picture's corners outside a `r`-pixel rounding.
fn round_corners(img: &mut RgbaImage, r: f64) {
    let (w, h) = img.dimensions();
    let r = r.min(w as f64 / 2.0).min(h as f64 / 2.0);
    let ri = r.ceil() as u32;
    for y in 0..ri.min(h) {
        for x in 0..ri.min(w) {
            let (dx, dy) = (r - (x as f64 + 0.5), r - (y as f64 + 0.5));
            let d = (dx * dx + dy * dy).sqrt();
            // Coverage of the pixel: 1 inside the arc, falling off across its edge.
            let cover = (r - d + 0.5).clamp(0.0, 1.0);
            if dx <= 0.0 || dy <= 0.0 || cover >= 1.0 {
                continue;
            }
            for (px, py) in [(x, y), (w - 1 - x, y), (x, h - 1 - y), (w - 1 - x, h - 1 - y)] {
                let p = img.get_pixel_mut(px, py);
                p.0[3] = (p.0[3] as f64 * cover).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn the_mat_grows_the_picture_and_keeps_it_whole() {
        let img = RgbaImage::from_pixel(200, 100, Rgba([255, 0, 0, 255]));
        let b = Backdrop { fill: Fill::Slate, padding: 0.1, radius: 0.0, shadow: false };
        let out = compose(&img, &b);
        assert_eq!(out.dimensions(), (220, 120));
        assert_eq!(out.get_pixel(110, 60).0, [255, 0, 0, 255]);
        assert_eq!(out.get_pixel(10, 10).0, [255, 0, 0, 255], "the picture's own corner, unrounded");
        assert_ne!(out.get_pixel(2, 2).0, [255, 0, 0, 255], "the mat");
    }

    #[test]
    fn rounded_corners_show_the_mat_and_the_shadow_falls_below() {
        let img = RgbaImage::from_pixel(200, 200, Rgba([255, 255, 255, 255]));
        let b = Backdrop { fill: Fill::Grey, padding: 0.1, radius: 0.1, shadow: true };
        let out = compose(&img, &b);
        assert_eq!(out.dimensions(), (240, 240));
        // The picture's very corner is the mat now.
        assert_ne!(out.get_pixel(20, 20).0, [255, 255, 255, 255]);
        // Below the picture is darker than above it: the shadow drops.
        let above = out.get_pixel(120, 8).0[0];
        let below = out.get_pixel(120, 231).0[0];
        assert!(below < above, "{below} < {above}");
    }
}
