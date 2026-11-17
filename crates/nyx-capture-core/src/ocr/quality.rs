//! Tesseract quality heuristics. Raw scores are never compared to RapidOCR.

use super::OcrResult;
use image::DynamicImage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Good,
    Poor,
    Empty,
}

#[derive(Debug, Clone)]
pub struct TessQuality {
    pub mean_conf: f64,
    pub low_frac: f64,
    pub garbage: f64,
    pub empty: bool,
    pub tiny_source: bool,
    pub word_count: usize,
}

impl TessQuality {
    pub fn band(&self) -> Band {
        if self.empty {
            Band::Empty
        } else if self.mean_conf < 55.0 || self.low_frac > 0.45 || self.garbage > 0.25 {
            Band::Poor
        } else {
            Band::Good
        }
    }
}

pub fn score_tesseract(result: &OcrResult, img: &DynamicImage) -> TessQuality {
    let words: Vec<&str> = result.text.split_whitespace().collect();
    let confs: Vec<f32> = result.lines.iter().filter(|l| l.conf >= 0.0).map(|l| l.conf).collect();
    let mean_conf = if confs.is_empty() { 0.0 } else { confs.iter().sum::<f32>() as f64 / confs.len() as f64 };
    let low_frac = if confs.is_empty() { 1.0 } else { confs.iter().filter(|c| **c < 40.0).count() as f64 / confs.len() as f64 };
    let garbage = garbage_frac(&result.text);
    let empty = words.is_empty();
    let tiny_source = img.width().min(img.height()) < 80;
    TessQuality { mean_conf, low_frac, garbage, empty, tiny_source, word_count: words.len() }
}

/// RapidOCR band. Mean rec score is 0–1 after converting stored 0–100 conf.
/// Never compared to Tesseract confidence.
pub fn score_rapid(result: &OcrResult) -> Band {
    if result.text.split_whitespace().next().is_none() {
        return Band::Empty;
    }
    if garbage_frac(&result.text) > 0.25 {
        return Band::Poor;
    }
    let confs: Vec<f32> = result.lines.iter().filter(|l| l.conf >= 0.0).map(|l| l.conf).collect();
    let mean = if confs.is_empty() { 0.0 } else { confs.iter().sum::<f32>() as f64 / confs.len() as f64 };
    // Stored as 0–100 (engine-local). Band uses 0–1 RapidOCR scale: mean < 0.50 is Poor.
    if mean / 100.0 < 0.50 { Band::Poor } else { Band::Good }
}

pub fn needs_fallback(q: &TessQuality) -> bool {
    if q.empty {
        return true;
    }
    if q.mean_conf < 55.0 && q.word_count >= 3 {
        return true;
    }
    if q.low_frac > 0.45 {
        return true;
    }
    if q.garbage > 0.25 {
        return true;
    }
    if q.tiny_source && q.mean_conf < 70.0 {
        return true;
    }
    false
}

pub fn garbage_frac(text: &str) -> f64 {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return 1.0;
    }
    let bad = words.iter().filter(|w| is_garbage(w)).count();
    bad as f64 / words.len() as f64
}

fn is_garbage(word: &str) -> bool {
    let letters: Vec<char> = word.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() >= 5 {
        let vowels = letters.iter().filter(|c| "aeiouAEIOU".contains(**c)).count();
        if vowels == 0 {
            return true;
        }
    }
    let special = word.chars().filter(|c| !c.is_alphanumeric() && !"/.,:;!?'\"-".contains(*c)).count();
    special * 2 > word.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::OcrLine;

    fn result(text: &str, confs: &[f32]) -> OcrResult {
        let lines = confs
            .iter()
            .enumerate()
            .map(|(i, c)| OcrLine { text: format!("w{i}"), x: 0, y: i as i32 * 10, w: 40, h: 10, conf: *c })
            .collect();
        OcrResult { text: text.into(), engine: "tesseract".into(), lines, fallback_used: false }
    }

    #[test]
    fn empty_needs_fallback() {
        let img = DynamicImage::new_luma8(200, 200);
        let q = score_tesseract(&result("", &[]), &img);
        assert!(q.empty);
        assert!(needs_fallback(&q));
        assert_eq!(q.band(), Band::Empty);
    }

    #[test]
    fn high_confidence_plain_text_stays() {
        let img = DynamicImage::new_luma8(200, 200);
        let q = score_tesseract(&result("Open the settings window", &[90.0, 88.0, 92.0, 85.0]), &img);
        assert!(!needs_fallback(&q));
        assert_eq!(q.band(), Band::Good);
    }

    #[test]
    fn low_confidence_many_words_falls_back() {
        let img = DynamicImage::new_luma8(200, 200);
        let q = score_tesseract(&result("aaa bbb ccc ddd", &[20.0, 22.0, 18.0, 25.0]), &img);
        assert!(needs_fallback(&q));
        assert_eq!(q.band(), Band::Poor);
    }

    #[test]
    fn garbage_detector() {
        assert!(is_garbage("BCDFGH"));
        assert!(!is_garbage("Settings"));
        assert!(garbage_frac("### $$$") > 0.9);
    }

    #[test]
    fn rapid_good_band_ignores_tesseract_scale() {
        let r = OcrResult {
            text: "Open the settings window".into(),
            engine: "rapidocr".into(),
            lines: vec![OcrLine { text: "Open the settings window".into(), x: 0, y: 0, w: 80, h: 12, conf: 92.0 }],
            fallback_used: false,
        };
        assert_eq!(score_rapid(&r), Band::Good);
    }

    #[test]
    fn rapid_low_rec_score_is_poor() {
        let r = OcrResult {
            text: "Open settings".into(),
            engine: "rapidocr".into(),
            lines: vec![OcrLine { text: "Open settings".into(), x: 0, y: 0, w: 40, h: 10, conf: 30.0 }],
            fallback_used: false,
        };
        assert_eq!(score_rapid(&r), Band::Poor);
    }
}
