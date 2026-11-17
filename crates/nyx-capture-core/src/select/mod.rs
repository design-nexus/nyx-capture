//! Windows and monitors. Hyprland via hyprctl; empty elsewhere.

mod hyprland;

use crate::geom::Rect;
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Monitor {
    pub name: String,
    pub geometry: Rect,
    pub scale: f64,
    pub focused: bool,
}

#[derive(Debug, Clone)]
pub struct Window {
    pub class: String,
    pub title: String,
    pub geometry: Rect,
    pub workspace: i32,
    pub focused: bool,
    pub hidden: bool,
}

pub fn monitors() -> Result<Vec<Monitor>> {
    hyprland::monitors()
}

pub fn windows() -> Result<Vec<Window>> {
    hyprland::windows()
}

pub fn active_window() -> Result<Option<Window>> {
    Ok(windows()?.into_iter().find(|w| w.focused && !w.hidden))
}

/// A title for a capture taken now: the focused window's title, or its class, at most
/// 120 characters; empty when there's none.
pub fn capture_title() -> String {
    let Ok(Some(w)) = active_window() else { return String::new() };
    let text = if w.title.trim().is_empty() { w.class } else { w.title };
    text.trim().chars().take(120).collect()
}

pub fn focused_monitor() -> Result<Option<Monitor>> {
    Ok(monitors()?.into_iter().find(|m| m.focused))
}

pub fn monitor_named(name: &str) -> Result<Option<Monitor>> {
    Ok(monitors()?.into_iter().find(|m| m.name == name))
}

pub fn desktop_bounds() -> Result<Rect> {
    let list = monitors()?;
    if list.is_empty() {
        anyhow::bail!("no monitors");
    }
    let min_x = list.iter().map(|m| m.geometry.x).min().unwrap();
    let min_y = list.iter().map(|m| m.geometry.y).min().unwrap();
    let max_x = list.iter().map(|m| m.geometry.x + m.geometry.w).max().unwrap();
    let max_y = list.iter().map(|m| m.geometry.y + m.geometry.h).max().unwrap();
    Ok(Rect::new(min_x, min_y, max_x - min_x, max_y - min_y))
}

pub fn snap_rect_at(point: (i32, i32), candidates: &[Rect]) -> Option<Rect> {
    candidates.iter().copied().filter(|r| r.contains(point.0, point.1)).min_by_key(|r| r.area())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_picks_smallest_containing_rect() {
        let monitor = Rect::new(0, 0, 1600, 1000);
        let window = Rect::new(100, 100, 400, 300);
        assert_eq!(snap_rect_at((120, 120), &[monitor, window]), Some(window));
        assert_eq!(snap_rect_at((10, 10), &[monitor, window]), Some(monitor));
        assert_eq!(snap_rect_at((-1, 0), &[monitor, window]), None);
    }
}
