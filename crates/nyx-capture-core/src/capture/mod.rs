//! Interchangeable screenshot backends.

mod grim;
pub mod portal;

use crate::detect;
use crate::geom::Rect;
use anyhow::Result;
use image::DynamicImage;

pub use grim::Grim;
pub use portal::Portal;

#[derive(Debug, Clone)]
pub enum CaptureMode {
    FullDesktop,
    Monitor(String),
    ActiveWindow,
    Window(Rect),
    Region(Rect),
    /// slurp, optionally hinted with window rectangles.
    SelectRegion,
    SelectWindow,
}

#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub mode: CaptureMode,
}

#[derive(Clone)]
pub struct Shot {
    pub image: DynamicImage,
    /// What was captured, in compositor layout coordinates.
    pub area: Rect,
    /// Image pixels per layout unit.
    pub scale: f64,
    pub output: Option<String>,
    pub backend: String,
    png: Option<Vec<u8>>,
}

impl Shot {
    pub fn from_png(bytes: Vec<u8>, area: Rect, output: Option<String>, backend: &str) -> Result<Self> {
        let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)?;
        Ok(Self {
            scale: Self::scale_of(&image, area),
            image,
            area,
            output,
            backend: backend.into(),
            png: Some(bytes),
        })
    }

    /// A shot made here (a stitched scroll), with `scale` image pixels per layout unit.
    pub fn from_image(image: DynamicImage, area: Rect, scale: f64, backend: &str) -> Self {
        Self { image, area, scale, output: None, backend: backend.into(), png: None }
    }

    pub fn scale_of(image: &DynamicImage, area: Rect) -> f64 {
        if area.w <= 0 {
            1.0
        } else {
            image.width() as f64 / area.w as f64
        }
    }

    pub fn png(&self) -> Result<Vec<u8>> {
        if let Some(bytes) = &self.png {
            return Ok(bytes.clone());
        }
        crate::export::png_bytes(&self.image)
    }

    pub fn crop_layout(&self, region: Rect) -> Result<Shot> {
        let img = region.to_image(self.area, self.scale, self.image.width() as i32, self.image.height() as i32);
        if img.is_empty() {
            anyhow::bail!("nothing to capture");
        }
        let cropped = self.image.crop_imm(img.x as u32, img.y as u32, img.w as u32, img.h as u32);
        Ok(Shot {
            scale: Self::scale_of(&cropped, region),
            image: cropped,
            area: region,
            output: self.output.clone(),
            backend: self.backend.clone(),
            png: None,
        })
    }
}

pub trait CaptureBackend {
    fn id(&self) -> &'static str;
    fn capture(&self, req: &CaptureRequest) -> Result<Shot>;
}

/// Pick a region with slurp. Empty / non-zero exit is cancel.
pub fn slurp_region() -> Result<Option<Rect>> {
    grim::slurp_geometry()
}

pub fn slurp_with_rects(rects: &[Rect]) -> Result<Option<Rect>> {
    grim::slurp_with_rects(rects)
}

/// `auto` uses grim when it is on PATH, otherwise the screenshot portal.
pub fn take(req: &CaptureRequest, backend: &str) -> Result<Shot> {
    match pick(backend) {
        "portal" => Portal.capture(req),
        _ => Grim.capture(req),
    }
}

pub fn pick(backend: &str) -> &'static str {
    if let Ok(forced) = std::env::var("NYX_CAPTURE_BACKEND") {
        if forced == "portal" {
            return "portal";
        }
        if forced == "grim" {
            return "grim";
        }
    }
    match backend {
        "portal" => "portal",
        "grim" => "grim",
        _ if detect::present("grim") => "grim",
        _ => "portal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_prefers_grim_when_forced() {
        unsafe { std::env::set_var("NYX_CAPTURE_BACKEND", "grim") };
        assert_eq!(pick("auto"), "grim");
        unsafe { std::env::set_var("NYX_CAPTURE_BACKEND", "portal") };
        assert_eq!(pick("auto"), "portal");
        unsafe { std::env::remove_var("NYX_CAPTURE_BACKEND") };
    }
}
