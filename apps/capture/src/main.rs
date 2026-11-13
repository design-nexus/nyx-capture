//! Capture — screenshots, annotation, and OCR for Omarchy.

mod card;
mod cmd;
mod control;
mod editor;
mod grid;
mod home;
mod naming;
mod notice;
mod ocr_view;
mod overlay;
mod pin;
mod paths;
mod prefs;
mod recording;
mod scrolling;
mod settings_dialog;
mod settings_page;
mod share;
mod theme;
mod widgets;
mod window;

use gtk::prelude::*;
use gtk::{gio, glib};
use nyx_capture_core::action::{self, Action, Outcome};
use nyx_capture_core::capture::CaptureMode;
use nyx_capture_core::ocr::OcrMode;

const DEFAULT_APP_ID: &str = "io.github.design_nexus.nyx.Capture";

/// The application ID: `NYX_CAPTURE_APP_ID` runs a test copy beside the real one.
pub fn app_id() -> String {
    std::env::var("NYX_CAPTURE_APP_ID").unwrap_or_else(|_| DEFAULT_APP_ID.into())
}

const USAGE: &str = "Usage: nyx-capture [OPTIONS]\n\
\n\
  --capture [MODE]    the capture bar (smart, window, screen, scroll, record, text,\n\
                      qr or colour; else Settings → Capture's Start with). Run again\n\
                      to close it, finish a scroll, or stop a recording\n\
  --cancel            cancel the capture bar or a scroll capture in progress\n\
  --region            drag a region\n\
  --window            pick a window\n\
  --active-window     the focused window\n\
  --monitor [NAME]    one monitor (focused if NAME is omitted)\n\
  --fullscreen        the focused monitor\n\
  --delay SECS        wait before capturing\n\
  --ocr, --text       copy the text in a region\n\
  --qr                copy what a QR code in a region says (marked sensitive)\n\
  --colour, --color   pick a pixel: copy its colour and print it\n\
  --ocr-file PATH     print the text in an image file (--json: {text, engine})\n\
  --pin               pin the shot after capture\n\
  --copy              put the image on the clipboard (default)\n\
  --save [PATH]       write a PNG\n\
  --edit              open the annotation editor\n\
  --scroll            the capture bar on Scroll: an area stitched as you scroll\n\
  --record            start a recording of the focused monitor\n\
  --record --region   pick a region, then record\n\
  --record --stop     stop the current recording\n\
  --record --pause    pause or resume (gpu-screen-recorder)\n\
  --record --webcam   picture-in-picture camera (mpv WebcamOverlay)\n\
  FILE, --file FILE   open an image in the editor (not added to history)\n\
  --clipboard         open the image on the clipboard in the editor\n\
  --section ID        open capture (the modes), recent, screenshots, recordings,\n\
                      starred, or settings\n\
  --toggle            close the window if it's open, otherwise open it\n";

fn main() -> glib::ExitCode {
    nyx_core::app::init(nyx_core::app::App { slug: "nyx-capture", name: "Capture" });
    // Windows without an application, like the editor, take their Wayland app ID from this.
    glib::set_prgname(Some(app_id().as_str()));
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return glib::ExitCode::SUCCESS;
    }

    if has_flag(&args, "--gst-worker") {
        return match nyx_capture_core::record::run_gst_worker() {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("{err}");
                glib::ExitCode::FAILURE
            }
        };
    }

    // Text from an image another app has (Nyx Clipboard's Extract text), on stdout.
    if let Some(path) = flag_value(&args, "--ocr-file") {
        return match ocr_file(path, has_flag(&args, "--json")) {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("nyx-capture: {err}");
                glib::ExitCode::FAILURE
            }
        };
    }

    // OCR for a shot a hotkey just put in history, run apart so the hotkey returns at once.
    if let Some(id) = flag_value(&args, "--ocr-id") {
        return match ocr_into_history(id) {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("nyx-capture: {err}");
                glib::ExitCode::FAILURE
            }
        };
    }


    if has_flag(&args, "--record") && !primary_running() {
        match run_record(&args) {
            Ok(()) => return glib::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("{err}");
                return glib::ExitCode::FAILURE;
            }
        }
    }

    if is_colour(&args) && !primary_running() {
        return match pick_colour_cold() {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("nyx-capture: {err}");
                glib::ExitCode::FAILURE
            }
        };
    }

    if let Some(action) = cli_action(&args)
        && !matches!(action.after, action::After::Edit | action::After::Pin)
        && !primary_running()
    {
        let title = nyx_capture_core::select::capture_title();
        // A QR code can hold a secret: its shot isn't kept.
        let keep = action.after != action::After::Qr;
        match run_cli(&action).map(|(outcome, shot)| {
            if let Some(shot) = shot.filter(|_| keep) {
                remember_cold(&shot, &title);
            }
            outcome
        }) {
            Ok(Outcome::Cancelled) => {}
            Ok(Outcome::Copied) => {}
            Ok(Outcome::Saved(path) | Outcome::Quick(path)) => eprintln!("{}", path.display()),
            Ok(Outcome::Text(text)) => {
                if !text.is_empty() {
                    eprintln!("{text}");
                }
            }
            Ok(Outcome::Qr(text)) => eprintln!("{}", nyx_capture_core::qr::classify(&text).copied()),
            Ok(Outcome::Colour(value)) => println!("{value}"),
            Err(err) => {
                eprintln!("{err}");
                return glib::ExitCode::FAILURE;
            }
        }
        return glib::ExitCode::SUCCESS;
    }

    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: still single-threaded; nothing else reads the environment yet.
        unsafe { std::env::set_var("GSK_RENDERER", "ngl") };
    }

    let app = gtk::Application::builder().application_id(app_id()).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
    app.connect_startup(|app| {
        theme::install();
        overlay::mark_theme_installed();
        nyx_core::control::serve_async(app, control::tools(), control::handle);
    });
    app.connect_command_line(|app, cl| {
        let argv: Vec<String> = cl.arguments().iter().map(|a| a.to_string_lossy().to_string()).collect();
        let has = |flag: &str| argv.iter().any(|a| a == flag);
        let value = |flag: &str| argv.iter().position(|a| a == flag).and_then(|i| argv.get(i + 1)).cloned();
        if has("--toggle")
            && let Some(w) = window::window()
            && w.is_visible()
        {
            w.close();
            return glib::ExitCode::SUCCESS;
        }
        // Esc while a scroll capture runs (Hyprland binds it only then): cancel it.
        if has("--cancel") {
            if overlay::is_open() {
                overlay::dismiss();
            } else if scrolling::is_active() {
                scrolling::cancel();
            }
            return glib::ExitCode::SUCCESS;
        }
        if has("--capture") {
            // `--capture record` (or smart, window, screen, scroll) opens the bar on that mode.
            let mode = value("--capture").filter(|v| !v.starts_with('-'));
            window::capture_from_key(app, mode.as_deref());
            return glib::ExitCode::SUCCESS;
        }
        if has("--scroll") {
            window::start_scroll();
            return glib::ExitCode::SUCCESS;
        }
        if is_colour(&argv) {
            window::capture_from_key(app, Some("colour"));
            return glib::ExitCode::SUCCESS;
        }
        if has("--clipboard") {
            window::edit_clipboard();
            return glib::ExitCode::SUCCESS;
        }
        if let Some(target) = image_arg(&argv) {
            // A relative path is the caller's, not this process's.
            let path = match target.strip_prefix("file://") {
                Some(_) => glib::filename_from_uri(&target).map(|(p, _)| p).unwrap_or_else(|_| target.clone().into()),
                None => std::path::PathBuf::from(&target),
            };
            let path = if path.is_relative() { cl.cwd().map(|d| d.join(&path)).unwrap_or(path) } else { path };
            window::edit_file(path);
            return glib::ExitCode::SUCCESS;
        }
        if has("--record") {
            if has("--stop") {
                window::stop_recording();
            } else if has("--pause") {
                window::pause_recording();
            } else {
                window::start_recording(has("--region"));
            }
            return glib::ExitCode::SUCCESS;
        }
        if let Some(action) = cli_action(&argv) {
            if matches!(action.after, action::After::Edit | action::After::Pin) || window::window().is_none() {
                window::present(app, value("--section").as_deref());
            }
            // A relative --save path is the caller's, not this process's.
            let save = action.save_path.map(|p| if p.is_relative() { cl.cwd().map(|d| d.join(&p)).unwrap_or(p) } else { p });
            window::capture_with_path(action.mode, action.after, action.delay, save);
            return glib::ExitCode::SUCCESS;
        }
        window::present(app, value("--section").as_deref());
        glib::ExitCode::SUCCESS
    });
    app.connect_shutdown(|_| {
        window::flush_deletes();
        prefs::flush();
    });
    app.run()
}

/// Session bus name of a live Capture window. A second `--region --copy` must
/// reach that process so two Exclusive overlays never fight.
fn primary_running() -> bool {
    name_owned(&app_id())
}

fn name_owned(name: &str) -> bool {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return false;
    };
    let Ok(reply) = bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(name,).to_variant()),
        Some(glib::VariantTy::new("(b)").unwrap()),
        gio::DBusCallFlags::NONE,
        200,
        gio::Cancellable::NONE,
    ) else {
        return false;
    };
    reply.child_value(0).get::<bool>().unwrap_or(false)
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(|s| s.as_str()).filter(|s| !s.starts_with('-'))
}

/// The image to edit: `--file PATH`, or a lone path (or file:// URL) as the only argument.
fn image_arg(args: &[String]) -> Option<String> {
    if let Some(v) = flag_value(args, "--file") {
        return Some(v.to_string());
    }
    match args {
        [_, only] if !only.starts_with('-') => Some(only.clone()),
        _ => None,
    }
}

fn is_colour(args: &[String]) -> bool {
    has_flag(args, "--colour") || has_flag(args, "--color")
}

/// `--colour` with no Capture window: pick a pixel, copy its value, print it.
fn pick_colour_cold() -> anyhow::Result<()> {
    let Some(rgb) = overlay::run_colour_cli()? else { return Ok(()) };
    let value = nyx_capture_core::colour::format(rgb, nyx_capture_core::colour::Format::parse(&prefs::get().colour_format));
    nyx_capture_core::clipboard::copy_text(&value)?;
    println!("{value}");
    Ok(())
}

fn cli_action(args: &[String]) -> Option<Action> {
    let mode = if has_flag(args, "--region") {
        CaptureMode::SelectRegion
    } else if has_flag(args, "--window") {
        CaptureMode::SelectWindow
    } else if has_flag(args, "--active-window") {
        CaptureMode::ActiveWindow
    } else if has_flag(args, "--fullscreen") {
        CaptureMode::FullDesktop
    } else if has_flag(args, "--monitor") {
        match flag_value(args, "--monitor") {
            Some(name) => CaptureMode::Monitor(name.to_string()),
            None => CaptureMode::FullDesktop,
        }
    } else if ["--ocr", "--text", "--qr", "--pin", "--edit"].iter().any(|f| has_flag(args, f)) {
        CaptureMode::SelectRegion
    } else {
        return None;
    };
    let delay = flag_value(args, "--delay").and_then(|s| s.parse().ok()).unwrap_or(0);
    let save = has_flag(args, "--save");
    let ocr = has_flag(args, "--ocr") || has_flag(args, "--text");
    let qr = has_flag(args, "--qr");
    let pin = has_flag(args, "--pin");
    let edit = has_flag(args, "--edit");
    let copy = has_flag(args, "--copy") || (!save && !ocr && !qr && !pin && !edit);
    let after = if qr { action::After::Qr } else { action::parse_after(copy, save, ocr, pin, edit).ok()? };
    let save_path = if save {
        Some(flag_value(args, "--save").map(std::path::PathBuf::from).unwrap_or_else(|| naming::next_path("")))
    } else {
        None
    };
    let prefs = prefs::get();
    Some(Action {
        mode,
        after,
        delay,
        save_path,
        ocr_langs: prefs.ocr_langs,
        ocr_mode: OcrMode::parse(&prefs.ocr_mode),
        backend: prefs.backend,
    })
}

fn run_record(args: &[String]) -> anyhow::Result<()> {
    use nyx_capture_core::record::{self, Container, RecordRequest, Target};
    if has_flag(args, "--stop") {
        if let Some(path) = record::stop()? {
            remember_recording_cold(&path);
            eprintln!("{}", path.display());
        }
        return Ok(());
    }
    if has_flag(args, "--pause") {
        let paused = record::toggle_pause()?;
        eprintln!("{}", if paused { "paused" } else { "recording" });
        return Ok(());
    }
    let delay = flag_value(args, "--delay").and_then(|s| s.parse().ok()).unwrap_or(prefs::get().record_delay);
    if delay > 0 && !overlay::run_countdown_blocking(delay) {
        return Ok(());
    }
    let prefs = prefs::get();
    let pipewire = nyx_capture_core::record::pick(&prefs.recorder) == Some(nyx_capture_core::RecordBackend::GstPortal);
    let target = if pipewire {
        Target::Portal
    } else if has_flag(args, "--region") {
        match pick_record_region() {
            Ok(t) => t,
            Err(err) if err.to_string() == "cancelled" => return Ok(()),
            Err(err) => return Err(err),
        }
    } else if prefs.record_use_portal {
        Target::Portal
    } else {
        record::default_target()?
    };
    let (desktop_audio, microphone) = match prefs.record_audio.as_str() {
        "mic" => (false, true),
        "both" => (true, true),
        "none" => (false, false),
        _ => (true, false),
    };
    let container = Container::parse(&prefs.record_container);
    // Recordings are the person's files: the Videos folder, history or not.
    let output = record::output_path(&record::default_output_dir(), container);
    let with_cam = has_flag(args, "--webcam") || prefs.record_webcam;
    if with_cam && nyx_capture_core::webcam::available() {
        let device = if prefs.record_webcam_device.is_empty() { None } else { Some(prefs.record_webcam_device.as_str()) };
        if let Err(err) = nyx_capture_core::webcam::start(device, nyx_capture_core::webcam::Size::parse(&prefs.record_webcam_size), true)
        {
            eprintln!("{err}");
        }
    }
    let session = record::start(&RecordRequest {
        target,
        desktop_audio,
        microphone,
        cursor: prefs.record_cursor,
        container,
        fps: 60,
        output,
        backend: prefs.recorder,
    });
    match session {
        Ok(session) => {
            eprintln!("{}", session.path.display());
            Ok(())
        }
        Err(err) => {
            if with_cam {
                nyx_capture_core::webcam::stop();
            }
            Err(err)
        }
    }
}

/// Put a hotkey's shot in history (with history on): the PNG, its thumbnail and title
/// now; its OCR in a detached `--ocr-id` run.
fn remember_cold(shot: &nyx_capture_core::capture::Shot, title: &str) {
    let p = prefs::get();
    if !p.history {
        return;
    }
    let saved = (|| {
        let png = shot.png()?;
        let db = nyx_capture_core::store::open(&paths::library_db())?;
        let (id, path) = db.insert_png(&paths::captures_dir(), &png, shot.image.width() as i32, shot.image.height() as i32, "grim")?;
        let _ = db.set_title(&id, title);
        let _ = db.thumb(&id, &path, &paths::thumb_file(&id), false);
        anyhow::Ok(id)
    })();
    match saved {
        Ok(id) if p.ocr_on_capture => {
            if let Ok(me) = std::env::current_exe() {
                let _ = std::process::Command::new(me)
                    .args(["--ocr-id", &id])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
        }
        Ok(_) => {}
        Err(err) => eprintln!("nyx-capture: history: {err}"),
    }
}

/// Recognize the text in an image file with Capture's OCR settings, and print it.
fn ocr_file(path: &str, json: bool) -> anyhow::Result<()> {
    let p = prefs::get();
    let img = image::open(path)?;
    let r = nyx_capture_core::ocr::recognize(&img, OcrMode::parse(&p.ocr_mode), &p.ocr_langs)?;
    if json {
        println!("{}", serde_json::json!({ "text": r.text, "engine": r.engine }));
    } else {
        println!("{}", r.text);
    }
    Ok(())
}

fn ocr_into_history(id: &str) -> anyhow::Result<()> {
    let p = prefs::get();
    let db = nyx_capture_core::store::open(&paths::library_db())?;
    let row = db.get(id)?.ok_or_else(|| anyhow::anyhow!("no capture {id}"))?;
    let img = image::open(&row.path)?;
    let r = nyx_capture_core::ocr::recognize(&img, OcrMode::parse(&p.ocr_mode), &p.ocr_langs)?;
    db.set_ocr(id, &r.engine, &r.text)
}

/// A recording stopped from the command line goes into history like one stopped in the window.
fn remember_recording_cold(path: &std::path::Path) {
    if !prefs::get().history {
        return;
    }
    let saved = (|| {
        let db = nyx_capture_core::store::open(&paths::library_db())?;
        let id = db.insert(&nyx_capture_core::store::NewCapture {
            kind: nyx_capture_core::CaptureKind::Recording,
            width: 0,
            height: 0,
            path: path.to_path_buf(),
            source: "record".into(),
        })?;
        let _ = db.thumb(&id, path, &paths::thumb_file(&id), false);
        anyhow::Ok(())
    })();
    if let Err(err) = saved {
        eprintln!("nyx-capture: history: {err}");
    }
}

fn pick_record_region() -> anyhow::Result<nyx_capture_core::record::Target> {
    use nyx_capture_core::record::Target;
    if overlay::prefer() && overlay::supported() {
        overlay::ensure_gtk()?;
        let still = overlay::freeze()?;
        let picked = {
            let result = std::rc::Rc::new(std::cell::RefCell::new(None));
            let main = glib::MainLoop::new(None, false);
            let result2 = result.clone();
            let main2 = main.clone();
            overlay::pick(still, false, move |rect| {
                *result2.borrow_mut() = Some(rect);
                main2.quit();
            })?;
            main.run();
            result.borrow_mut().take().flatten()
        };
        let Some(rect) = picked else {
            anyhow::bail!("cancelled");
        };
        return Ok(Target::Region(rect));
    }
    match nyx_capture_core::capture::slurp_region()? {
        Some(rect) => Ok(Target::Region(rect)),
        None => anyhow::bail!("cancelled"),
    }
}

fn run_cli(action: &Action) -> anyhow::Result<(Outcome, Option<nyx_capture_core::capture::Shot>)> {
    let select = matches!(action.mode, CaptureMode::SelectRegion | CaptureMode::SelectWindow);
    let mut action = action.clone();
    if action.delay > 0 && !overlay::run_countdown_blocking(action.delay) {
        return Ok((Outcome::Cancelled, None));
    }
    action.delay = 0;
    if select && overlay::prefer() {
        match overlay::run_cli(&action) {
            Ok(outcome) => return Ok(outcome),
            // A test copy on a virtual monitor never opens slurp on the real ones.
            Err(err) if overlay::testing() => return Err(err),
            Err(err) => eprintln!("nyx-capture: overlay unavailable ({err}); using slurp"),
        }
    }
    action::require_tools_for(&action.mode, action.after, action.ocr_mode)?;
    action::run_keeping(&action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_capture_core::action::After;

    #[test]
    fn region_copy_is_a_cli_action() {
        let args = vec!["nyx-capture".into(), "--region".into(), "--copy".into()];
        let a = cli_action(&args).expect("action");
        assert!(matches!(a.mode, CaptureMode::SelectRegion));
        assert!(matches!(a.after, After::Copy));
    }

    #[test]
    fn bare_launch_opens_the_window() {
        let args = vec!["nyx-capture".into()];
        assert!(cli_action(&args).is_none());
    }

    #[test]
    fn pin_and_edit_are_cli_actions() {
        let pin = cli_action(&["nyx-capture".into(), "--pin".into()]).expect("pin");
        assert!(matches!(pin.mode, CaptureMode::SelectRegion));
        assert!(matches!(pin.after, After::Pin));
        let edit = cli_action(&["nyx-capture".into(), "--region".into(), "--edit".into()]).expect("edit");
        assert!(matches!(edit.after, After::Edit));
    }

    #[test]
    fn delay_parses_on_fullscreen() {
        let a = cli_action(&["nyx-capture".into(), "--fullscreen".into(), "--delay".into(), "3".into(), "--copy".into()]).expect("action");
        assert_eq!(a.delay, 3);
        assert!(matches!(a.after, After::Copy));
    }

    #[test]
    fn a_lone_path_or_file_flag_is_an_image_to_edit() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(image_arg(&a(&["nyx-capture", "shot.png"])).as_deref(), Some("shot.png"));
        assert_eq!(image_arg(&a(&["nyx-capture", "--file", "/tmp/a.png"])).as_deref(), Some("/tmp/a.png"));
        assert_eq!(image_arg(&a(&["nyx-capture", "--region"])), None);
        assert_eq!(image_arg(&a(&["nyx-capture"])), None);
        assert!(cli_action(&a(&["nyx-capture", "shot.png"])).is_none());
        let qr = cli_action(&a(&["nyx-capture", "--qr"])).expect("qr");
        assert!(matches!(qr.after, After::Qr));
        let text = cli_action(&a(&["nyx-capture", "--text"])).expect("text");
        assert!(matches!(text.after, After::Ocr));
    }

    #[test]
    fn a_missing_bus_name_is_not_the_primary() {
        assert!(!name_owned("io.github.design_nexus.nyx.Capture.NotRunning"));
    }
}
