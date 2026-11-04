//! Scrolling capture: frames of the same screen area, taken while the page under it
//! scrolls, stitched into one tall image. Parts that don't move (a browser's toolbar, a
//! page's fixed header or footer) are found by comparing frames and kept once, at the top
//! and bottom; only the part that scrolls is matched and stitched. Each frame is matched
//! against the last one that added anything, so scrolling back up adds nothing twice.

use image::{GenericImageView, RgbaImage};

/// Columns sampled across a row to compare rows quickly.
const SAMPLES: u32 = 96;
/// Mean difference (0–255) at which two overlapping strips count as the same content.
const MATCH: f64 = 6.0;
/// Mean difference under which a row is the same in two frames (it didn't move).
const SAME_ROW: f64 = 2.0;
/// The overlap must be at least this share of the moving part to trust a match.
const MIN_OVERLAP: f64 = 0.12;

pub struct Stitcher {
    /// Everything above the footer: the header, then the scrolling part stitched so far.
    body: RgbaImage,
    /// The newest frame's rows below the scrolling part.
    footer: RgbaImage,
    /// Rows that stay still at the top and bottom, once known.
    still: Option<(usize, usize)>,
    last: Option<Rows>,
    frames: usize,
}

/// A frame reduced to grey samples per row.
struct Rows {
    width: usize,
    height: usize,
    grey: Vec<u8>,
}

impl Rows {
    fn of(frame: &RgbaImage) -> Rows {
        let (w, h) = frame.dimensions();
        let width = SAMPLES.min(w).max(1) as usize;
        let mut grey = Vec::with_capacity(width * h as usize);
        for y in 0..h {
            for i in 0..width {
                let x = (i as u32 * w) / width as u32;
                let p = frame.get_pixel(x, y).0;
                grey.push(((p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10) as u8);
            }
        }
        Rows { width, height: h as usize, grey }
    }

    fn row(&self, y: usize) -> &[u8] {
        &self.grey[y * self.width..(y + 1) * self.width]
    }

    fn row_diff(&self, other: &Rows, y: usize) -> f64 {
        let (a, b) = (self.row(y), other.row(y));
        a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u64).sum::<u64>() as f64 / self.width as f64
    }
}

impl Default for Stitcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Stitcher {
    pub fn new() -> Self {
        Self { body: RgbaImage::new(0, 0), footer: RgbaImage::new(0, 0), still: None, last: None, frames: 0 }
    }

    /// Add a frame. Returns how many rows it added (0 when the page didn't move down,
    /// moved up, or the frame doesn't line up with the last one).
    pub fn push(&mut self, frame: &RgbaImage) -> u32 {
        let rows = Rows::of(frame);
        let Some(last) = &self.last else {
            self.body = frame.clone();
            self.last = Some(rows);
            self.frames = 1;
            return frame.height();
        };
        if last.height != rows.height || last.width != rows.width || frame.width() != self.body.width() {
            return 0;
        }
        let h = rows.height;
        let (top, bottom) = self.still.unwrap_or_else(|| still_bands(last, &rows));
        if top + bottom + 8 >= h {
            // Nothing (or almost nothing) moved.
            return 0;
        }
        let Some(moved) = offset(last, &rows, top, h - bottom) else {
            // Nothing stitched yet: a first frame that nothing lines up with (taken while
            // something was still fading on screen) gives way to this one.
            if self.frames == 1 {
                self.body = frame.clone();
                self.last = Some(rows);
            }
            return 0;
        };
        if moved == 0 {
            return 0;
        }
        let w = frame.width();
        if self.still.is_none() {
            // The first frame's footer comes off; the newest frame's goes back on at the end.
            self.still = Some((top, bottom));
            let keep = self.body.height() - bottom as u32;
            self.body = self.body.view(0, 0, w, keep).to_image();
        }
        let end = (h - bottom) as u32;
        let added = moved as u32;
        let old = self.body.height();
        let mut grown = RgbaImage::new(w, old + added);
        image::imageops::replace(&mut grown, &self.body, 0, 0);
        let fresh = frame.view(0, end - added, w, added).to_image();
        image::imageops::replace(&mut grown, &fresh, 0, old as i64);
        self.body = grown;
        self.footer = frame.view(0, end, w, bottom as u32).to_image();
        self.last = Some(rows);
        self.frames += 1;
        added
    }

    /// The stitched image so far (header, scrolled part, footer).
    pub fn image(&self) -> RgbaImage {
        if self.footer.height() == 0 {
            return self.body.clone();
        }
        let w = self.body.width();
        let mut out = RgbaImage::new(w, self.body.height() + self.footer.height());
        image::imageops::replace(&mut out, &self.body, 0, 0);
        image::imageops::replace(&mut out, &self.footer, 0, self.body.height() as i64);
        out
    }

    pub fn into_image(self) -> RgbaImage {
        self.image()
    }

    /// Height of the stitched image so far.
    pub fn height(&self) -> u32 {
        self.body.height() + self.footer.height()
    }

    /// Frames that added something.
    pub fn frames(&self) -> usize {
        self.frames
    }
}

/// Rows at the top and bottom that are the same in both frames: they didn't scroll.
fn still_bands(prev: &Rows, next: &Rows) -> (usize, usize) {
    let h = prev.height;
    let top = (0..h).take_while(|&y| prev.row_diff(next, y) < SAME_ROW).count();
    let bottom = (0..h - top).take_while(|&i| prev.row_diff(next, h - 1 - i) < SAME_ROW).count();
    (top, bottom)
}

/// How many rows the part between `top` and `end` moved up from `prev` to `next`: the
/// smallest `d` where `next[top..end-d]` matches `prev[top+d..end]`. `Some(0)` when
/// nothing moved; `None` when it doesn't line up (it moved down, or changed too much).
fn offset(prev: &Rows, next: &Rows, top: usize, end: usize) -> Option<usize> {
    let h = end.saturating_sub(top);
    if h == 0 {
        return None;
    }
    let min_overlap = ((h as f64 * MIN_OVERLAP).ceil() as usize).max(1);
    let step = (h / 200).max(1);
    let mut best: Option<(usize, f64)> = None;
    for d in 0..=(h - min_overlap) {
        let err = strip_error(prev, next, top, end, d, step, best.map(|b| b.1));
        if let Some(err) = err
            && best.is_none_or(|(_, e)| err < e)
        {
            best = Some((d, err));
            if err == 0.0 {
                break;
            }
        }
    }
    best.filter(|&(_, e)| e <= MATCH).map(|(d, _)| d)
}

/// Mean difference between `next`'s rows from `top` and `prev`'s rows from `top + d`, on
/// every `step`th row up to `end`; `None` as soon as it can't beat `beat`.
fn strip_error(prev: &Rows, next: &Rows, top: usize, end: usize, d: usize, step: usize, beat: Option<f64>) -> Option<f64> {
    let overlap = end - top - d;
    let rows = overlap.div_ceil(step);
    let budget = beat.map(|b| b * (rows * prev.width) as f64);
    let mut total = 0u64;
    let mut y = 0;
    while y < overlap {
        let a = prev.row(top + y + d);
        let b = next.row(top + y);
        total += a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u64).sum::<u64>();
        if let Some(budget) = budget
            && total as f64 > budget
        {
            return None;
        }
        y += step;
    }
    Some(total as f64 / (rows * prev.width) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tall "page" with rows that don't repeat, like a real page's lines.
    fn page(h: u32) -> RgbaImage {
        RgbaImage::from_fn(120, h, |x, y| {
            let mut v = (y as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ ((x / 10) as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
            v ^= v >> 29;
            let v = v as u8;
            image::Rgba([v, v.wrapping_mul(3), 255 - v, 255])
        })
    }

    fn view(page: &RgbaImage, top: u32, h: u32) -> RgbaImage {
        page.view(0, top, page.width(), h).to_image()
    }

    /// A window: a still header and footer around a view of `page` scrolled to `top`.
    fn window(page: &RgbaImage, top: u32, header: u32, body: u32, footer: u32) -> RgbaImage {
        let mut out = RgbaImage::from_pixel(page.width(), header + body + footer, image::Rgba([40, 40, 60, 255]));
        for x in 0..page.width() {
            out.put_pixel(x, header / 2, image::Rgba([200, 210, 240, 255]));
            out.put_pixel(x, header + body + footer / 2, image::Rgba([240, 120, 160, 255]));
        }
        image::imageops::replace(&mut out, &view(page, top, body), 0, header as i64);
        out
    }

    #[test]
    fn scrolling_down_appends_only_new_rows() {
        let p = page(1000);
        let mut s = Stitcher::new();
        assert_eq!(s.push(&view(&p, 0, 300)), 300);
        assert_eq!(s.push(&view(&p, 120, 300)), 120);
        assert_eq!(s.push(&view(&p, 300, 300)), 180);
        assert_eq!(s.height(), 600);
        assert_eq!(s.image().as_raw(), view(&p, 0, 600).as_raw());
        assert_eq!(s.frames(), 3);
    }

    #[test]
    fn a_still_header_and_footer_are_kept_once() {
        let p = page(1500);
        let (header, body, footer) = (60, 300, 30);
        let mut s = Stitcher::new();
        s.push(&window(&p, 0, header, body, footer));
        assert_eq!(s.push(&window(&p, 100, header, body, footer)), 100);
        assert_eq!(s.push(&window(&p, 220, header, body, footer)), 120);
        // Header once, the page from 0 to 520, footer once.
        let want = window(&p, 0, header, 520, footer);
        assert_eq!(s.image().dimensions(), want.dimensions());
        assert_eq!(s.image().as_raw(), want.as_raw());
    }

    #[test]
    fn a_still_frame_adds_nothing() {
        let p = page(600);
        let mut s = Stitcher::new();
        s.push(&view(&p, 0, 300));
        assert_eq!(s.push(&view(&p, 0, 300)), 0);
        assert_eq!(s.height(), 300);
    }

    #[test]
    fn scrolling_back_up_adds_nothing_twice() {
        let p = page(1200);
        let mut s = Stitcher::new();
        s.push(&view(&p, 0, 300));
        s.push(&view(&p, 200, 300));
        assert_eq!(s.push(&view(&p, 100, 300)), 0);
        assert_eq!(s.push(&view(&p, 350, 300)), 150);
        assert_eq!(s.image().as_raw(), view(&p, 0, 650).as_raw());
    }

    #[test]
    fn a_first_frame_nothing_matches_gives_way() {
        let p = page(1200);
        let noise = page(3000);
        let mut s = Stitcher::new();
        s.push(&view(&noise, 2500, 300));
        assert_eq!(s.push(&view(&p, 0, 300)), 0);
        assert_eq!(s.push(&view(&p, 100, 300)), 100);
        assert_eq!(s.image().as_raw(), view(&p, 0, 400).as_raw());
    }

    #[test]
    fn a_frame_that_jumped_too_far_is_skipped() {
        let p = page(2000);
        let mut s = Stitcher::new();
        s.push(&view(&p, 0, 300));
        s.push(&view(&p, 100, 300));
        assert_eq!(s.push(&view(&p, 1200, 300)), 0);
        assert_eq!(s.height(), 400);
    }
}
