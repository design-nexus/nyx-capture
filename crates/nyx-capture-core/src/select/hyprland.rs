//! hyprctl JSON: monitors and clients in layout coordinates.

use super::{Monitor, Window};
use crate::detect;
use crate::geom::Rect;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::process::{Command, Stdio};

#[derive(Deserialize)]
struct HyprMonitor {
    name: String,
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    scale: f64,
    #[serde(default)]
    transform: i32,
    focused: bool,
    #[serde(rename = "activeWorkspace", default)]
    active_workspace: Option<HyprWorkspace>,
    /// Shown over the active one when open; id 0 when none is.
    #[serde(rename = "specialWorkspace", default)]
    special_workspace: Option<HyprWorkspace>,
}

#[derive(Deserialize)]
struct HyprClient {
    class: String,
    title: String,
    at: [i32; 2],
    size: [i32; 2],
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    mapped: bool,
    #[serde(rename = "focusHistoryID", default)]
    focus_history_id: i32,
    workspace: HyprWorkspace,
}

#[derive(Deserialize)]
struct HyprWorkspace {
    #[serde(default)]
    id: i32,
}

pub fn monitors() -> Result<Vec<Monitor>> {
    let raw = hyprctl(&["monitors", "-j"])?;
    let list: Vec<HyprMonitor> = serde_json::from_str(&raw).context("hyprctl monitors")?;
    Ok(list
        .into_iter()
        .map(|m| {
            let mut w = (m.width as f64 / m.scale).floor() as i32;
            let mut h = (m.height as f64 / m.scale).floor() as i32;
            // 1 and 3 rotate 90° and 270°; 5 and 7 are the same, flipped.
            if matches!(m.transform, 1 | 3 | 5 | 7) {
                std::mem::swap(&mut w, &mut h);
            }
            Monitor { name: m.name, geometry: Rect::new(m.x, m.y, w.max(1), h.max(1)), scale: m.scale, focused: m.focused }
        })
        .collect())
}

/// Workspaces on screen now: each monitor's active one and any open special one.
fn visible_workspaces() -> Option<Vec<i32>> {
    let raw = hyprctl(&["monitors", "-j"]).ok()?;
    let list: Vec<HyprMonitor> = serde_json::from_str(&raw).ok()?;
    Some(
        list.iter()
            .flat_map(|m| [m.active_workspace.as_ref(), m.special_workspace.as_ref()])
            .flatten()
            .map(|w| w.id)
            .filter(|&id| id != 0)
            .collect(),
    )
}

pub fn windows() -> Result<Vec<Window>> {
    let raw = hyprctl(&["clients", "-j"])?;
    let list: Vec<HyprClient> = serde_json::from_str(&raw).context("hyprctl clients")?;
    // Windows on other workspaces aren't on screen; snapping to one would pick nothing
    // the person can see. Without the monitor list, keep them all.
    let shown = visible_workspaces();
    Ok(list
        .into_iter()
        .filter(|c| c.mapped && !c.hidden)
        .filter(|c| shown.as_ref().is_none_or(|ids| ids.contains(&c.workspace.id)))
        .map(|c| Window {
            class: c.class,
            title: c.title,
            geometry: Rect::new(c.at[0], c.at[1], c.size[0], c.size[1]),
            workspace: c.workspace.id,
            focused: c.focus_history_id == 0,
            hidden: c.hidden,
        })
        .collect())
}

fn hyprctl(args: &[&str]) -> Result<String> {
    if !detect::present("hyprctl") {
        bail!("hyprctl isn't available");
    }
    let out = Command::new("hyprctl").args(args).stdin(Stdio::null()).output().context("hyprctl")?;
    if !out.status.success() {
        bail!("hyprctl failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_json_accounts_for_scale() {
        let raw = r#"[{"name":"eDP-2","width":2560,"height":1600,"x":0,"y":0,"scale":1.6,"transform":0,"focused":true}]"#;
        let list: Vec<HyprMonitor> = serde_json::from_str(raw).unwrap();
        let m = &list[0];
        let w = (m.width as f64 / m.scale).floor() as i32;
        let h = (m.height as f64 / m.scale).floor() as i32;
        assert_eq!((w, h), (1600, 1000));
    }

    #[test]
    fn monitor_json_allows_empty_special_workspace_without_id() {
        let raw = r#"[{"name":"eDP-1","width":2560,"height":1600,"x":0,"y":0,"scale":1.6,"transform":0,"focused":true,"activeWorkspace":{"address":"1","id":1,"type":"normal","name":"1"},"specialWorkspace":{"address":"","type":"","name":""}}]"#;
        let list: Vec<HyprMonitor> = serde_json::from_str(raw).unwrap();
        assert_eq!(list[0].active_workspace.as_ref().map(|w| w.id), Some(1));
        assert_eq!(list[0].special_workspace.as_ref().map(|w| w.id), Some(0));
    }
}
