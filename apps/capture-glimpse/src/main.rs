//! Glimpse plugin. It opens `capture.sqlite` read-only.

use anyhow::{Result, bail};
use glimpse_plugin_sdk::{Action, Hit, Request, serve};
use nyx_capture_core::search::library_query;
use nyx_capture_core::store::{CaptureRow, open_readonly};
use serde_json::{Value, json};
use std::path::PathBuf;

fn main() {
    if let Err(error) = serve(&["search", "preview", "reader"], answer) {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn library() -> PathBuf {
    if let Some(path) = std::env::var_os("NYX_CAPTURE_DB") {
        return PathBuf::from(path);
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    data.unwrap_or_else(|| PathBuf::from(".local/share")).join("nyx-capture/capture.sqlite")
}

fn answer(request: &Request) -> Result<Value> {
    match request.op.as_str() {
        "search" => search(request),
        "preview" => preview(request),
        "activate" => activate(request),
        "extract" => extract(request),
        other => bail!("unsupported operation {other}"),
    }
}

fn search(request: &Request) -> Result<Value> {
    let query = request.args["query"].as_str().unwrap_or("");
    let limit = request.args["limit"].as_i64().unwrap_or(10).clamp(1, 10);
    let mut hits = Vec::new();
    if let Some(hit) = command_hit(query) {
        hits.push(hit);
        return Ok(serde_json::to_value(hits)?);
    }
    let asked = library_query(query);
    let db = open_readonly(&library()).map_err(|_| anyhow::anyhow!("Capture has no library yet."))?;
    let items = db.list(asked.view, &asked.text, limit)?;
    for row in items {
        let text = if row.sensitive { String::new() } else { db.ocr_text(&row.id)?.unwrap_or_default() };
        hits.push(library_hit(row, text));
    }
    Ok(serde_json::to_value(hits)?)
}

fn preview(request: &Request) -> Result<Value> {
    let id = request.args["result_id"].as_str().unwrap_or("");
    if id.is_empty() {
        bail!("missing capture");
    }
    if let Some(text) = command_preview(id) {
        return Ok(json!({"text": text}));
    }
    let db = open_readonly(&library()).map_err(|_| anyhow::anyhow!("Capture has no library yet."))?;
    let Some(row) = db.get(id)? else { bail!("that capture is not in the library") };
    let ocr = if row.sensitive { String::new() } else { db.ocr_text(id)?.unwrap_or_default() };
    let title = if row.title.is_empty() {
        row.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Capture".into())
    } else {
        row.title
    };
    let mut lines = vec![title];
    if !ocr.is_empty() {
        lines.push(ocr);
    }
    Ok(json!({"text": lines.join("\n")}))
}

fn activate(request: &Request) -> Result<Value> {
    let token = request.args["token"].as_str().unwrap_or("");
    let flag = match token {
        "region" => "--region",
        "window" => "--window",
        "fullscreen" => "--fullscreen",
        "scroll" => "--scroll",
        "text" => "--text",
        "qr" => "--qr",
        "colour" => "--colour",
        "clipboard" => "--clipboard",
        _ => bail!("unknown capture command"),
    };
    // The plugin cannot spawn GTK. Until Glimpse is allowed to launch nyx-capture,
    // activating a command copies the CLI so it can be run outside the sandbox.
    Ok(serde_json::to_value(Action::Copy { text: format!("nyx-capture {flag}") })?)
}

fn command_hit(query: &str) -> Option<Hit> {
    let token = match query.trim().to_ascii_lowercase().as_str() {
        "capture region" => "region",
        "capture window" => "window",
        "capture screen" | "capture fullscreen" => "fullscreen",
        "capture scroll" => "scroll",
        "capture text" | "copy text from screen" | "ocr" => "text",
        "scan qr code" | "scan qr" | "capture qr" | "qr code" => "qr",
        "pick colour" | "pick color" | "colour picker" | "color picker" => "colour",
        "edit clipboard image" | "annotate clipboard" => "clipboard",
        _ => return None,
    };
    let (title, subtitle) = match token {
        "region" => ("Capture a region", "Drag a region on screen"),
        "window" => ("Capture a window", "Pick a window"),
        "scroll" => ("Scrolling capture", "Scroll a page; Capture stitches it into one tall shot"),
        "text" => ("Capture text", "Copy the text in an area of the screen"),
        "qr" => ("Scan a QR code", "Copy what a QR code on screen says"),
        "colour" => ("Pick a colour", "Copy the colour of a pixel on screen"),
        "clipboard" => ("Edit the clipboard image", "Open the copied image in the editor"),
        _ => ("Capture the screen", "The focused monitor"),
    };
    Some(Hit { id: format!("cmd:{token}"), title: title.into(), subtitle: subtitle.into(), action: Action::Activate { token: token.into() } })
}

fn command_preview(id: &str) -> Option<String> {
    match id {
        "cmd:region" => Some("Drag a region. Run nyx-capture --region.".into()),
        "cmd:window" => Some("Pick a window. Run nyx-capture --window.".into()),
        "cmd:fullscreen" => Some("Capture the focused monitor. Run nyx-capture --fullscreen.".into()),
        "cmd:scroll" => Some("Scroll a region. Run nyx-capture --scroll.".into()),
        "cmd:text" => Some("Copy the text in an area. Run nyx-capture --text.".into()),
        "cmd:qr" => Some("Copy what a QR code says. Run nyx-capture --qr.".into()),
        "cmd:colour" => Some("Copy a pixel's colour. Run nyx-capture --colour.".into()),
        "cmd:clipboard" => Some("Annotate the copied image. Run nyx-capture --clipboard.".into()),
        _ => None,
    }
}

fn extract(request: &Request) -> Result<Value> {
    let path = request.args["path"].as_str().unwrap_or("");
    if path.is_empty() {
        bail!("missing path");
    }
    let db = open_readonly(&library()).map_err(|_| anyhow::anyhow!("Capture has no library yet."))?;
    let Some(row) = db.get_by_path(std::path::Path::new(path))? else {
        return Ok(json!([]));
    };
    if row.sensitive {
        return Ok(json!([]));
    }
    let ocr = db.ocr_text(&row.id)?.unwrap_or_default();
    if ocr.trim().is_empty() {
        return Ok(json!([]));
    }
    Ok(json!([{"text": ocr}]))
}

fn library_hit(row: CaptureRow, ocr: String) -> Hit {
    let title = if row.title.is_empty() {
        row.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Capture".into())
    } else {
        row.title
    };
    let subtitle = if ocr.trim().is_empty() { "No text".into() } else { ocr.lines().next().unwrap_or("").to_string() };
    Hit { id: row.id, title, subtitle, action: Action::Copy { text: ocr } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_capture_core::store::{CaptureKind, NewCapture, open};
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn commands_do_not_need_a_library() {
        let _guard = LOCK.lock().unwrap();
        unsafe { std::env::remove_var("NYX_CAPTURE_DB") };
        let hits = answer(&request("search", json!({"query": "capture region", "limit": 10}))).unwrap();
        assert_eq!(hits[0]["id"], "cmd:region");
        assert_eq!(hits[0]["action"]["kind"], "activate");
        assert_eq!(hits[0]["action"]["token"], "region");
        let preview = answer(&request("preview", json!({"result_id": "cmd:region"}))).unwrap();
        assert!(preview["text"].as_str().unwrap().contains("nyx-capture --region"));
        let action = answer(&request("activate", json!({"token": "region"}))).unwrap();
        assert_eq!(action["kind"], "copy");
        assert_eq!(action["text"], "nyx-capture --region");
        let screen = answer(&request("search", json!({"query": "capture screen"}))).unwrap();
        assert_eq!(screen[0]["action"]["token"], "fullscreen");
        let scroll = answer(&request("search", json!({"query": "capture scroll"}))).unwrap();
        assert_eq!(scroll[0]["action"]["token"], "scroll");
        let action = answer(&request("activate", json!({"token": "scroll"}))).unwrap();
        assert_eq!(action["text"], "nyx-capture --scroll");
        for (query, flag) in [("scan qr code", "--qr"), ("pick colour", "--colour"), ("capture text", "--text"), ("edit clipboard image", "--clipboard")] {
            let hit = answer(&request("search", json!({"query": query}))).unwrap();
            let token = hit[0]["action"]["token"].as_str().unwrap().to_string();
            let action = answer(&request("activate", json!({"token": token}))).unwrap();
            assert_eq!(action["text"], format!("nyx-capture {flag}"));
        }
    }

    #[test]
    fn search_and_preview_copy_ocr() {
        let _guard = LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("nyx-capture-glimpse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("capture.sqlite");
        let id = {
            let db = open(&path).unwrap();
            let id = db
                .insert(&NewCapture {
                    kind: CaptureKind::Screenshot,
                    width: 10,
                    height: 10,
                    path: dir.join("shot.png"),
                    source: "grim".into(),
                })
                .unwrap();
            db.set_ocr(&id, "tesseract", "kernel panic on boot").unwrap();
            id
        };
        unsafe { std::env::set_var("NYX_CAPTURE_DB", &path) };

        let hits = answer(&request("search", json!({"query": "kernel", "limit": 10}))).unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 1);
        assert_eq!(hits[0]["id"], id);
        assert_eq!(hits[0]["action"]["kind"], "copy");
        assert_eq!(hits[0]["action"]["text"], "kernel panic on boot");
        let preview = answer(&request("preview", json!({"result_id": id}))).unwrap();
        let text = preview["text"].as_str().unwrap();
        assert!(text.contains("kernel panic on boot"));
        let extracted = answer(&request("extract", json!({"path": dir.join("shot.png").display().to_string()}))).unwrap();
        assert_eq!(extracted[0]["text"], "kernel panic on boot");

        {
            let db = open(&path).unwrap();
            db.set_sensitive(&id, true).unwrap();
        }
        let hits = answer(&request("search", json!({"query": "kernel", "limit": 10}))).unwrap();
        assert!(hits.as_array().unwrap().is_empty());
        let preview = answer(&request("preview", json!({"result_id": id}))).unwrap();
        assert!(!preview["text"].as_str().unwrap().contains("kernel panic on boot"));
        let extracted = answer(&request("extract", json!({"path": dir.join("shot.png").display().to_string()}))).unwrap();
        assert!(extracted.as_array().unwrap().is_empty());

        unsafe { std::env::remove_var("NYX_CAPTURE_DB") };
        let _ = std::fs::remove_dir_all(dir);
    }

    fn request(op: &str, args: Value) -> Request {
        Request { api_version: 1, id: 1, op: op.into(), args }
    }
}
