//! A pixel of a shot, written as HEX, RGB or HSL.

use crate::capture::Shot;
use image::GenericImageView;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    #[default]
    Hex,
    Rgb,
    Hsl,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Hex, Format::Rgb, Format::Hsl];

    pub fn parse(s: &str) -> Format {
        match s {
            "rgb" => Format::Rgb,
            "hsl" => Format::Hsl,
            _ => Format::Hex,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Format::Hex => "hex",
            Format::Rgb => "rgb",
            Format::Hsl => "hsl",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Hex => "HEX",
            Format::Rgb => "RGB",
            Format::Hsl => "HSL",
        }
    }
}

/// `rgb` as `#1e1e2e`, `rgb(30, 30, 46)` or `hsl(240, 21%, 15%)`.
pub fn format(rgb: [u8; 3], fmt: Format) -> String {
    let [r, g, b] = rgb;
    match fmt {
        Format::Hex => format!("#{r:02x}{g:02x}{b:02x}"),
        Format::Rgb => format!("rgb({r}, {g}, {b})"),
        Format::Hsl => {
            let (h, s, l) = hsl(rgb);
            format!("hsl({}, {}%, {}%)", h.round() as i32 % 360, (s * 100.0).round() as i32, (l * 100.0).round() as i32)
        }
    }
}

/// Hue in degrees, saturation and lightness from 0 to 1.
pub fn hsl([r, g, b]: [u8; 3]) -> (f64, f64, f64) {
    let (r, g, b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-9 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s, l)
}

/// The pixel under a layout point (fractional on a scaled monitor), in the shot's
/// native pixels.
pub fn sample(shot: &Shot, pt: (f64, f64)) -> Option<[u8; 3]> {
    let x = ((pt.0 - shot.area.x as f64) * shot.scale).floor();
    let y = ((pt.1 - shot.area.y as f64) * shot.scale).floor();
    if x < 0.0 || y < 0.0 || x >= shot.image.width() as f64 || y >= shot.image.height() as f64 {
        return None;
    }
    let p = shot.image.get_pixel(x as u32, y as u32).0;
    Some([p[0], p[1], p[2]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;

    #[test]
    fn formats() {
        let c = [30, 30, 46];
        assert_eq!(format(c, Format::Hex), "#1e1e2e");
        assert_eq!(format(c, Format::Rgb), "rgb(30, 30, 46)");
        assert_eq!(format(c, Format::Hsl), "hsl(240, 21%, 15%)");
        assert_eq!(format([255, 0, 0], Format::Hsl), "hsl(0, 100%, 50%)");
        assert_eq!(format([128, 128, 128], Format::Hsl), "hsl(0, 0%, 50%)");
        assert_eq!(Format::parse("rgb"), Format::Rgb);
        assert_eq!(Format::parse("nope"), Format::Hex);
    }

    #[test]
    fn samples_native_pixels() {
        let mut img = image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255]));
        img.put_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
        // A 2x monitor: layout (101, 51) is pixel (2, 2), and half a unit is a pixel.
        let shot = Shot::from_image(image::DynamicImage::ImageRgba8(img), Rect::new(100, 50, 2, 2), 2.0, "test");
        assert_eq!(sample(&shot, (101.0, 51.0)), Some([10, 20, 30]));
        assert_eq!(sample(&shot, (100.5, 50.5)), Some([0, 0, 0]));
        assert_eq!(sample(&shot, (99.0, 50.0)), None);
    }
}
