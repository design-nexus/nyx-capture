//! grim as the primary Wayland screenshot backend.

use super::{CaptureBackend, CaptureMode, CaptureRequest, Shot};
use crate::geom::Rect;
use crate::select;
use anyhow::{Context, Result, bail, ensure};
use image::DynamicImage;
use std::io::Write;
use std::process::{Command, Stdio};

pub struct Grim;

impl CaptureBackend for Grim {
    fn id(&self) -> &'static str {
        "grim"
    }

    fn capture(&self, req: &CaptureRequest) -> Result<Shot> {
        match &req.mode {
            CaptureMode::FullDesktop => capture_args(&[], None),
            CaptureMode::Monitor(name) => capture_args(&["-o", name], Some(name.clone())),
            CaptureMode::Region(rect) => {
                ensure!(!rect.is_empty(), "nothing to capture");
                let g = rect.to_geometry();
                capture_args(&["-g", &g], None).map(|mut s| {
                    s.area = *rect;
                    s.scale = Shot::scale_of(&s.image, s.area);
                    s
                })
            }
            CaptureMode::SelectRegion | CaptureMode::SelectWindow => {
                bail!("selector modes must be resolved before grim")
            }
            CaptureMode::Window(rect) => capture_rect(*rect),
            CaptureMode::ActiveWindow => {
                let rect = select::active_window()?.ok_or_else(|| anyhow::anyhow!("no focused window"))?.geometry;
                capture_rect(rect)
            }
        }
    }
}

fn capture_rect(rect: Rect) -> Result<Shot> {
    ensure!(!rect.is_empty(), "nothing to capture");
    let g = rect.to_geometry();
    capture_args(&["-g", &g], None).map(|mut s| {
        s.area = rect;
        s.scale = Shot::scale_of(&s.image, s.area);
        s
    })
}

fn capture_args(extra: &[&str], output: Option<String>) -> Result<Shot> {
    let mut cmd = Command::new("grim");
    cmd.args(["-t", "png"]).args(extra).arg("-").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = cmd.output().context("grim isn't available")?;
    if !out.status.success() {
        bail!("grim failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let image = image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Png).context("grim's image")?;
    let area = match output.as_deref() {
        Some(name) => select::monitor_named(name)?.map(|m| m.geometry).unwrap_or_else(|| fallback_area(&image)),
        None => select::desktop_bounds().unwrap_or_else(|_| fallback_area(&image)),
    };
    Shot::from_png(out.stdout, area, output, "grim")
}

fn fallback_area(image: &DynamicImage) -> Rect {
    Rect::new(0, 0, image.width() as i32, image.height() as i32)
}

/// slurp prints geometry or fails when cancelled.
pub fn slurp_geometry() -> Result<Option<Rect>> {
    let out = Command::new("slurp")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("slurp isn't available")?;
    if !out.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Rect::from_geometry(&text))
}

/// Hint slurp with window and monitor rectangles, Omarchy smart-capture style.
pub fn slurp_with_rects(rects: &[Rect]) -> Result<Option<Rect>> {
    let mut child = Command::new("slurp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("slurp isn't available")?;
    if let Some(mut stdin) = child.stdin.take() {
        let body = rects.iter().map(|r| r.to_geometry()).collect::<Vec<_>>().join("\n");
        let _ = stdin.write_all(body.as_bytes());
    }
    let out = child.wait_with_output().context("slurp")?;
    if !out.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Rect::from_geometry(&text))
}
