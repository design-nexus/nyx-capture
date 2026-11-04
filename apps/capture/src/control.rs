//! Tools Nyx Assistant can call while Capture is open.

use crate::window;
use nyx_capture_core::capture::CaptureMode;
use nyx_core::control::{Reply, text};
use serde_json::{Value, json};

pub fn tools() -> Value {
    json!([
        {"name": "capture.region", "description": "Drag a screen region and copy the screenshot.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "capture.window", "description": "Pick a window and copy the screenshot.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "capture.fullscreen", "description": "Capture the focused monitor and copy the screenshot.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "capture.text", "description": "Drag a screen region and copy the text in it.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "capture.colour", "description": "Pick a pixel on screen and copy its colour.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "capture.search", "description": "Search screenshot history by OCR text.", "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}},
        {"name": "capture.open", "description": "Open a capture in the annotation editor by id.", "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}},
        {"name": "capture.ocr_text", "description": "Return stored OCR text for a capture id.", "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}},
        {"name": "capture.export", "description": "Write a flattened PNG of a capture inside the home folder. The original file stays as it is.", "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}, "path": {"type": "string"}, "overwrite": {"type": "boolean"}}, "required": ["id", "path"]}},
        {"name": "capture.record_start", "description": "Start a screen recording of the focused monitor.", "inputSchema": {"type": "object", "properties": {"region": {"type": "boolean"}}}},
        {"name": "capture.record_stop", "description": "Stop the current screen recording and save the file.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "capture.record_pause", "description": "Pause or resume the current recording when the backend supports it.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "capture.send_screenshot", "description": "Capture the focused monitor or return OCR for a history id. Refused until Capture settings allow Assistant to send a screenshot.", "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}}}
    ])
}

pub fn handle(name: &str, args: Value, reply: Reply) {
    let result = match name {
        "capture.region" => start(CaptureMode::SelectRegion),
        "capture.window" => start(CaptureMode::SelectWindow),
        "capture.fullscreen" => start(CaptureMode::FullDesktop),
        "capture.text" => {
            window::capture_with(CaptureMode::SelectRegion, nyx_capture_core::action::After::Ocr, 0);
            Ok(json!({"started": true}))
        }
        "capture.colour" => {
            window::open_bar("colour");
            Ok(json!({"started": true}))
        }
        "capture.search" => search(&args),
        "capture.open" => open(&args),
        "capture.ocr_text" => ocr_text(&args),
        "capture.export" => export(&args),
        "capture.record_start" => {
            window::start_recording(args["region"] == true);
            Ok(json!({"started": true}))
        }
        "capture.record_stop" => {
            window::stop_recording();
            Ok(json!({"stopped": true}))
        }
        "capture.record_pause" => {
            window::pause_recording();
            Ok(json!({"toggled": true}))
        }
        "capture.send_screenshot" => send_screenshot(&args),
        other => Err(format!("unknown tool {other}")),
    };
    reply(result);
}

fn start(mode: CaptureMode) -> Result<Value, String> {
    window::capture_now(mode);
    Ok(json!({"started": true}))
}

fn search(args: &Value) -> Result<Value, String> {
    let query = args["query"].as_str().unwrap_or("");
    window::search(query).map_err(|e| e.to_string())
}

fn open(args: &Value) -> Result<Value, String> {
    let id = args["id"].as_str().unwrap_or("");
    window::open_id(id).map_err(|e| e.to_string())
}

fn ocr_text(args: &Value) -> Result<Value, String> {
    let id = args["id"].as_str().unwrap_or("");
    window::ocr_text(id).map_err(|e| e.to_string())
}

fn export(args: &Value) -> Result<Value, String> {
    let id = text(args, "id")?;
    let dest = nyx_core::control::save_target(args)?;
    window::export_id(&id, &dest).map_err(|e| e.to_string())
}

fn send_screenshot(args: &Value) -> Result<Value, String> {
    if !crate::prefs::get().assistant_can_send {
        return Err("Assistant can't send a screenshot until you turn that on in Capture settings.".into());
    }
    let id = args["id"].as_str().unwrap_or("");
    if id.is_empty() {
        window::capture_now(CaptureMode::FullDesktop);
        return Ok(json!({"started": true, "mode": "fullscreen"}));
    }
    window::send_screenshot(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_is_a_tool() {
        let tools = tools();
        let names: Vec<_> = tools.as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"capture.export"));
        assert!(names.contains(&"capture.open"));
        assert!(names.contains(&"capture.record_start"));
        assert!(names.contains(&"capture.record_stop"));
        assert!(names.contains(&"capture.record_pause"));
        assert!(names.contains(&"capture.send_screenshot"));
        assert!(names.contains(&"capture.text"));
        assert!(names.contains(&"capture.colour"));
        assert!(!names.iter().any(|n| n.contains("qr")));
    }
}
