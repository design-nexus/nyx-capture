//! CLI capture actions used by `nyx-capture` without opening the history window.

use crate::capture::{self, CaptureMode, CaptureRequest, slurp_region};
use crate::geom::Rect;
use crate::ocr::{OcrMode, recognize};
use crate::select;
use crate::clipboard;
use anyhow::{Result, bail};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum After {
    Copy,
    Save,
    Ocr,
    Edit,
    Pin,
    /// Copy and save at once.
    Quick,
}

#[derive(Debug, Clone)]
pub struct Action {
    pub mode: CaptureMode,
    pub after: After,
    pub delay: u32,
    pub save_path: Option<PathBuf>,
    pub ocr_langs: String,
    pub ocr_mode: OcrMode,
    pub backend: String,
}

pub enum Outcome {
    Copied,
    Saved(PathBuf),
    /// Copied and saved to this path.
    Quick(PathBuf),
    Text(String),
    Cancelled,
}

pub fn run(action: &Action) -> Result<Outcome> {
    Ok(run_keeping(action)?.0)
}

/// [`run`], also handing back the shot (for history).
pub fn run_keeping(action: &Action) -> Result<(Outcome, Option<crate::capture::Shot>)> {
    let Some(shot) = capture(action)? else { return Ok((Outcome::Cancelled, None)) };
    Ok((finish(&shot, action)?, Some(shot)))
}

/// Grim (and slurp, if the mode needs it). `None` is a cancelled selector.
pub fn capture(action: &Action) -> Result<Option<crate::capture::Shot>> {
    if action.delay > 0 {
        thread::sleep(Duration::from_secs(action.delay as u64));
    }
    if capture::pick(&action.backend) == "portal" {
        let interactive = matches!(
            action.mode,
            CaptureMode::SelectRegion | CaptureMode::SelectWindow | CaptureMode::Region(_) | CaptureMode::Window(_)
        );
        return capture::portal::take(interactive);
    }
    let mode = resolve_mode(&action.mode)?;
    let Some(mode) = mode else { return Ok(None) };
    Ok(Some(capture::take(&CaptureRequest { mode }, &action.backend)?))
}

/// Copy, save, or OCR a shot that is already in hand (overlay crop, or grim).
pub fn finish(shot: &crate::capture::Shot, action: &Action) -> Result<Outcome> {
    match action.after {
        After::Copy => {
            clipboard::copy_png(&shot.png()?)?;
            Ok(Outcome::Copied)
        }
        After::Save => {
            let path = action.save_path.clone().unwrap_or_else(default_save_path);
            crate::atomic_write(&path, &shot.png()?)?;
            Ok(Outcome::Saved(path))
        }
        After::Quick => {
            let png = shot.png()?;
            clipboard::copy_png(&png)?;
            let path = action.save_path.clone().unwrap_or_else(default_save_path);
            crate::atomic_write(&path, &png)?;
            Ok(Outcome::Quick(path))
        }
        After::Ocr => {
            let result = recognize(&shot.image, action.ocr_mode, &action.ocr_langs)?;
            clipboard::copy_text(&result.text)?;
            Ok(Outcome::Text(result.text))
        }
        After::Edit | After::Pin => Ok(Outcome::Cancelled),
    }
}

fn resolve_mode(mode: &CaptureMode) -> Result<Option<CaptureMode>> {
    match mode {
        CaptureMode::SelectRegion => Ok(slurp_region()?.map(CaptureMode::Region)),
        CaptureMode::Region(r) if r.is_empty() => Ok(slurp_region()?.map(CaptureMode::Region)),
        CaptureMode::SelectWindow => {
            let mut rects: Vec<Rect> = select::monitors()?.into_iter().map(|m| m.geometry).collect();
            rects.extend(select::windows()?.into_iter().map(|w| w.geometry));
            Ok(crate::capture::slurp_with_rects(&rects)?.map(CaptureMode::Window))
        }
        CaptureMode::FullDesktop => {
            if let Some(m) = select::focused_monitor()? {
                Ok(Some(CaptureMode::Monitor(m.name)))
            } else {
                Ok(Some(CaptureMode::FullDesktop))
            }
        }
        other => Ok(Some(other.clone())),
    }
}

pub fn default_save_path() -> PathBuf {
    save_path_in(&default_save_dir())
}

pub fn default_save_dir() -> PathBuf {
    let pictures = std::env::var_os("XDG_PICTURES_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Pictures")))
        .unwrap_or_else(|| PathBuf::from("."));
    pictures.join("Nyx Capture")
}

pub fn save_path_in(dir: &std::path::Path) -> PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    unique_png(dir, &format!("capture-{stamp}"))
}

/// `dir/stem.png`, or `stem-2.png`, `stem-3.png`… when that is taken. Creates `dir`.
pub fn unique_png(dir: &std::path::Path, stem: &str) -> PathBuf {
    let _ = std::fs::create_dir_all(dir);
    let first = dir.join(format!("{stem}.png"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{stem}-{n}.png"))).find(|p| !p.exists()).unwrap_or(first)
}

/// A file name from a pattern: `{date}`, `{time}` and `{app}` filled in. An empty app
/// takes a separator next to it along, so `shot-{date}-{app}` gives `shot-2026-11-03`.
pub fn file_stem(pattern: &str, date: &str, time: &str, app: &str) -> String {
    let mut out = pattern.replace("{date}", date).replace("{time}", time);
    if app.is_empty() {
        for sep in ["-", "_", " ", "."] {
            out = out.replace(&format!("{sep}{{app}}"), "").replace(&format!("{{app}}{sep}"), "");
        }
    }
    let out = out.replace("{app}", app).replace('/', "-");
    let out = out.trim_matches(|c: char| c == '-' || c == '_' || c.is_whitespace());
    if out.is_empty() { "screenshot".into() } else { out.to_string() }
}

/// A window class as a short file-name word: `org.mozilla.firefox` → `firefox`.
pub fn app_slug(class: &str) -> String {
    let last = class.rsplit('.').next().unwrap_or(class).to_lowercase();
    let mut slug = String::new();
    for c in last.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

pub fn parse_after(_copy: bool, save: bool, ocr: bool, pin: bool, edit: bool) -> Result<After> {
    if ocr {
        Ok(After::Ocr)
    } else if pin {
        Ok(After::Pin)
    } else if edit {
        Ok(After::Edit)
    } else if save {
        Ok(After::Save)
    } else {
        Ok(After::Copy)
    }
}

pub fn require_tools_for(mode: &CaptureMode, after: After, ocr_mode: OcrMode) -> Result<()> {
    let tools = crate::detect::Tools::probe();
    if !tools.grim {
        bail!("Screenshots need grim");
    }
    if matches!(mode, CaptureMode::Region(_) | CaptureMode::SelectRegion | CaptureMode::SelectWindow) && !tools.slurp {
        bail!("Region capture needs slurp or the GTK overlay");
    }
    if matches!(mode, CaptureMode::ActiveWindow) && !tools.hyprctl {
        bail!("Window capture needs hyprctl");
    }
    if matches!(after, After::Copy | After::Ocr | After::Quick) && !tools.wl_copy {
        bail!("Copy needs wl-copy");
    }
    if matches!(after, After::Ocr) {
        match ocr_mode {
            OcrMode::Tesseract if !tools.tesseract => bail!("OCR needs tesseract"),
            OcrMode::RapidOcr => {}
            OcrMode::Automatic if !tools.can_ocr() => bail!("OCR needs tesseract or RapidOCR models"),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_fill_in_the_pattern() {
        let p = "screenshot-{date}_{time}-{app}";
        assert_eq!(file_stem(p, "2026-11-03", "19-52-01", "firefox"), "screenshot-2026-11-03_19-52-01-firefox");
        assert_eq!(file_stem(p, "2026-11-03", "19-52-01", ""), "screenshot-2026-11-03_19-52-01");
        assert_eq!(file_stem("{app}_{date}", "2026-11-03", "x", ""), "2026-11-03");
        assert_eq!(file_stem("{app}", "d", "t", ""), "screenshot");
        assert_eq!(file_stem("a/b-{date}", "d", "t", ""), "a-b-d");
    }

    #[test]
    fn classes_become_short_words() {
        assert_eq!(app_slug("org.mozilla.firefox"), "firefox");
        assert_eq!(app_slug("Alacritty"), "alacritty");
        assert_eq!(app_slug("io.github.design_nexus.nyx.Files"), "files");
        assert_eq!(app_slug("Google Chrome"), "google-chrome");
        assert_eq!(app_slug(""), "");
    }

    #[test]
    fn taken_names_get_a_number() {
        let dir = std::env::temp_dir().join(format!("nyx-capture-names-{}", std::process::id()));
        let first = unique_png(&dir, "shot");
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(unique_png(&dir, "shot"), dir.join("shot-2.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
