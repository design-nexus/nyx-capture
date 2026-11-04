//! Screenshot capture, annotation objects, OCR, and local history. No GTK.

pub mod action;
pub mod annot;
pub mod capture;
pub mod clipboard;
pub mod colour;
pub mod detect;
pub mod export;
pub mod geom;
pub mod ocr;
pub mod qr;
pub mod record;
pub mod redact;
pub mod scroll;
pub mod search;
pub mod select;
pub mod store;
pub mod webcam;

pub use annot::{ArrowStyle, Colour, EditState, History, Mark, NormRect, SpotShape};
pub use capture::{CaptureBackend, CaptureMode, CaptureRequest, Grim, Portal, Shot};
pub use detect::Tools;
pub use geom::Rect;
pub use ocr::{OcrEngine, OcrMode, OcrResult, recognize};
pub use record::{Backend as RecordBackend, Container, RecordCaps, RecordRequest, Session as RecordSession, Target as RecordTarget};
pub use store::{CaptureKind, CaptureRow, Db};

use anyhow::{Context, Result};
use std::path::Path;

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).ok();
    let tmp = dir.join(format!(".{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("cap")));
    std::fs::write(&tmp, data).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))
}
