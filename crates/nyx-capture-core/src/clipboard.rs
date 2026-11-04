//! Clipboard via wl-copy.

use anyhow::{Context, Result, bail};
use std::io::Write;
use std::process::{Command, Stdio};

pub fn copy_png(bytes: &[u8]) -> Result<()> {
    pipe(&["wl-copy", "--type", "image/png"], bytes)
}

pub fn copy_text(text: &str) -> Result<()> {
    pipe(&["wl-copy", "--type", "text/plain;charset=utf-8"], text.as_bytes())
}

/// Text marked sensitive, so clipboard history leaves it out.
pub fn copy_secret(text: &str) -> Result<()> {
    pipe(&["wl-copy", "--sensitive", "--type", "text/plain;charset=utf-8"], text.as_bytes())
}

/// A file, as file managers and chat apps paste it.
pub fn copy_uri(uri: &str) -> Result<()> {
    pipe(&["wl-copy", "--type", "text/uri-list"], format!("{uri}\r\n").as_bytes())
}

/// The image on the clipboard, PNG first. Text alone is an error.
pub fn paste_image() -> Result<image::DynamicImage> {
    let types = Command::new("wl-paste").arg("--list-types").output().context("wl-paste isn't available")?;
    let types = String::from_utf8_lossy(&types.stdout);
    let pick = image_type(types.lines()).context("The clipboard has no image")?;
    let out = Command::new("wl-paste").args(["--no-newline", "--type", pick]).output().context("wl-paste")?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("The clipboard image couldn't be read");
    }
    image::load_from_memory(&out.stdout).context("The clipboard image couldn't be read")
}

fn image_type<'a>(types: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let types: Vec<&str> = types.map(str::trim).filter(|t| t.starts_with("image/")).collect();
    types.iter().find(|t| **t == "image/png").or(types.first()).copied()
}

fn pipe(args: &[&str], bytes: &[u8]) -> Result<()> {
    let (program, rest) = args.split_first().context("empty command")?;
    // wl-copy forks a process that serves the clipboard and keeps any inherited pipe
    // open until something else is copied, so nothing here may wait on its output.
    let mut child = Command::new(program).args(rest).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().with_context(|| format!("{program} isn't available"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(bytes).context("clipboard stdin")?;
    }
    let status = child.wait().context("clipboard")?;
    if !status.success() {
        bail!("{program} failed ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_is_preferred_and_text_is_not_an_image() {
        assert_eq!(image_type(["text/plain", "image/jpeg", "image/png"].into_iter()), Some("image/png"));
        assert_eq!(image_type(["image/webp"].into_iter()), Some("image/webp"));
        assert_eq!(image_type(["text/plain", "UTF8_STRING"].into_iter()), None);
    }

    #[test]
    fn a_copier_that_stays_in_the_background_does_not_block() {
        let started = std::time::Instant::now();
        pipe(&["sh", "-c", "cat >/dev/null; sleep 5 & exit 0"], b"png").unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
}
