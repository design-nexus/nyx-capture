//! Sharing through Nyx Share: the Share service opens its Share Sheet for the
//! picture or recording. Captures are sent as read-only descriptors; edited
//! pictures as their rendered PNG, never by changing the original.

use nyx_share_client::{Client, Content, ContentKind, ShareOptions};
use std::path::PathBuf;

fn send(content: Content) -> Result<(), String> {
    let options = ShareOptions { app_name: Some("Capture".into()), ..Default::default() };
    Client::connect().and_then(|c| c.share(&content, &options)).map(|_| ()).map_err(|e| e.to_string())
}

/// A picture as rendered (marks burned in), named like the capture.
pub fn png(bytes: Vec<u8>, name: String, done: impl FnOnce(Result<(), String>) + 'static) {
    crate::cmd::background(
        move || {
            let mut content = Content::new();
            content.bytes(ContentKind::Image, &name, Some("image/png"), &bytes).map_err(|e| e.to_string())?;
            send(content)
        },
        done,
    );
}

/// A capture from the gallery: an edited picture as rendered, anything else as it is.
pub fn capture(path: PathBuf, recording: bool, done: impl FnOnce(Result<(), String>) + 'static) {
    crate::cmd::background(
        move || {
            let mut content = Content::new();
            if recording {
                content.file(&path).map_err(|e| e.to_string())?;
            } else {
                let bytes = nyx_capture_core::export::shareable_png(&path).map_err(|e| e.to_string())?;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Capture.png".into());
                content.bytes(ContentKind::Image, &name, Some("image/png"), &bytes).map_err(|e| e.to_string())?;
            }
            send(content)
        },
        done,
    );
}
