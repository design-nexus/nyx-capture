//! QR codes on screen: where they are and what they say.
//!
//! A QR code can hold a secret (the otpauth:// URI behind a 2FA setup), so callers
//! name a result by its [`Kind`] and never print or keep its text.

use image::{DynamicImage, GrayImage, imageops};

/// A code found in an image: its text and its corners in that image's pixels
/// (top-left, top-right, bottom-right, bottom-left).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Code {
    pub text: String,
    pub corners: [(i32, i32); 4],
}

impl Code {
    /// The box around the corners: x, y, width, height.
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        let xs = self.corners.map(|p| p.0);
        let ys = self.corners.map(|p| p.1);
        let (x0, x1) = (*xs.iter().min().unwrap_or(&0), *xs.iter().max().unwrap_or(&0));
        let (y0, y1) = (*ys.iter().min().unwrap_or(&0), *ys.iter().max().unwrap_or(&0));
        (x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
    }
}

/// Codes small on a large screen still have a few pixels per module; below this the
/// image is scanned again at twice the size.
const UPSCALE_BELOW: u32 = 600;

/// Every QR code in `img`, top to bottom. Light-on-dark codes are found too.
pub fn scan(img: &DynamicImage) -> Vec<Code> {
    let grey = img.to_luma8();
    let mut found = decode_all(&grey, 1);
    if found.is_empty() {
        let mut inverted = grey.clone();
        imageops::invert(&mut inverted);
        found = decode_all(&inverted, 1);
    }
    if found.is_empty() && grey.width().min(grey.height()) < UPSCALE_BELOW {
        let big = imageops::resize(&grey, grey.width() * 2, grey.height() * 2, imageops::FilterType::Nearest);
        found = decode_all(&big, 2);
    }
    found.sort_by_key(|c| (c.bounds().1, c.bounds().0));
    found.dedup_by(|a, b| a.text == b.text && a.bounds() == b.bounds());
    found
}

/// The text of the code in `img` (the first, if there are several).
pub fn decode(img: &DynamicImage) -> Option<String> {
    scan(img).into_iter().next().map(|c| c.text)
}

fn decode_all(grey: &GrayImage, factor: i32) -> Vec<Code> {
    let (w, h) = (grey.width() as usize, grey.height() as usize);
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| grey.get_pixel(x as u32, y as u32).0[0]);
    prepared
        .detect_grids()
        .into_iter()
        .filter_map(|grid| {
            let (_, text) = grid.decode().ok()?;
            let corners = grid.bounds.map(|p| (p.x / factor, p.y / factor));
            Some(Code { text, corners })
        })
        .collect()
}

/// What a code holds, so it can be named without showing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A web address, with its host.
    Link(String),
    Wifi,
    /// A 2FA setup code (otpauth://).
    TwoFactor,
    Contact,
    Email,
    Phone,
    Text,
}

impl Kind {
    /// "Link copied" and the like, for a toast. Only a link names anything (its host).
    pub fn copied(&self) -> String {
        match self {
            Kind::Link(host) if !host.is_empty() => format!("Link to {host} copied"),
            Kind::Link(_) => "Link copied".into(),
            Kind::Wifi => "Wi-Fi network copied".into(),
            Kind::TwoFactor => "2FA setup code copied".into(),
            Kind::Contact => "Contact copied".into(),
            Kind::Email => "Email address copied".into(),
            Kind::Phone => "Phone number copied".into(),
            Kind::Text => "QR code text copied".into(),
        }
    }
}

pub fn classify(text: &str) -> Kind {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let rest = &t[t.find("://").map(|i| i + 3).unwrap_or(0)..];
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = host.rsplit('@').next().unwrap_or(host);
        let host = host.split(':').next().unwrap_or(host).trim_start_matches("www.");
        return Kind::Link(host.to_string());
    }
    if lower.starts_with("wifi:") {
        Kind::Wifi
    } else if lower.starts_with("otpauth://") || lower.starts_with("otpauth-migration://") {
        Kind::TwoFactor
    } else if lower.starts_with("begin:vcard") || lower.starts_with("mecard:") {
        Kind::Contact
    } else if lower.starts_with("mailto:") || lower.starts_with("matmsg:") {
        Kind::Email
    } else if lower.starts_with("tel:") || lower.starts_with("sms:") || lower.starts_with("smsto:") {
        Kind::Phone
    } else {
        Kind::Text
    }
}

/// The address to open, when the code is a web link.
pub fn link(text: &str) -> Option<&str> {
    matches!(classify(text), Kind::Link(_)).then_some(text.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qrcode::QrCode;

    fn code_image(text: &str, quiet: u32) -> DynamicImage {
        let code = QrCode::new(text.as_bytes()).unwrap();
        let img = code.render::<image::Luma<u8>>().module_dimensions(6, 6).quiet_zone(false).build();
        let mut canvas = GrayImage::from_pixel(img.width() + quiet * 2, img.height() + quiet * 2, image::Luma([255]));
        imageops::overlay(&mut canvas, &img, quiet as i64, quiet as i64);
        DynamicImage::ImageLuma8(canvas)
    }

    #[test]
    fn reads_a_code_and_where_it_is() {
        let img = code_image("https://example.com/a?b=1", 40);
        let codes = scan(&img);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].text, "https://example.com/a?b=1");
        let (x, y, w, h) = codes[0].bounds();
        assert!((35..=45).contains(&x) && (35..=45).contains(&y), "{x},{y}");
        assert!(w > 50 && h > 50);
    }

    #[test]
    fn reads_light_on_dark_codes() {
        let mut img = code_image("WIFI:S:home;T:WPA;P:secret;;", 30).to_luma8();
        imageops::invert(&mut img);
        assert_eq!(decode(&DynamicImage::ImageLuma8(img)).as_deref(), Some("WIFI:S:home;T:WPA;P:secret;;"));
    }

    #[test]
    fn nothing_in_a_blank_image() {
        assert!(scan(&DynamicImage::ImageLuma8(GrayImage::from_pixel(200, 120, image::Luma([255])))).is_empty());
    }

    #[test]
    fn kinds_name_without_showing() {
        assert_eq!(classify("https://www.example.com:8080/x"), Kind::Link("example.com".into()));
        assert_eq!(classify("otpauth://totp/a?secret=XYZ"), Kind::TwoFactor);
        assert_eq!(classify("WIFI:S:x;;"), Kind::Wifi);
        assert_eq!(classify("BEGIN:VCARD\nFN:A"), Kind::Contact);
        assert_eq!(classify("tel:+1555"), Kind::Phone);
        assert_eq!(classify("hello"), Kind::Text);
        assert!(!Kind::TwoFactor.copied().contains("otpauth"));
        assert_eq!(link(" https://a.b "), Some("https://a.b"));
        assert_eq!(link("otpauth://x"), None);
    }
}
