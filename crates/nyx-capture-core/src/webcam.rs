//! Picture-in-picture webcam via mpv, with the WebcamOverlay window
//! class that Omarchy's and NyxOS's Hyprland rules float, place and pin.

use crate::detect;
use anyhow::{Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static CHILD: Mutex<Option<Child>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Small,
    Medium,
    Large,
}

impl Size {
    pub fn parse(s: &str) -> Self {
        match s {
            "small" => Self::Small,
            "large" => Self::Large,
            _ => Self::Medium,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    pub fn app_id(self) -> String {
        format!("WebcamOverlay-{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub path: String,
    pub name: String,
}

pub fn available() -> bool {
    detect::present("mpv")
}

pub fn list_devices() -> Vec<Device> {
    if detect::present("v4l2-ctl")
        && let Ok(out) = Command::new("v4l2-ctl").arg("--list-devices").output()
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        let found: Vec<Device> = parse_v4l2_devices(&text)
            .into_iter()
            .filter(|d| capture_capable(&d.path))
            .collect();
        if !found.is_empty() {
            return found;
        }
    }
    (0..8)
        .map(|i| PathBuf::from(format!("/dev/video{i}")))
        .filter(|p| p.exists())
        .map(|p| Device { path: p.to_string_lossy().into_owned(), name: "Camera".into() })
        .collect()
}

/// Command line for tests and for spawn.
pub fn argv(device: &str, size: Size, capture_options: &str) -> Vec<String> {
    vec![
        "mpv".into(),
        format!("av://v4l2:{device}"),
        "--profile=low-latency".into(),
        "--untimed".into(),
        "--no-cache".into(),
        format!("--demuxer-lavf-o={capture_options}"),
        "--vf=lavfi=[crop=ih*8/9:ih]".into(),
        "--title=WebcamOverlay".into(),
        format!("--wayland-app-id={}", size.app_id()),
        "--no-border".into(),
        "--no-audio".into(),
        "--no-osc".into(),
        "--osd-level=0".into(),
        "--really-quiet".into(),
    ]
}

pub fn start(device: Option<&str>, size: Size, settle: bool) -> Result<u32> {
    stop();
    if !available() {
        bail!("webcam overlay needs mpv");
    }
    let devices = list_devices();
    let path = match device.filter(|d| !d.is_empty()) {
        Some(d) => d.to_string(),
        None => devices.first().map(|d| d.path.clone()).ok_or_else(|| anyhow::anyhow!("no webcam devices found"))?,
    };
    if !Path::new(&path).exists() {
        bail!("{path} is not a camera");
    }
    let options = capture_options(&path);
    let args = argv(&path, size, &options);
    let child = spawn(&args)?;
    let pid = child.id();
    if let Ok(mut slot) = CHILD.lock() {
        *slot = Some(child);
    }
    write_pid(pid);
    wait_mapped();
    if settle {
        std::thread::sleep(Duration::from_millis(600));
    }
    Ok(pid)
}

pub fn stop() {
    if let Ok(mut slot) = CHILD.lock()
        && let Some(mut child) = slot.take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
    if let Some(pid) = read_pid() {
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
    }
    let _ = fs::remove_file(pid_path());
}

pub fn active() -> bool {
    if let Some(pid) = read_pid() {
        return Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false);
    }
    false
}

fn capture_options(device: &str) -> String {
    let formats = Command::new("v4l2-ctl")
        .args(["--list-formats-ext", "-d", device])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    for res in ["640x360", "1280x720", "1920x1080"] {
        if formats.contains(res) {
            return format!("video_size={res},framerate=30");
        }
    }
    "framerate=30".into()
}

fn capture_capable(device: &str) -> bool {
    let Ok(out) = Command::new("v4l2-ctl").args(["--device", device, "--info"]).output() else {
        return true;
    };
    if !out.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut inspect = false;
    for line in text.lines() {
        if line.contains("Device Caps") {
            inspect = true;
            continue;
        }
        if inspect && line.contains("Video Capture") {
            return true;
        }
        if inspect && !line.starts_with(' ') && !line.starts_with('\t') {
            inspect = false;
        }
    }
    false
}

fn parse_v4l2_devices(text: &str) -> Vec<Device> {
    let mut name = String::new();
    let mut emitted = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.is_empty() && !line.starts_with([' ', '\t']) {
            name = line.trim_end_matches(':').trim().to_string();
            emitted = false;
            continue;
        }
        if emitted {
            continue;
        }
        let device = line.trim();
        if device.starts_with("/dev/video") {
            emitted = true;
            out.push(Device { path: device.into(), name: name.clone() });
        }
    }
    out
}

fn spawn(args: &[String]) -> Result<Child> {
    let (bin, rest) = args.split_first().expect("argv");
    Command::new(bin).args(rest).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(Into::into)
}

fn wait_mapped() {
    if !detect::present("hyprctl") {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if mapped() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn mapped() -> bool {
    let Ok(out) = Command::new("hyprctl").args(["-j", "clients"]).output() else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout).contains("WebcamOverlay")
}

fn pid_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join("nyx-capture").join("webcam.pid")
}

fn write_pid(pid: u32) {
    let path = pid_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, pid.to_string());
}

fn read_pid() -> Option<u32> {
    fs::read_to_string(pid_path()).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpv_argv_uses_omarchy_class() {
        let args = argv("/dev/video0", Size::Medium, "framerate=30");
        assert_eq!(args[0], "mpv");
        assert!(args.contains(&"av://v4l2:/dev/video0".into()));
        assert!(args.contains(&"--title=WebcamOverlay".into()));
        assert!(args.contains(&"--wayland-app-id=WebcamOverlay-medium".into()));
        assert!(args.contains(&"--vf=lavfi=[crop=ih*8/9:ih]".into()));
    }

    #[test]
    fn size_parse_defaults_medium() {
        assert_eq!(Size::parse("small"), Size::Small);
        assert_eq!(Size::parse("large"), Size::Large);
        assert_eq!(Size::parse("nope"), Size::Medium);
        assert_eq!(Size::Medium.app_id(), "WebcamOverlay-medium");
    }

    #[test]
    fn parses_v4l2_list() {
        let text = "Integrated Camera (usb-0000:00:14.0-4):\n\t/dev/video0\n\t/dev/video1\n\nDummy:\n\t/dev/video2\n";
        let d = parse_v4l2_devices(text);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].path, "/dev/video0");
        assert!(d[0].name.contains("Integrated Camera"));
        assert_eq!(d[1].path, "/dev/video2");
    }
}
