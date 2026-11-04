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

/// A file, as file managers and chat apps paste it.
pub fn copy_uri(uri: &str) -> Result<()> {
    pipe(&["wl-copy", "--type", "text/uri-list"], format!("{uri}\r\n").as_bytes())
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
    fn a_copier_that_stays_in_the_background_does_not_block() {
        let started = std::time::Instant::now();
        pipe(&["sh", "-c", "cat >/dev/null; sleep 5 & exit 0"], b"png").unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
}
