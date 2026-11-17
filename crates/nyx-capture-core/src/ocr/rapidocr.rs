//! RapidOCR via `rapidocr-core`. Models load on first use, never at startup.

use super::{OcrEngine, OcrLine, OcrResult};
use anyhow::{Context, Result};
use image::DynamicImage;
use rapidocr_core::RapidOcr as Engine;
use rapidocr_core::model::{ModelCache, ModelDownloadMode, PPOCRV6_SMALL};
use std::path::PathBuf;
use std::sync::Mutex;

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

pub struct RapidOcr;

impl OcrEngine for RapidOcr {
    fn id(&self) -> &'static str {
        "rapidocr"
    }

    fn recognize(&self, img: &DynamicImage, _langs: &str) -> Result<OcrResult> {
        let rgb = img.to_rgb8();
        let mut slot = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(load()?);
        }
        let engine = slot.as_mut().expect("engine just loaded");
        let out = engine.run_image(&rgb).context("RapidOCR")?;
        let lines: Vec<OcrLine> = out
            .lines
            .into_iter()
            .filter(|l| !l.text.trim().is_empty())
            .map(|l| {
                let (x, y, w, h) = quad_box(&l.bbox.points);
                OcrLine { text: l.text, x, y, w, h, conf: l.score * 100.0 }
            })
            .collect();
        let text = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
        Ok(OcrResult { text, engine: "rapidocr".into(), lines, fallback_used: false })
    }
}

/// Fetch RapidOCR's models into its cache (when the person asks, from Settings).
pub fn download_models() -> Result<()> {
    ModelCache::new(cache_dir()).ensure_ppocrv6_small(ModelDownloadMode::Missing).context("RapidOCR models")?;
    Ok(())
}

pub fn models_present() -> bool {
    ModelCache::new(cache_dir()).missing_assets(&PPOCRV6_SMALL).is_empty()
}

pub fn cache_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("NYX_CAPTURE_OCR_CACHE") {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(p).join("nyx-capture/rapidocr");
    }
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h).join(".cache/nyx-capture/rapidocr");
    }
    PathBuf::from("/tmp/nyx-capture/rapidocr")
}

fn download_mode() -> ModelDownloadMode {
    if cfg!(test) {
        return ModelDownloadMode::Never;
    }
    match std::env::var("NYX_CAPTURE_OCR_DOWNLOAD") {
        Ok(v) if v.eq_ignore_ascii_case("never") => ModelDownloadMode::Never,
        _ => ModelDownloadMode::Missing,
    }
}

fn load() -> Result<Engine> {
    let cache = ModelCache::new(cache_dir());
    cache.ensure_ppocrv6_small(download_mode()).context("RapidOCR models")?;
    Engine::new(cache.config_for(&PPOCRV6_SMALL)).context("RapidOCR engine")
}

fn quad_box(points: &[[f32; 2]; 4]) -> (i32, i32, i32, i32) {
    let xs = points.map(|p| p[0]);
    let ys = points.map(|p| p[1]);
    let min_x = xs.into_iter().fold(f32::INFINITY, f32::min);
    let max_x = xs.into_iter().fold(f32::NEG_INFINITY, f32::max);
    let min_y = ys.into_iter().fold(f32::INFINITY, f32::min);
    let max_y = ys.into_iter().fold(f32::NEG_INFINITY, f32::max);
    let x = min_x.floor() as i32;
    let y = min_y.floor() as i32;
    (x, y, (max_x.ceil() as i32 - x).max(0), (max_y.ceil() as i32 - y).max(0))
}

#[cfg(test)]
pub fn engine_is_loaded() -> bool {
    ENGINE.lock().map(|g| g.is_some()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probing_models_does_not_load_the_engine() {
        let _ = models_present();
        assert!(!engine_is_loaded());
    }
}
