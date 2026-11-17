//! Layout rectangles and the mapping to image pixels on scaled monitors.

use serde::{Deserialize, Serialize};

/// A box in integer coordinates. `Shot` documents whether those are layout or image pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn area(self) -> i64 {
        self.w.max(0) as i64 * self.h.max(0) as i64
    }

    pub fn contains(self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }

    /// grim / slurp geometry: `x,y WxH`.
    pub fn to_geometry(self) -> String {
        format!("{},{} {}x{}", self.x, self.y, self.w, self.h)
    }

    /// Parse `x,y WxH`, including the decimal forms slurp sometimes prints.
    pub fn from_geometry(s: &str) -> Option<Self> {
        let s = s.trim();
        let (xy, wh) = s.split_once(' ')?;
        let (x, y) = xy.split_once(',')?;
        let (w, h) = wh.split_once('x')?;
        Some(Self { x: parse_coord(x)?, y: parse_coord(y)?, w: parse_coord(w)?.max(0), h: parse_coord(h)?.max(0) })
    }

    /// Image-pixel box as a layout box given the capture's origin and scale.
    pub fn to_layout(self, origin: Rect, scale: f64) -> Rect {
        let s = if scale <= 0.0 { 1.0 } else { scale };
        Rect {
            x: origin.x + (self.x as f64 / s).round() as i32,
            y: origin.y + (self.y as f64 / s).round() as i32,
            w: (self.w as f64 / s).round() as i32,
            h: (self.h as f64 / s).round() as i32,
        }
    }

    /// Layout box as an image-pixel box, clipped to `image_w`×`image_h`.
    pub fn to_image(self, origin: Rect, scale: f64, image_w: i32, image_h: i32) -> Rect {
        let s = if scale <= 0.0 { 1.0 } else { scale };
        let x = ((self.x - origin.x) as f64 * s).round() as i32;
        let y = ((self.y - origin.y) as f64 * s).round() as i32;
        let x0 = x.clamp(0, image_w);
        let y0 = y.clamp(0, image_h);
        let x1 = (x + (self.w as f64 * s).round() as i32).clamp(0, image_w);
        let y1 = (y + (self.h as f64 * s).round() as i32).clamp(0, image_h);
        Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }
}

fn parse_coord(s: &str) -> Option<i32> {
    let s = s.trim();
    if let Ok(n) = s.parse::<i32>() {
        return Some(n);
    }
    s.parse::<f64>().ok().map(|n| n.round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_round_trip() {
        let r = Rect::new(100, 50, 300, 200);
        assert_eq!(Rect::from_geometry(&r.to_geometry()), Some(r));
        assert_eq!(Rect::from_geometry("10.4,20.6 30.4x40.6"), Some(Rect::new(10, 21, 30, 41)));
        assert_eq!(Rect::from_geometry("nope"), None);
    }

    #[test]
    fn maps_between_image_and_layout() {
        // A 1.5× monitor: 300 layout units wide come out as 450 pixels.
        let origin = Rect::new(1000, 50, 300, 200);
        let scale = 1.5;
        let layout = Rect::new(150, 30, 45, 15).to_layout(origin, scale);
        assert_eq!(layout, Rect::new(1100, 70, 30, 10));
        assert_eq!(layout.to_image(origin, scale, 450, 300), Rect::new(150, 30, 45, 15));
        assert_eq!(Rect::new(990, 40, 20, 20).to_image(origin, scale, 450, 300), Rect::new(0, 0, 15, 15));
    }
}
