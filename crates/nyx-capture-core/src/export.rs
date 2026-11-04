//! PNG bytes, flatten of annotation sidecars, and simple file writes.

use anyhow::{Context, Result};
use image::DynamicImage;
use std::path::Path;

pub fn png_bytes(image: &DynamicImage) -> Result<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).context("png encode")?;
    Ok(out.into_inner())
}

pub fn write_png(image: &DynamicImage, path: &Path) -> Result<()> {
    let bytes = png_bytes(image)?;
    crate::atomic_write(path, &bytes)
}

/// Original PNG plus `{id}.ann.json` marks. The source file is never overwritten.
pub fn flatten_png(path: &Path) -> Result<DynamicImage> {
    let src = image::open(path).with_context(|| format!("open {}", path.display()))?.to_rgba8();
    let state = crate::annot::EditState::load(&crate::annot::sidecar_path(path)).unwrap_or_default();
    Ok(DynamicImage::ImageRgba8(crate::annot::flatten(&src, &state)))
}

pub fn write_flat(src: &Path, dest: &Path) -> Result<()> {
    write_png(&flatten_png(src)?, dest)
}

/// True when `path` has annotation marks or a crop beside it.
pub fn is_edited(path: &Path) -> bool {
    crate::annot::EditState::load(&crate::annot::sidecar_path(path)).is_ok_and(|s| s.is_edited())
}

/// The picture to hand out (copy, pin, thumbnail, Nyx Assistant): with its marks burned
/// in, so a redaction is never left behind in what leaves the editor.
pub fn shareable(path: &Path) -> Result<DynamicImage> {
    if is_edited(path) {
        flatten_png(path)
    } else {
        image::open(path).with_context(|| format!("open {}", path.display()))
    }
}

/// [`shareable`] as PNG bytes; the file as it is when there's nothing to burn in.
pub fn shareable_png(path: &Path) -> Result<Vec<u8>> {
    if is_edited(path) {
        png_bytes(&flatten_png(path)?)
    } else {
        std::fs::read(path).with_context(|| format!("read {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annot::{Colour, EditState, Mark};
    use image::{Rgba, RgbaImage};

    #[test]
    fn flatten_burns_marks_and_leaves_the_original() {
        let dir = std::env::temp_dir().join(format!("nyx-capture-flat-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("shot.png");
        let dest = dir.join("out.png");
        RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255])).save(&src).unwrap();
        let original = std::fs::read(&src).unwrap();
        let mut st = EditState::default();
        st.set_crop(Some(crate::annot::NormRect::new(0.0, 0.0, 0.5, 1.0)));
        st.marks.push(Mark::Rect {
            rect: crate::annot::NormRect::new(0.0, 0.0, 1.0, 1.0),
            colour: Colour::rgb(1.0, 0.0, 0.0),
            size: 0.2,
            fill: false,
            radius: 0.0,
        });
        st.save(&crate::annot::sidecar_path(&src)).unwrap();
        write_flat(&src, &dest).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), original);
        let out = image::open(&dest).unwrap();
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
