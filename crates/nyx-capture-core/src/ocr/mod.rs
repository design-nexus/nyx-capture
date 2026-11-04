//! Dual-engine OCR. Tesseract is the default; RapidOCR is a lazy fallback.

mod preprocess;
mod quality;
mod rapidocr;
mod tesseract;

#[cfg(test)]
mod fixtures;

use anyhow::Result;
use image::DynamicImage;

pub use quality::{Band, TessQuality, needs_fallback, score_rapid, score_tesseract};
pub use rapidocr::{RapidOcr, download_models, models_present};
pub use tesseract::{Tesseract, languages as tesseract_languages};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrMode {
    Automatic,
    Tesseract,
    RapidOcr,
}

impl OcrMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "tesseract" => Self::Tesseract,
            "rapidocr" => Self::RapidOcr,
            _ => Self::Automatic,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OcrLine {
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// Engine-local confidence 0–100. Not comparable across engines.
    pub conf: f32,
}

#[derive(Debug, Clone)]
pub struct OcrResult {
    pub text: String,
    pub engine: String,
    pub lines: Vec<OcrLine>,
    pub fallback_used: bool,
}

pub trait OcrEngine {
    fn id(&self) -> &'static str;
    fn recognize(&self, img: &DynamicImage, langs: &str) -> Result<OcrResult>;
}

/// Run the configured pipeline. RapidOCR is only invoked on first fallback or
/// when the mode is RapidOCR; models are never loaded at startup.
pub fn recognize(img: &DynamicImage, mode: OcrMode, langs: &str) -> Result<OcrResult> {
    let prep = preprocess::prepare(img);
    match mode {
        OcrMode::Tesseract => Ok(scale_result(Tesseract.recognize(&prep.image, langs)?, prep.sx, prep.sy)),
        OcrMode::RapidOcr => Ok(scale_result(RapidOcr.recognize(&prep.image, langs)?, prep.sx, prep.sy)),
        OcrMode::Automatic => {
            let tess = match Tesseract.recognize(&prep.image, langs) {
                Ok(r) => scale_result(r, prep.sx, prep.sy),
                Err(err) => {
                    return RapidOcr
                        .recognize(&prep.image, langs)
                        .map(|r| scale_result(r, prep.sx, prep.sy))
                        .map_err(|_| err);
                }
            };
            let q = score_tesseract(&tess, img);
            if !needs_fallback(&q) {
                return Ok(tess);
            }
            // Automatic never downloads models. Missing cache keeps Tesseract so CLI OCR cannot hang on first RapidOCR fetch.
            if !models_present() {
                return Ok(tess);
            }
            match RapidOcr.recognize(&prep.image, langs) {
                Ok(rapid) => Ok(pick_better(tess, scale_result(rapid, prep.sx, prep.sy), img)),
                Err(_) => Ok(tess),
            }
        }
    }
}

/// Wrapped lines joined into paragraphs: a blank line still separates them, and a
/// word hyphenated across a line end is put back together.
pub fn unwrap_lines(text: &str) -> String {
    let mut out = String::new();
    let mut para = String::new();
    let flush = |out: &mut String, para: &mut String| {
        if !para.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(para);
            para.clear();
        }
    };
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            flush(&mut out, &mut para);
            continue;
        }
        if para.ends_with('-') && line.starts_with(|c: char| c.is_lowercase()) {
            para.pop();
        } else if !para.is_empty() {
            para.push(' ');
        }
        para.push_str(line);
    }
    flush(&mut out, &mut para);
    out
}

fn scale_result(mut result: OcrResult, sx: f32, sy: f32) -> OcrResult {
    if (sx - 1.0).abs() < f32::EPSILON && (sy - 1.0).abs() < f32::EPSILON {
        return result;
    }
    for line in &mut result.lines {
        line.x = ((line.x as f32) * sx).round() as i32;
        line.y = ((line.y as f32) * sy).round() as i32;
        line.w = ((line.w as f32) * sx).round() as i32;
        line.h = ((line.h as f32) * sy).round() as i32;
    }
    result
}

fn pick_better(tess: OcrResult, rapid: OcrResult, img: &DynamicImage) -> OcrResult {
    let t = score_tesseract(&tess, img).band();
    let r = score_rapid(&rapid);
    if matches!(r, Band::Good) && matches!(t, Band::Poor | Band::Empty) {
        OcrResult { fallback_used: true, ..rapid }
    } else {
        tess
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tess(text: &str, conf: f32) -> OcrResult {
        OcrResult {
            text: text.into(),
            engine: "tesseract".into(),
            lines: vec![OcrLine { text: text.into(), x: 0, y: 0, w: 80, h: 12, conf }],
            fallback_used: false,
        }
    }

    fn rapid(text: &str, conf: f32) -> OcrResult {
        OcrResult { engine: "rapidocr".into(), ..tess(text, conf) }
    }

    #[test]
    fn rapid_wins_only_when_good_and_tess_is_poor() {
        let img = DynamicImage::new_luma8(200, 200);
        let chosen = pick_better(tess("aaa bbb ccc ddd", 20.0), rapid("Open settings", 90.0), &img);
        assert_eq!(chosen.engine, "rapidocr");
        assert!(chosen.fallback_used);
    }

    #[test]
    fn tess_stays_when_its_band_is_good() {
        let img = DynamicImage::new_luma8(200, 200);
        let chosen = pick_better(tess("Open the settings window", 90.0), rapid("Open the settings window", 95.0), &img);
        assert_eq!(chosen.engine, "tesseract");
        assert!(!chosen.fallback_used);
    }

    #[test]
    fn wrapped_lines_become_paragraphs() {
        assert_eq!(unwrap_lines("The quick brown\nfox jumps over the la-\nzy dog.\n\nNext one\nhere."), "The quick brown fox jumps over the lazy dog.\n\nNext one here.");
        assert_eq!(unwrap_lines("A -\nB"), "A - B");
        assert_eq!(unwrap_lines(""), "");
    }

    #[test]
    fn empty_rapid_does_not_replace_empty_tess() {
        let img = DynamicImage::new_luma8(200, 200);
        let chosen = pick_better(tess("", -1.0), rapid("", 0.0), &img);
        assert_eq!(chosen.engine, "tesseract");
    }
}
