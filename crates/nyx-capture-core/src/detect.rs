//! What is installed. Settings hide anything that is missing.

use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Tools {
    pub grim: bool,
    pub portal: bool,
    pub slurp: bool,
    pub hyprctl: bool,
    pub tesseract: bool,
    pub wl_copy: bool,
    pub gpu_screen_recorder: bool,
    pub wf_recorder: bool,
    pub wl_screenrec: bool,
    pub ffmpeg: bool,
    pub mpv: bool,
    pub gst_launch: bool,
}

impl Tools {
    pub fn probe() -> Self {
        Self {
            grim: present("grim"),
            portal: present("xdg-desktop-portal"),
            slurp: present("slurp"),
            hyprctl: present("hyprctl"),
            tesseract: present("tesseract"),
            wl_copy: present("wl-copy"),
            gpu_screen_recorder: present("gpu-screen-recorder"),
            wf_recorder: present("wf-recorder"),
            wl_screenrec: present("wl-screenrec"),
            ffmpeg: present("ffmpeg"),
            mpv: present("mpv"),
            gst_launch: present("gst-launch-1.0"),
        }
    }

    pub fn can_capture(&self) -> bool {
        self.grim || self.portal
    }

    pub fn can_region(&self) -> bool {
        self.grim && self.slurp
    }

    pub fn can_windows(&self) -> bool {
        self.grim && self.hyprctl
    }

    pub fn can_ocr(&self) -> bool {
        self.tesseract || crate::ocr::models_present()
    }

    pub fn can_copy_image(&self) -> bool {
        self.wl_copy
    }

    pub fn can_record(&self) -> bool {
        self.gpu_screen_recorder || self.wf_recorder || self.wl_screenrec || self.can_gst_record()
    }

    pub fn can_gst_record(&self) -> bool {
        self.gst_launch && self.portal
    }

    pub fn can_webcam(&self) -> bool {
        self.mpv
    }

    /// Capture's own stitcher needs only grim.
    pub fn can_scroll(&self) -> bool {
        self.grim
    }
}

pub fn present(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_program_is_false() {
        assert!(!present("nyx-capture-no-such-binary"));
    }
}
