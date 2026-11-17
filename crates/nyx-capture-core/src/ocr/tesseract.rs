//! Tesseract via the system CLI. TSV gives boxes without linking leptonica.

use super::{OcrEngine, OcrLine, OcrResult};
use anyhow::{Context, Result, bail};
use image::DynamicImage;
use std::io::Write;
use std::process::{Command, Stdio};

pub struct Tesseract;

impl OcrEngine for Tesseract {
    fn id(&self) -> &'static str {
        "tesseract"
    }

    fn recognize(&self, img: &DynamicImage, langs: &str) -> Result<OcrResult> {
        let png = crate::export::png_bytes(img)?;
        let mut child = Command::new("tesseract")
            .args(["stdin", "stdout", "--oem", "1", "--psm", "6", "-l", langs, "--dpi", "300", "tsv"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("tesseract isn't available")?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&png).context("tesseract stdin")?;
        }
        let out = child.wait_with_output().context("tesseract")?;
        if !out.status.success() {
            bail!("tesseract failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(parse_tsv(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// The languages Tesseract has data for (`tesseract --list-langs`), without `osd`
/// (orientation detection, not a language).
pub fn languages() -> Vec<String> {
    let Ok(out) = std::process::Command::new("tesseract").arg("--list-langs").output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "osd")
        .map(str::to_string)
        .collect()
}

struct LineAcc {
    /// (block, paragraph, line): line numbers restart in each block and paragraph.
    num: (i32, i32, i32),
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    words: Vec<(String, f32)>,
}

fn parse_tsv(tsv: &str) -> OcrResult {
    let mut lines: Vec<OcrLine> = Vec::new();
    let mut grouped: Vec<LineAcc> = Vec::new();
    for row in tsv.lines().skip(1) {
        let cols: Vec<&str> = row.split('\t').collect();
        if cols.len() < 12 {
            continue;
        }
        let level: i32 = cols[0].parse().unwrap_or(0);
        if level != 5 {
            continue;
        }
        let num = |i: usize| cols[i].parse::<i32>().unwrap_or(0);
        let line_num = (num(2), num(3), num(4));
        let left: i32 = cols[6].parse().unwrap_or(0);
        let top: i32 = cols[7].parse().unwrap_or(0);
        let width: i32 = cols[8].parse().unwrap_or(0);
        let height: i32 = cols[9].parse().unwrap_or(0);
        let conf: f32 = cols[10].parse().unwrap_or(-1.0);
        let text = cols[11].trim();
        if text.is_empty() {
            continue;
        }
        match grouped.last_mut() {
            Some(g) if g.num == line_num => {
                g.x = g.x.min(left);
                g.y = g.y.min(top);
                g.w = (left + width).max(g.x + g.w) - g.x;
                g.h = (top + height).max(g.y + g.h) - g.y;
                g.words.push((text.to_string(), conf));
            }
            _ => grouped.push(LineAcc {
                num: line_num,
                x: left,
                y: top,
                w: width,
                h: height,
                words: vec![(text.to_string(), conf)],
            }),
        }
    }
    for g in grouped {
        let text = g.words.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(" ");
        let confs: Vec<f32> = g.words.iter().map(|(_, c)| *c).filter(|c| *c >= 0.0).collect();
        let conf = if confs.is_empty() { -1.0 } else { confs.iter().sum::<f32>() / confs.len() as f32 };
        lines.push(OcrLine { text, x: g.x, y: g.y, w: g.w, h: g.h, conf });
    }
    let text = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
    OcrResult { text, engine: "tesseract".into(), lines, fallback_used: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_word_rows_into_lines() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t1\t1\t1\t1\t10\t10\t40\t12\t90\tHello\n\
5\t1\t1\t1\t1\t2\t55\t10\t40\t12\t88\tworld\n\
5\t1\t1\t1\t2\t1\t10\t28\t50\t12\t80\tSettings\n";
        let r = parse_tsv(tsv);
        assert_eq!(r.lines.len(), 2);
        assert_eq!(r.lines[0].text, "Hello world");
        assert_eq!(r.lines[1].text, "Settings");
        assert_eq!(r.text, "Hello world\nSettings");
        assert!((r.lines[0].conf - 89.0).abs() < 0.1);
    }

    #[test]
    fn first_lines_of_two_blocks_stay_apart() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t1\t1\t1\t1\t10\t10\t40\t12\t90\tName\n\
5\t1\t2\t1\t1\t1\t300\t10\t40\t12\t90\tPassword\n";
        let r = parse_tsv(tsv);
        assert_eq!(r.lines.len(), 2);
        assert_eq!(r.lines[1].text, "Password");
        assert_eq!(r.lines[1].x, 300);
    }
}
