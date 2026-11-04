//! Screen recording by wrapping gpu-screen-recorder, wf-recorder, wl-screenrec,
//! or gst-launch-1.0 after the XDG ScreenCast portal. This crate does not
//! implement a capture engine.

use crate::detect;
use crate::geom::Rect;
use crate::select;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static CHILD: Mutex<Option<Child>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    GpuScreenRecorder,
    WfRecorder,
    WlScreenrec,
    GstPortal,
}

impl Backend {
    pub fn id(self) -> &'static str {
        match self {
            Self::GpuScreenRecorder => "gpu-screen-recorder",
            Self::WfRecorder => "wf-recorder",
            Self::WlScreenrec => "wl-screenrec",
            Self::GstPortal => "pipewire",
        }
    }

    pub fn caps(self) -> RecordCaps {
        match self {
            Self::GpuScreenRecorder => RecordCaps { region: true, audio: true, pause: true, hardware: true, portal: true },
            Self::WfRecorder => RecordCaps { region: true, audio: true, pause: false, hardware: false, portal: false },
            Self::WlScreenrec => RecordCaps { region: true, audio: true, pause: false, hardware: true, portal: false },
            Self::GstPortal => RecordCaps { region: false, audio: true, pause: false, hardware: false, portal: true },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RecordCaps {
    pub region: bool,
    pub audio: bool,
    pub pause: bool,
    pub hardware: bool,
    pub portal: bool,
}

#[derive(Debug, Clone)]
pub enum Target {
    Monitor(String),
    Region(Rect),
    Portal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Webm,
    Gif,
}

impl Container {
    pub fn parse(s: &str) -> Self {
        match s {
            "webm" => Self::Webm,
            "gif" => Self::Gif,
            _ => Self::Mp4,
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4 | Self::Gif => "mp4",
            Self::Webm => "webm",
        }
    }

    pub fn gsr(self) -> &'static str {
        match self {
            Self::Webm => "webm",
            Self::Mp4 | Self::Gif => "mp4",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordRequest {
    pub target: Target,
    pub desktop_audio: bool,
    pub microphone: bool,
    pub cursor: bool,
    pub container: Container,
    pub fps: u32,
    pub output: PathBuf,
    pub backend: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub backend: String,
    pub pid: u32,
    pub path: PathBuf,
    pub paused: bool,
    pub gif: bool,
}

pub trait Recorder {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> RecordCaps;
}

/// First installed recorder, or the named preference when that binary exists.
pub fn pick(pref: &str) -> Option<Backend> {
    if let Ok(forced) = std::env::var("NYX_CAPTURE_RECORDER") {
        return named(&forced);
    }
    if pref != "auto"
        && let Some(b) = named(pref)
        && present(b)
    {
        return Some(b);
    }
    [Backend::GpuScreenRecorder, Backend::WfRecorder, Backend::WlScreenrec, Backend::GstPortal]
        .into_iter()
        .find(|&b| present(b))
}

fn named(id: &str) -> Option<Backend> {
    match id {
        "gpu-screen-recorder" => Some(Backend::GpuScreenRecorder),
        "wf-recorder" => Some(Backend::WfRecorder),
        "wl-screenrec" => Some(Backend::WlScreenrec),
        "pipewire" => Some(Backend::GstPortal),
        _ => None,
    }
}

fn present(b: Backend) -> bool {
    match b {
        Backend::GstPortal => detect::present("gst-launch-1.0") && detect::present("xdg-desktop-portal"),
        other => detect::present(other.id()),
    }
}

pub fn default_target() -> Result<Target> {
    if let Some(m) = select::focused_monitor()? {
        return Ok(Target::Monitor(m.name));
    }
    if let Some(m) = select::monitors()?.into_iter().next() {
        return Ok(Target::Monitor(m.name));
    }
    Ok(Target::Portal)
}

/// Command line for tests and for spawn. `path` is the file the backend writes.
pub fn argv(backend: Backend, req: &RecordRequest, path: &Path) -> Result<Vec<String>> {
    match backend {
        Backend::GpuScreenRecorder => Ok(gsr_argv(req, path)),
        Backend::WfRecorder => wf_argv(req, path),
        Backend::WlScreenrec => wl_argv(req, path),
        Backend::GstPortal => bail!("pipewire recording uses the ScreenCast portal"),
    }
}

fn gsr_argv(req: &RecordRequest, path: &Path) -> Vec<String> {
    let mut a = vec!["gpu-screen-recorder".into()];
    match &req.target {
        Target::Monitor(name) => {
            a.push("-w".into());
            a.push(name.clone());
            a.push("-s".into());
            a.push("0x0".into());
        }
        Target::Region(r) => {
            a.push("-w".into());
            a.push(format!("{}x{}+{}+{}", r.w, r.h, r.x, r.y));
        }
        Target::Portal => {
            a.push("-w".into());
            a.push("portal".into());
            a.push("-s".into());
            a.push("0x0".into());
        }
    }
    a.extend([
        "-k".into(),
        "auto".into(),
        "-f".into(),
        req.fps.max(1).to_string(),
        "-fm".into(),
        "cfr".into(),
        "-fallback-cpu-encoding".into(),
        "yes".into(),
        "-cursor".into(),
        if req.cursor { "yes" } else { "no" }.into(),
        "-c".into(),
        req.container.gsr().into(),
    ]);
    if let Some(audio) = audio_gsr(req.desktop_audio, req.microphone) {
        a.push("-a".into());
        a.push(audio);
        a.push("-ac".into());
        a.push("aac".into());
    }
    a.push("-o".into());
    a.push(path.to_string_lossy().into_owned());
    a
}

fn audio_gsr(desktop: bool, mic: bool) -> Option<String> {
    match (desktop, mic) {
        (true, true) => Some("default_output|default_input".into()),
        (true, false) => Some("default_output".into()),
        (false, true) => Some("default_input".into()),
        (false, false) => None,
    }
}

fn wf_argv(req: &RecordRequest, path: &Path) -> Result<Vec<String>> {
    if matches!(req.target, Target::Portal) {
        bail!("wf-recorder has no portal backend");
    }
    let mut a = vec!["wf-recorder".into(), "-r".into(), req.fps.max(1).to_string(), "-R".into(), "48000".into()];
    match &req.target {
        Target::Monitor(name) => {
            a.push("-o".into());
            a.push(name.clone());
        }
        Target::Region(r) => {
            a.push("-g".into());
            a.push(format!("{},{} {}x{}", r.x, r.y, r.w, r.h));
        }
        Target::Portal => {}
    }
    // One device: the desktop's sound (the default output's monitor) wins over the mic.
    if req.desktop_audio || req.microphone {
        // Without pactl to name a device, wf-recorder records the default source.
        a.push(match audio_device(req.desktop_audio, req.microphone) {
            Some(device) => format!("--audio={device}"),
            None => "--audio".into(),
        });
    }
    a.push("-f".into());
    a.push(path.to_string_lossy().into_owned());
    Ok(a)
}

/// The PulseAudio/PipeWire source to record: the default sink's monitor for desktop
/// sound, the default source for the mic. None when neither is wanted (or found).
fn audio_device(desktop: bool, mic: bool) -> Option<String> {
    let pactl = |what: &str| {
        Command::new("pactl").arg(what).output().ok().filter(|o| o.status.success()).and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            (!s.is_empty()).then_some(s)
        })
    };
    if desktop {
        pactl("get-default-sink").map(|sink| format!("{sink}.monitor"))
    } else if mic {
        pactl("get-default-source")
    } else {
        None
    }
}

fn wl_argv(req: &RecordRequest, path: &Path) -> Result<Vec<String>> {
    if matches!(req.target, Target::Portal) {
        bail!("wl-screenrec has no portal backend");
    }
    let mut a = vec!["wl-screenrec".into()];
    match &req.target {
        Target::Monitor(name) => {
            a.push("--output".into());
            a.push(name.clone());
        }
        Target::Region(r) => {
            // slurp's "X,Y WxH", as wf-recorder takes.
            a.push("-g".into());
            a.push(format!("{},{} {}x{}", r.x, r.y, r.w, r.h));
        }
        Target::Portal => {}
    }
    if req.desktop_audio || req.microphone {
        a.push("--audio".into());
        if let Some(device) = audio_device(req.desktop_audio, req.microphone) {
            a.push("--audio-device".into());
            a.push(device);
        }
    }
    a.push("-f".into());
    a.push(path.to_string_lossy().into_owned());
    Ok(a)
}

pub fn start(req: &RecordRequest) -> Result<Session> {
    if let Some(s) = active() {
        bail!("already recording ({})", s.path.display());
    }
    let Some(first) = pick(&req.backend) else {
        bail!("No screen recorder is installed (gpu-screen-recorder, wf-recorder, wl-screenrec, or gst-launch-1.0)");
    };
    if let Some(dir) = req.output.parent() {
        fs::create_dir_all(dir)?;
    }
    let gif = req.container == Container::Gif;
    let mut backends = vec![first];
    for b in [Backend::GpuScreenRecorder, Backend::WfRecorder, Backend::WlScreenrec, Backend::GstPortal] {
        if b != first && present(b) {
            backends.push(b);
        }
    }
    let mut last = anyhow::anyhow!("no recorder started");
    for backend in backends {
        if matches!(req.target, Target::Portal) && !backend.caps().portal {
            continue;
        }
        match try_start(backend, req, gif) {
            Ok(session) => return Ok(session),
            Err(err) => last = err,
        }
    }
    Err(last)
}

fn try_start(backend: Backend, req: &RecordRequest, gif: bool) -> Result<Session> {
    if backend == Backend::GstPortal {
        return try_start_gst(req, gif);
    }
    let args = argv(backend, req, &req.output)?;
    let child = spawn(&args)?;
    let pid = child.id();
    if let Ok(mut slot) = CHILD.lock() {
        *slot = Some(child);
    }
    let portal = matches!(req.target, Target::Portal);
    let deadline = Instant::now() + Duration::from_secs(if portal { 120 } else { 15 });
    while Instant::now() < deadline {
        if req.output.is_file() && req.output.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            let session = Session { backend: backend.id().into(), pid, path: req.output.clone(), paused: false, gif };
            write_session(&session)?;
            return Ok(session);
        }
        if !alive(pid) {
            reap();
            let _ = fs::remove_file(&req.output);
            bail!("{} exited before the recording file appeared", backend.id());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = signal(pid, "-INT");
    reap();
    let _ = fs::remove_file(&req.output);
    bail!("{} did not create {}", backend.id(), req.output.display())
}

pub fn stop() -> Result<Option<PathBuf>> {
    let Some(session) = read_session() else { return Ok(None) };
    if !ours(&session) {
        // The recorder is gone and its pid may be someone else's now: signal nothing.
        clear_session();
        return Ok(session.path.is_file().then_some(session.path));
    }
    let _ = signal(session.pid, "-INT");
    // A software encoder can take a while to write out the end of a long recording.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && alive(session.pid) {
        reap();
        std::thread::sleep(Duration::from_millis(100));
    }
    if alive(session.pid) {
        let _ = signal(session.pid, "-KILL");
    }
    reap();
    clear_session();
    let _ = fs::remove_file(gst_job_path());
    let _ = fs::remove_file(gst_pid_path());
    crate::webcam::stop();
    if !session.path.is_file() {
        bail!("the recording file is missing");
    }
    let path = if session.gif { to_gif(&session.path)? } else { session.path };
    if detect::present("ffmpeg") {
        let _ = trim_lead(&path, 0.1);
    }
    Ok(Some(path))
}

pub fn toggle_pause() -> Result<bool> {
    let Some(mut session) = read_session() else {
        bail!("not recording");
    };
    if session.backend != Backend::GpuScreenRecorder.id() {
        bail!("pause needs gpu-screen-recorder");
    }
    if !ours(&session) {
        clear_session();
        bail!("not recording");
    }
    signal(session.pid, "-USR2")?;
    session.paused = !session.paused;
    write_session(&session)?;
    Ok(session.paused)
}

pub fn active() -> Option<Session> {
    reap();
    let session = read_session()?;
    if ours(&session) {
        Some(session)
    } else {
        clear_session();
        None
    }
}

/// The last session's file when its recorder ended by itself (it crashed, or was killed),
/// so it can still go into history. Clears the session.
pub fn ended() -> Option<PathBuf> {
    let session = read_session()?;
    if ours(&session) {
        return None;
    }
    clear_session();
    session.path.is_file().then_some(session.path)
}

/// True when the session's pid is alive and is its recorder (not a reused pid).
fn ours(session: &Session) -> bool {
    alive(session.pid) && runs(session.pid, &session.backend)
}

/// Whether `pid` is the program behind `backend` (by `/proc/<pid>/comm`, which keeps 15
/// characters). True when /proc can't tell.
fn runs(pid: u32, backend: &str) -> bool {
    let Ok(comm) = fs::read_to_string(format!("/proc/{pid}/comm")) else { return true };
    let program = if backend == Backend::GstPortal.id() { "gst-launch-1.0" } else { backend };
    let short: String = program.chars().take(15).collect();
    comm.trim() == short
}

fn reap() {
    let Ok(mut slot) = CHILD.lock() else { return };
    if let Some(child) = slot.as_mut()
        && child.try_wait().ok().flatten().is_some()
    {
        slot.take();
    }
}

/// Copy a time range with ffmpeg. `end` is exclusive; `None` means through the end.
pub fn trim(src: &Path, dest: &Path, start: f64, end: Option<f64>) -> Result<()> {
    if !detect::present("ffmpeg") {
        bail!("trim needs ffmpeg");
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-y", "-ss", &format!("{start:.3}"), "-i"]).arg(src);
    if let Some(end) = end {
        cmd.args(["-to", &format!("{end:.3}")]);
    }
    cmd.args(["-c", "copy"]).arg(dest);
    let status = cmd.status()?;
    if !status.success() {
        bail!("ffmpeg trim failed");
    }
    Ok(())
}

fn trim_lead(path: &Path, secs: f64) -> Result<()> {
    // The copy keeps the file's own container.
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("mp4");
    let tmp = path.with_extension(format!("trim.{ext}"));
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-y", "-ss", &format!("{secs:.3}"), "-i"]).arg(path).args(["-c", "copy"]).arg(&tmp);
    if cmd.status()?.success() {
        fs::rename(&tmp, path)?;
    } else {
        let _ = fs::remove_file(&tmp);
    }
    Ok(())
}

fn to_gif(mp4: &Path) -> Result<PathBuf> {
    if !detect::present("ffmpeg") {
        return Ok(mp4.to_path_buf());
    }
    let gif = mp4.with_extension("gif");
    let vf = "fps=15,scale=800:-1:flags=lanczos,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse";
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(mp4)
        .args(["-vf", vf])
        .arg(&gif)
        .status()?;
    if status.success() && gif.is_file() {
        let _ = fs::remove_file(mp4);
        Ok(gif)
    } else {
        Ok(mp4.to_path_buf())
    }
}

fn spawn(args: &[String]) -> Result<Child> {
    let (bin, rest) = args.split_first().expect("argv");
    Command::new(bin).args(rest).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(Into::into)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GstJob {
    output: PathBuf,
    container: String,
    desktop_audio: bool,
    microphone: bool,
    cursor: bool,
}

fn gst_job_path() -> PathBuf {
    session_path().parent().unwrap_or_else(|| Path::new(".")).join("gst-job.json")
}

fn gst_pid_path() -> PathBuf {
    session_path().parent().unwrap_or_else(|| Path::new(".")).join("gst.pid")
}

fn try_start_gst(req: &RecordRequest, gif: bool) -> Result<Session> {
    if !present(Backend::GstPortal) {
        bail!("pipewire recording needs gst-launch-1.0 and xdg-desktop-portal");
    }
    let job = GstJob {
        output: req.output.clone(),
        container: match req.container {
            Container::Webm => "webm".into(),
            _ => "mp4".into(),
        },
        desktop_audio: req.desktop_audio,
        microphone: req.microphone,
        cursor: req.cursor,
    };
    let job_path = gst_job_path();
    if let Some(dir) = job_path.parent() {
        fs::create_dir_all(dir)?;
    }
    crate::atomic_write(&job_path, serde_json::to_vec_pretty(&job)?.as_slice())?;
    let _ = fs::remove_file(gst_pid_path());
    let exe = std::env::current_exe().context("current exe")?;
    let child = Command::new(exe)
        .args(["--record", "--gst-worker"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn gst worker")?;
    let worker = child.id();
    if let Ok(mut slot) = CHILD.lock() {
        *slot = Some(child);
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if req.output.is_file() && req.output.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            let pid = fs::read_to_string(gst_pid_path()).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(worker);
            let session = Session { backend: Backend::GstPortal.id().into(), pid, path: req.output.clone(), paused: false, gif };
            write_session(&session)?;
            return Ok(session);
        }
        if !alive(worker) {
            reap();
            let _ = fs::remove_file(&req.output);
            bail!("pipewire recorder exited before the recording file appeared");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = signal(worker, "-INT");
    if let Some(pid) = fs::read_to_string(gst_pid_path()).ok().and_then(|s| s.trim().parse().ok()) {
        let _ = signal(pid, "-INT");
    }
    reap();
    let _ = fs::remove_file(&req.output);
    bail!("pipewire recorder did not create {}", req.output.display())
}

/// Hidden worker: hold the ScreenCast session and run gst-launch until it exits.
pub fn run_gst_worker() -> Result<()> {
    let job: GstJob = serde_json::from_slice(&fs::read(gst_job_path()).context("gst job")?)?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().context("portal runtime")?;
    let cast = rt.block_on(open_screencast(job.cursor))?;
    let fd = cast.fd.as_raw_fd();
    clear_cloexec(fd)?;
    let container = Container::parse(&job.container);
    let args = gst_args(fd, cast.node, &job.output, container, job.desktop_audio, job.microphone);
    let mut child = spawn(&args)?;
    if let Some(dir) = gst_pid_path().parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(gst_pid_path(), child.id().to_string());
    let status = child.wait()?;
    drop(cast);
    let _ = fs::remove_file(gst_job_path());
    let _ = fs::remove_file(gst_pid_path());
    if !status.success() {
        bail!("gst-launch-1.0 exited {status}");
    }
    Ok(())
}

struct GstCast {
    _proxy: ashpd::desktop::screencast::Screencast,
    _session: ashpd::desktop::Session<ashpd::desktop::screencast::Screencast>,
    fd: std::os::fd::OwnedFd,
    node: u32,
}

async fn open_screencast(cursor: bool) -> Result<GstCast> {
    use ashpd::desktop::PersistMode;
    use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
    let proxy = Screencast::new().await.context("screencast portal")?;
    let session = proxy.create_session(Default::default()).await.context("screencast session")?;
    let cursor_mode = if cursor { CursorMode::Embedded } else { CursorMode::Hidden };
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(cursor_mode)
                .set_sources(SourceType::Monitor | SourceType::Window)
                .set_multiple(false)
                .set_persist_mode(PersistMode::DoNot),
        )
        .await
        .context("screencast sources")?;
    let response = proxy.start(&session, None, Default::default()).await.context("screencast start")?.response().context("screencast response")?;
    let stream = response.streams().first().ok_or_else(|| anyhow::anyhow!("screencast returned no stream"))?;
    let node = stream.pipe_wire_node_id();
    let fd = proxy.open_pipe_wire_remote(&session, Default::default()).await.context("pipewire remote")?;
    Ok(GstCast { _proxy: proxy, _session: session, fd, node })
}

/// gst-launch-1.0 argv. `fd` is the portal PipeWire remote.
pub fn gst_args(fd: i32, node: u32, output: &Path, container: Container, desktop: bool, mic: bool) -> Vec<String> {
    let mut a = vec!["gst-launch-1.0".into(), "-e".into()];
    a.extend([
        "pipewiresrc".into(),
        format!("fd={fd}"),
        format!("path={node}"),
        "do-timestamp=true".into(),
        "!".into(),
        "queue".into(),
        "!".into(),
        "videoconvert".into(),
        "!".into(),
        "video/x-raw,format=I420".into(),
        "!".into(),
    ]);
    match container {
        Container::Webm => a.extend(["vp8enc".into(), "deadline=1".into(), "!".into(), "mux.".into()]),
        Container::Mp4 | Container::Gif => a.extend(["avenc_mpeg4".into(), "!".into(), "mux.".into()]),
    }
    if desktop || mic {
        let audio_enc = if container == Container::Webm { "opusenc" } else { "avenc_aac" };
        if desktop && mic {
            if let Some(mon) = pulse_monitor() {
                a.extend([
                    "audiomixer".into(),
                    "name=mix".into(),
                    "!".into(),
                    "audioconvert".into(),
                    "!".into(),
                    "audioresample".into(),
                    "!".into(),
                    audio_enc.into(),
                    "!".into(),
                    "mux.".into(),
                    "pulsesrc".into(),
                    format!("device={mon}"),
                    "!".into(),
                    "queue".into(),
                    "!".into(),
                    "mix.".into(),
                    "pulsesrc".into(),
                    "!".into(),
                    "queue".into(),
                    "!".into(),
                    "mix.".into(),
                ]);
            } else {
                push_pulsesrc(&mut a, None, audio_enc);
            }
        } else if desktop {
            push_pulsesrc(&mut a, pulse_monitor().as_deref(), audio_enc);
        } else {
            push_pulsesrc(&mut a, None, audio_enc);
        }
    }
    match container {
        Container::Webm => a.extend(["webmmux".into(), "name=mux".into(), "!".into()]),
        Container::Mp4 | Container::Gif => a.extend(["mp4mux".into(), "name=mux".into(), "fragment-duration=1000".into(), "!".into()]),
    }
    a.extend(["filesink".into(), format!("location={}", output.display())]);
    a
}

fn push_pulsesrc(a: &mut Vec<String>, device: Option<&str>, enc: &str) {
    a.push("pulsesrc".into());
    if let Some(device) = device {
        a.push(format!("device={device}"));
    }
    a.extend(["!".into(), "audioconvert".into(), "!".into(), "audioresample".into(), "!".into(), enc.into(), "!".into(), "mux.".into()]);
}

fn pulse_monitor() -> Option<String> {
    let out = Command::new("pactl").args(["get-default-sink"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let sink = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sink.is_empty() { None } else { Some(format!("{sink}.monitor")) }
}

fn clear_cloexec(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        bail!("fcntl F_GETFD");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        bail!("fcntl F_SETFD");
    }
    Ok(())
}

fn signal(pid: u32, sig: &str) -> Result<()> {
    let status = Command::new("kill").args([sig, &pid.to_string()]).status()?;
    if status.success() { Ok(()) } else { bail!("kill {sig} {pid} failed") }
}

/// Running: not gone, and not a zombie (a recorder this process started, which has
/// exited and only waits to be collected).
fn alive(pid: u32) -> bool {
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        // "pid (comm) S ...": the state follows the last ')'.
        return stat.rsplit_once(')').and_then(|(_, rest)| rest.trim_start().chars().next()).is_some_and(|state| state != 'Z' && state != 'X');
    }
    Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false)
}

pub fn session_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join("nyx-capture").join("recording.json")
}

fn write_session(session: &Session) -> Result<()> {
    let path = session_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    crate::atomic_write(&path, serde_json::to_vec_pretty(session)?.as_slice())
}

fn read_session() -> Option<Session> {
    let bytes = fs::read(session_path()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn clear_session() {
    let _ = fs::remove_file(session_path());
}

pub struct NoneRecorder;

impl Recorder for NoneRecorder {
    fn id(&self) -> &'static str {
        "none"
    }
    fn capabilities(&self) -> RecordCaps {
        RecordCaps::default()
    }
}

impl Recorder for Backend {
    fn id(&self) -> &'static str {
        Backend::id(*self)
    }
    fn capabilities(&self) -> RecordCaps {
        self.caps()
    }
}

pub fn default_output_dir() -> PathBuf {
    let videos = std::env::var_os("XDG_VIDEOS_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Videos")))
        .unwrap_or_else(|| PathBuf::from("."));
    videos.join("Nyx Capture")
}

pub fn output_path(dir: &Path, container: Container) -> PathBuf {
    let _ = fs::create_dir_all(dir);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    dir.join(format!("recording-{stamp}.{}", container.ext()))
}

pub fn tools_caps() -> RecordCaps {
    pick("auto").map(|b| b.caps()).unwrap_or_default()
}

pub fn can_pause() -> bool {
    active().is_some_and(|s| s.backend == Backend::GpuScreenRecorder.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(target: Target) -> RecordRequest {
        RecordRequest {
            target,
            desktop_audio: true,
            microphone: false,
            cursor: true,
            container: Container::Mp4,
            fps: 60,
            output: PathBuf::from("/tmp/rec.mp4"),
            backend: "auto".into(),
        }
    }

    #[test]
    fn gsr_monitor_audio_and_cursor() {
        let args = gsr_argv(&req(Target::Monitor("eDP-2".into())), Path::new("/tmp/rec.mp4"));
        assert_eq!(args[0], "gpu-screen-recorder");
        assert!(args.contains(&"-w".into()));
        assert!(args.contains(&"eDP-2".into()));
        assert!(args.contains(&"default_output".into()));
        assert!(args.contains(&"yes".into()));
        assert!(args.contains(&"mp4".into()));
    }

    #[test]
    fn gsr_region_geometry() {
        let r = req(Target::Region(Rect::new(10, 20, 300, 200)));
        let args = gsr_argv(&r, Path::new("/tmp/rec.mp4"));
        assert!(args.iter().any(|a| a == "300x200+10+20"));
    }

    #[test]
    fn wf_region_uses_slurp_space() {
        let r = req(Target::Region(Rect::new(10, 20, 300, 200)));
        let args = wf_argv(&r, Path::new("/tmp/rec.mp4")).unwrap();
        assert!(args.contains(&"-g".into()));
        assert!(args.contains(&"10,20 300x200".into()));
        assert!(args.iter().any(|a| a.starts_with("--audio")));
    }

    #[test]
    fn wf_rejects_portal() {
        assert!(wf_argv(&req(Target::Portal), Path::new("/tmp/rec.mp4")).is_err());
    }

    #[test]
    fn gif_records_as_mp4() {
        assert_eq!(Container::Gif.ext(), "mp4");
        assert_eq!(Container::Gif.gsr(), "mp4");
        assert_eq!(Container::parse("gif"), Container::Gif);
        assert_eq!(Container::parse("webm"), Container::Webm);
    }

    #[test]
    fn pick_honours_forced_env() {
        unsafe { std::env::set_var("NYX_CAPTURE_RECORDER", "wf-recorder") };
        assert_eq!(pick("auto"), Some(Backend::WfRecorder));
        unsafe { std::env::remove_var("NYX_CAPTURE_RECORDER") };
    }

    #[test]
    fn session_path_is_under_runtime() {
        let p = session_path();
        assert_eq!(p.file_name().unwrap(), "recording.json");
    }

    #[test]
    fn an_exited_child_is_not_alive() {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(200));
        // Exited but not yet collected: a zombie.
        assert!(!alive(pid));
        let _ = child.wait();
        assert!(alive(std::process::id()));
    }

    #[test]
    fn recorder_pids_are_checked_by_program() {
        let me = std::process::id();
        // This test binary isn't a screen recorder.
        assert!(!runs(me, "wf-recorder"));
        let comm = std::fs::read_to_string(format!("/proc/{me}/comm")).unwrap();
        assert!(runs(me, comm.trim()));
    }

    #[test]
    fn gst_mp4_pipeline_uses_portal_fd() {
        let args = gst_args(3, 42, Path::new("/tmp/rec.mp4"), Container::Mp4, true, false);
        assert_eq!(args[0], "gst-launch-1.0");
        assert!(args.contains(&"-e".into()));
        assert!(args.contains(&"pipewiresrc".into()));
        assert!(args.contains(&"fd=3".into()));
        assert!(args.contains(&"path=42".into()));
        assert!(args.contains(&"mp4mux".into()));
        assert!(args.contains(&"avenc_mpeg4".into()));
        assert!(args.contains(&"pulsesrc".into()));
    }

    #[test]
    fn gst_webm_pipeline_is_silent() {
        let args = gst_args(3, 42, Path::new("/tmp/rec.webm"), Container::Webm, false, false);
        assert!(args.contains(&"vp8enc".into()));
        assert!(args.contains(&"webmmux".into()));
        assert!(!args.contains(&"pulsesrc".into()));
    }
}
