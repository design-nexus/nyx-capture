# Nyx Capture

Native screenshot, annotation, OCR, and (later) screen recording for the Nyx suite. Repository is the existing workspace `design-nexus/nyx` at `/home/ken/Projects/nyx`. Capture is a new app in that workspace, not a new repository and not a fork of Flameshot, ShareX, or Satty.

Phase 1 is the only implementation this plan authorizes. Later phases are specified so crate boundaries and the SQLite schema do not have to be rewritten. They stay unbuilt until the previous phase is accepted.

---

## Decisions from the existing tree

`STYLE.md` in [nyx-design](https://github.com/design-nexus/nyx-design) is the visual contract. Music (`apps/music/src/window.rs`) is the sidebar reference. Settings is the settings-card reference. Photos (`apps/gallery/src/edit/`) is the annotation-object reference. Shared behavior comes from `nyx-core`, `nyx-theme`, `nyx-ui`, and `nyx-icons`. Every current app is GTK 4 (`gtk4` 0.11, feature `v4_18`) with no libadwaita and no WebKit. Capture follows that.

There is no Nyx Drive app and no Nyx Clipboard app. Clipboard is GTK plus `wl-copy`/`wl-paste` (Chat) or `wl-clipboard-rs` (Passwords). Drive sharing is a Phase 3 placeholder that does not invent a storage API.

Assistant integration today is per-app D-Bus via `nyx_core::control` (`Describe` / `Call` on `io.github.design_nexus.nyx.Control`). Handlers run on the GTK thread; long work uses `serve_async` the way News and Music do. Assistant already has a separate sandboxed ability (`apps/assistant/abilities/capture`) that shells out to `grim` and `tesseract`. That ability is not replaced in Phase 1. Changing Assistant needs an explicit yes.

Glimpse plugins are sandboxed JSON-lines processes (`apps/glimpse/PLUGIN_API.md`, `crates/glimpse-plugin-sdk`). They search and return plain text. They do not embed GTK. News already has the sandbox exception for a library under `~/.local/share` (`bind_news_library` in `apps/glimpse/src/plugins.rs`). Capture needs the same pattern for `nyx-capture`.

SQLite in this repo is `rusqlite` 0.40 with `bundled`. News FTS5 uses `tokenize='unicode61'` with content tables and insert/delete/update triggers. Capture copies that shape.

Theme, density, text size, and “open on” already live in `~/.config/nyx/theme.toml`. Capture does not invent a second theme store.

Omarchy already ships capture scripts (`omarchy-capture-screenshot` → `omasnap`, `omarchy-capture-region` + `slurp`/`hyprpicker`, `omarchy-capture-text` → grim+tesseract, `omarchy-capture-screenrecording` → gpu-screen-recorder with wf-recorder fallback). Settings → Capture currently launches those scripts. Capture does not wrap them as the product. They remain the Omarchy fallback until Ken asks Settings to switch. This plan does not edit Settings, Assistant, Chat, Photos, or Glimpse except the one-line Capture library bind that News already established as the plugin pattern.

Photos already implements object markup (pen, highlighter, arrow, rectangle, ellipse, text, pixelate, crop, undo/redo) with normalised coordinates and Cairo. Phase 1 copies that model into `nyx-capture-core` rather than extracting Photos into a shared crate.

---

## What the app is

| | |
| --- | --- |
| Package | `nyx-capture` |
| Binary | `nyx-capture` |
| Helper | same binary, fast CLI modes that skip the history window |
| App id | `io.github.design_nexus.nyx.Capture` |
| Pin id | `io.github.design_nexus.nyx.Capture.Pin` |
| Slug | `nyx-capture` via `nyx_core::app::init` |
| Config | `~/.config/nyx-capture/settings.toml` |
| Library | `~/.local/share/nyx-capture/capture.sqlite` |
| Files | `~/.local/share/nyx-capture/captures/{id}.png` plus `{id}.ann.json` |
| Cache | `~/.cache/nyx-capture/` (thumbnails, RapidOCR models) |
| Window | Music’s frame for History: sidebar, toolbar, virtualized grid, status bar, settings card. Default 1120×800, tiled. Overlay and pins are separate surfaces. |

Sidebar groups, in order:

- **Recent**
- **Screenshots**
- **Recordings** (empty state in Phase 1; records land here in Phase 2)
- **Starred**

Settings is the gear card, never a sidebar row. Pages have no titles. The window title is `Capture — Recent`. The status bar holds `F1 Shortcuts`, the capture count, and the gear.

Core capture works with history off, OCR off, recording unbuilt, and no network.

---

## Crate layout

```
crates/nyx-capture-core/     # no GTK
apps/capture/                 # GTK overlay, editor, history, pins
apps/capture-glimpse/         # GTK-free Glimpse plugin
```

A second repository would duplicate edition, license, and CI for a library only Nyx apps link. The workspace already discovers `apps/*` and `crates/*`.

### `nyx-capture-core`

No GTK. Unit-tested with the `nyx-test` identity.

| Module | Role |
| --- | --- |
| `geom` | Layout vs image-pixel rectangles. Copy the scale mapping from `apps/assistant/crates/assistant-desktop/src/screen.rs`. |
| `detect` | `which` for grim, slurp, hyprctl, gtk4-layer-shell compositor support, portal, tesseract, RapidOCR models, recorders. |
| `capture` | `CaptureBackend` trait and backends: Grim, Portal, SlurpRegion. |
| `select` | Window and monitor lists (Hyprland `hyprctl` JSON; empty capabilities elsewhere). |
| `imageio` | PNG/WebP/JPEG read/write via `image` 0.25. |
| `annot` | `Mark` objects, `EditState`, undo stack, rasterise. Pattern from Photos `edit/`. |
| `ocr` | `OcrEngine` trait, Tesseract, RapidOCR, `Automatic` pipeline. |
| `ocr/quality` | Heuristics that decide fallback. Scores from different engines are never compared raw. |
| `store` | SQLite + FTS5, retention, optional dHash. |
| `export` | Flatten annotated image; copy-ready PNG bytes. |
| `record` | Trait and no-op in Phase 1. Phase 2 fills it. |

```rust
pub trait CaptureBackend {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> CaptureCaps;
    fn capture(&self, req: CaptureRequest) -> Result<Shot>;
}

pub struct CaptureRequest {
    pub mode: CaptureMode, // FullDesktop, Monitor(name), ActiveWindow, Window(id), Region(Rect), Delay { mode, secs }
}

pub struct Shot {
    pub image: DynamicImage, // image crate
    pub area: Rect,          // layout coordinates
    pub scale: f64,          // image px / layout unit
    pub output: Option<String>,
}

pub trait RegionSelector {
    fn pick(&self, hint: SelectHint) -> Result<Rect>;
}

pub trait OcrEngine {
    fn id(&self) -> &'static str;
    fn recognize(&self, img: &DynamicImage) -> Result<OcrResult>;
}

pub struct OcrResult {
    pub text: String,
    pub engine: String,
    pub lines: Vec<OcrLine>, // text + bbox in image pixels + engine-local confidence
}

pub enum OcrMode { Automatic, Tesseract, RapidOcr }

pub trait Recorder {
    fn capabilities(&self) -> RecordCaps;
    fn start(&self, req: RecordRequest) -> Result<Recording>;
}
```

Capability flags hide unsupported settings. The GTK app never offers a control whose backend is missing.

### `nyx-capture` (GTK)

| File | Role |
| --- | --- |
| `main.rs` | `app::init`, GSK `ngl`, GApplication `HANDLES_COMMAND_LINE`, control interface, `--toggle`. |
| `window.rs` | History shell. Music sidebar construction (`hexpand(false)` on sidebar/nav/scroller). |
| `overlay.rs` | Fullscreen still overlay, region/window pick, magnifier, dimensions, keys. |
| `editor.rs` | Annotation canvas + tool strip. Photos editor flow, Capture tools added. |
| `pin.rs` | Floating always-on-top reference windows. |
| `grid.rs` | History grid (virtualized `GridView`) and thumbnails; the window holds filter, reopen, copy, export, delete. |
| `ocr_view.rs` | Overlay of recognized lines; copy text. |
| `settings_dialog.rs` | Shared settings card. |
| `control.rs` | Assistant tools while the app is open. |
| `prefs.rs` | `prefs_store!`. |
| `paths.rs`, `cmd.rs`, `theme.rs`, `widgets.rs` | Re-exports plus app-only bits. |
| `cmd.rs` (`background`) | Worker threads for OCR, thumbnails, encode, slurp. GTK never waits. |

Cold-start path: `nyx-capture --region` builds the overlay only. History window, RapidOCR, and SQLite open after the shot is accepted (or never, if history is off and the user copies then quits).

### `nyx-capture-glimpse`

GTK-free. Opens `capture.sqlite` read-only (`query_only`). Capabilities `search` and `preview`. Actions: copy OCR text, or `activate` tokens that the host turns into open/copy. Commands in search: `capture region`, `capture window`, `capture screen` return hits whose `activate` token is `region` / `window` / `fullscreen`. The plugin cannot spawn GTK; activate is handled by launching `nyx-capture --region` on the host side through the existing `Action::Open`/`Activate` path, documented in PLUGIN_API.md.

Glimpse sandbox: add `bind_capture_library` next to `bind_news_library`, plugin id exactly `nyx-capture`, mount `$XDG_DATA_HOME/nyx-capture` at `/capture-library`, set `NYX_CAPTURE_DB=/capture-library/capture.sqlite`. That is a one-function change in Glimpse, the same exception News already has. Ask before landing it.

---

## Backends and risks

### Screenshot

| Backend | Role | Risk | Mitigation |
| --- | --- | --- | --- |
| **grim** | Primary capture | Hyprland/wlroots only; needs geometry string; scale mismatch | Already used by Chat, Assistant desktop, Omarchy. Reuse `Shot` scale mapping. |
| **GTK4 overlay** | Region/window UI | Layer Shell not on every compositor; exclusive keyboard grab | `gtk4-layer-shell` 0.8.x (matches gtk4 0.11). Probe `is_supported()`. Fallback: slurp. |
| **slurp** | Region fallback | No magnifier, no Nyx chrome | Acceptable fallback. Same geometry format grim already takes. |
| **hyprctl clients/monitors** | Window and monitor lists | Hyprland-only | Capability-gated. Active window = focused client. |
| **ashpd Screenshot portal** | Cross-desktop | Interactive dialog, slow, may refuse region | Last resort when grim is absent. Interactive=false when the portal allows it. |
| **Direct screencopy** | Future | Protocol churn, buffer formats, DMA-BUF | Not in Phase 1–2. |

**Capture sequence on Hyprland (Phase 1):**

1. Hide Capture windows (`minimize` or unmap overlay-to-be).
2. `grim -t png` of all outputs (or `-o` for one monitor). This is the freeze: a still, not hyprpicker.
3. Show a layer-shell overlay per output, painting the still, dimmed with `@veil`.
4. Pointer draws a region; hinted window/monitor rects from `hyprctl`; a click under 20px² snaps to the smallest containing rect (Omarchy smart capture).
5. Magnifier near the cursor (zoomed crop of the still). Readout: `WxH` and `X,Y`.
6. Keys: Esc cancel, Enter accept, Space toggle window-snap, arrows nudge, Shift+arrows resize, `[` `]` delay is already elapsed.
7. Crop the still. Hand to editor or clipboard per prefs.

Do not assume global input, arbitrary positioning, or screen access on GNOME. Portal path skips the custom overlay.

Delayed capture: countdown label on a small layer surface, then the sequence above.

### Recording (Phase 2 only)

Do not write a capture engine. Detect, in order:

1. **gpu-screen-recorder** — Omarchy’s primary; NVENC / AMF / VAAPI / Vulkan. Portal flag exists (`OMARCHY_SCREENRECORD_USE_PORTAL`).
2. **wf-recorder** / **wl-screenrec** — software or VAAPI; already the Omarchy fallback.
3. **PipeWire + GStreamer** via XDG ScreenCast portal (`ashpd` screencast) — GNOME/KDE and sandboxed sessions.

Export/trim with FFmpeg (Media Player already depends on it). GIF via ffmpeg palettegen. Pause/resume only if the selected backend reports it. Webcam overlay: Omarchy’s mpv `WebcamOverlay` + Hyprland window rules, optional.

### OCR

| Engine | Binding | Risk | Mitigation |
| --- | --- | --- | --- |
| **Tesseract (default)** | System `tesseract` CLI, TSV output | Process spawn cost ~20–50ms | Matches Assistant ability and `omarchy-capture-text`. No bindgen, no clang at build. Bounding boxes from TSV. |
| **leptess 0.14.0** | In-process | Last crates.io release 2023; bindgen | Spike only. Adopt if TSV is too slow or boxes are wrong. Do not take `tesseract-rs` `build-tesseract` (downloads and compiles C++). |
| **RapidOCR** | `rapidocr-core` 0.2.2 (ONNX Runtime) | Young crate, low download count | Spike in milestone 0. If unsuitable, thin `ort` wrapper around official RapidOCR ONNX det/rec models. Models live in cache, loaded on first fallback. |

Both engines stay local. RapidOCR is never loaded at startup.

### Other crates (Phase 1)

Align versions with News/Photos:

- `gtk4` 0.11, `v4_18`
- `gtk4-layer-shell` 0.8
- `rusqlite` 0.40, `bundled` (FTS5 is in the amalgam)
- `image` 0.25 (`png`, `jpeg`, `webp`)
- `anyhow`, `serde`, `serde_json`, `toml` as News
- `ashpd` 0.13, features `screenshot` (Phase 1) and later `screencast`
- `pangocairo` 0.22 (text marks, same as Photos)

No Qt, no Electron, no cloud OCR.

---

## Adaptive OCR algorithm

Mode default: `Automatic`.

1. Optional preprocess: grayscale, 300 DPI equivalent upsample if the shorter side is under 400 px, slight contrast stretch. Do not binarize by default (hurts UI screenshots).
2. Run Tesseract: `--oem 1 --psm 6 -l $langs --dpi 300 tsv`. Parse text, line boxes, per-word `conf`.
3. Compute **Tesseract quality**, never a raw mean confidence:
   - `mean_conf` of words with conf ≥ 0
   - `low_frac`: share of words with conf < 40
   - `garbage`: share of tokens matching `[^\p{L}\p{N}\s/.,:;!?'"-]` heavy or long consonant runs
   - `empty`: no words, or only whitespace
   - `tiny_source`: source shorter side < 80 px after crop
4. Fallback to RapidOCR if mode is Automatic and any of:
   - empty
   - mean_conf < 55 and at least 3 words
   - low_frac > 0.45
   - garbage > 0.25
   - tiny_source and mean_conf < 70
5. RapidOCR result is scored separately: character-level model score, line count vs image aspect, same garbage detector. **Do not compare Tesseract conf to RapidOCR conf.**
6. Pick RapidOCR when its quality band is `good` and Tesseract’s is `poor` or `empty`; otherwise keep Tesseract (including Tesseract-empty if RapidOCR is also empty).
7. Return `{text, engine, lines[]}` plus `fallback_used`.

Settings: Automatic / Tesseract only / RapidOCR only. Copy text from the OCR view. FTS5 indexes the chosen text for Glimpse.

Calibrate thresholds on a fixture set of UI screenshots, terminal text, small fonts, and photos of paper. Store fixtures under `crates/nyx-capture-core/tests/ocr/` (synthetic images, no third-party screenshots in git if license is unclear).

---

## GTK surfaces

### Overlay (layer shell)

- Layer `Overlay`, anchors all edges, `KeyboardMode::Exclusive`, namespace `nyx-capture`.
- Background: the grim still, then `@veil`.
- Selection: 1 px `@accent` stroke, `@nav_active` fill at low alpha.
- Chrome: dimension chip and magnifier only. No title bar, no libadwaita header.
- Colors from the same semantic tokens. No hardcoded hex.

If layer-shell is missing: exec slurp, then grim `-g`, then editor.

### Editor

Write-style command bar is optional; Photos’ side panel is the closer reference. For Capture, a dense bottom or side tool strip on the same `bg` surface:

Tools: arrow, line, rectangle, ellipse, freehand, highlighter, text, numbered marker, blur, pixelate, crop. Color swatches from the theme plus a few annotation primaries mixed from `danger`/`accent` (not Photos’ iOS hex list). Stroke size slider. Undo/redo. Copy, Save, Pin, OCR, Close.

Marks are objects. Original PNG is never overwritten. `{id}.ann.json` stores `EditState`. Export flattens.

Numbered markers are `Mark::Callout { at, n, colour }` auto-incremented.

Blur/pixelate stay objects until export; preview uses the Photos pipeline (pixelate exists there; blur is applied in the pixel pipeline before strokes).

### Pins

`gtk::ApplicationWindow` with class `io.github.design_nexus.nyx.Capture.Pin`. `install.sh` writes `~/.config/hypr/nyx.lua`:

```
o.window("^io\\.github\\.design_nexus\\.nyx\\.Capture\\.Pin$", { float = true, pin = true, size = { 480, 320 } })
```

Opacity slider, scroll-wheel resize, Esc or click-x dismisses. On compositors without pin, the window is a normal float.

### History

Virtualized grid of thumbnails (Photos grid pattern). Search uses FTS5. Filters from the sidebar. Delete is confirmed. Retention prunes on startup from a worker. History can be disabled: then files are not written unless the user Save-s.

---

## Schema v1

```sql
PRAGMA journal_mode=WAL;
CREATE TABLE captures (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,          -- screenshot | recording
  created_at INTEGER NOT NULL,
  width INTEGER, height INTEGER,
  path TEXT NOT NULL,
  thumb_path TEXT,
  dhash INTEGER,
  starred INTEGER NOT NULL DEFAULT 0,
  sensitive INTEGER NOT NULL DEFAULT 0,
  source TEXT,                 -- grim | portal | ...
  ocr_engine TEXT,
  ocr_status TEXT              -- pending | done
);
CREATE VIRTUAL TABLE captures_fts USING fts5(
  ocr, title,
  content='captures', content_rowid='rowid', tokenize='unicode61'
);
-- triggers as News
```

`sensitive` is never uploaded (nothing uploads in Phase 1). History off → no insert. Retention: delete files then rows older than N days, except starred.

---

## CLI and IPC

```
nyx-capture                 # history window
nyx-capture --region
nyx-capture --window
nyx-capture --active-window
nyx-capture --monitor [NAME]
nyx-capture --fullscreen
nyx-capture --delay SECS
nyx-capture --ocr           # region then copy text
nyx-capture --pin
nyx-capture --copy|--save|--edit
nyx-capture --section recent|starred|settings
nyx-capture --toggle
```

GApplication: a second invocation reaches the primary. Overlay requests still run even if History is open.

Control tools (while open):

- `capture.region` / `capture.window` / `capture.fullscreen`
- `capture.search` `{query}`
- `capture.open` `{id}`
- `capture.ocr_text` `{id}` (returns stored OCR; does not recapture)

Nothing deletes. Export uses `nyx_core::control::save_target`.

Hyprland binds stay in the user’s hypr config. Document suggested binds. Do not edit `/usr/share/omarchy` or `~/.config/hypr/hyprland.lua` except `nyx.lua` via `install.sh`.

---

## Prefs (`settings.toml`)

- theme fields already in `prefs_store!` / suite theme
- `sidebar_collapsed`
- `last_section`
- `after_capture`: `edit` | `copy` | `save` | `pin`
- `save_dir` (default XDG Pictures / Nyx Capture)
- `history`: bool, default true
- `retention_days`: i64, default 90, 0 = keep
- `ocr_mode`: `automatic` | `tesseract` | `rapidocr`
- `ocr_langs`: default `eng`
- `ocr_on_capture`: bool, default true, async
- `backend`: `auto` | `grim` | `portal`
- `selector`: `auto` | `overlay` | `slurp`

Recording keys appear in Phase 2 and are omitted from the card until a recorder is detected.

---

## Reuse (do not rebuild)

| Existing | Use |
| --- | --- |
| `nyx-core` | identity, XDG, prefs, cmd, control, atomic writes |
| `nyx-theme` / `nyx-ui` / `nyx-icons` | look, chrome, settings card, icon template |
| Photos `edit/marks.rs` + `pipeline.rs` | annotation objects, Cairo draw, blur/pixelate order |
| Assistant `screen.rs` | grim geometry, scale, layout↔image |
| Chat `composer.rs` screenshot | grim `-g` + slurp cancel handling |
| News store + FTS5 + glimpse plugin | schema/triggers/plugin/sandbox bind |
| `install.sh` / `bin/nyx` / `uninstall.sh` | add `capture` to `ALL`, deps, glimpse plugin copy, nyx.lua pin rule |
| Omarchy gpu-screen-recorder path | Phase 2 recorder detection, not Phase 1 UI |
| Music `window.rs` | sidebar hexpand false, compact, search, F1 |

Do not reuse Omarchy’s slurp+hyprpicker overlay as the Nyx selector. Do not extract Photos in Phase 1. Do not teach Chat to call Capture unless asked.

---

## Install

`install.sh` `app_info` entry:

- ids `Capture`
- bins `nyx-capture nyx-capture-glimpse`
- deps `grim tesseract gtk4-layer-shell tesseract-data-eng`
- optional at runtime: `slurp`, `hyprpicker`, portal backends, RapidOCR models (downloaded into cache on first fallback, or shipped as a data file if license allows)

`ALL` arrays in `install.sh`, `uninstall.sh`, and `bin/nyx`. Icon glyph in `nyx-icons/make_icons.py`. Desktop file `io.github.design_nexus.nyx.Capture.desktop`. App `install.sh`/`uninstall.sh` wrappers like News.

---

## Phase 1 milestones

Implement in this order. Each milestone is shippable.

### M0 — Spike (no UI polish)

- grim full/region/monitor on this Hyprland box; measure latency.
- gtk4-layer-shell overlay on a still.
- Tesseract TSV vs leptess.
- `rapidocr-core` load + one image; memory before/after lazy init.
- Write numbers into `apps/capture/BENCHMARKS.md`.

Accept: grim region round-trip documented; overlay draws; both OCR engines produce text from a fixture.

### M1 — Core crate + CLI capture

`nyx-capture-core` + `nyx-capture --fullscreen|--region|--monitor|--active-window` using grim/slurp, PNG to stdout or a temp file, `--copy` via `wl-copy`. No history UI.

Accept: `nyx-capture --region --copy` puts a PNG on the clipboard. Cancel is silent.

### M2 — Overlay selector

Custom overlay with magnifier, dimensions, window snap, keys. slurp fallback.

Accept: works on Hyprland multi-monitor; GNOME path uses portal or slurp without crashing.

### M3 — Annotation editor + clipboard

Tools listed above, undo/redo, object file, flatten export, copy annotated PNG.

Accept: open a fixture PNG, annotate, copy, original file unchanged.

### M4 — Pins

Floating pin windows, opacity, resize, dismiss, Hyprland `pin` rule.

### M5 — History + FTS5

SQLite, thumbnails, search, retention, disable history.

### M6 — Adaptive OCR

Tesseract default, RapidOCR lazy fallback, three modes, copy text, FTS index. Quality tests on fixtures.

### M7 — Shell, theme, control, Glimpse, install

Music history window, settings card, shortcuts, control interface, glimpse plugin + sandbox bind (with Ken’s yes on Glimpse), `install.sh capture`.

Phase 1 accepted when M0–M7 pass the tests below and the capture chord feels immediate on this machine.

---

## Phase 2 — Recording (1.5)

gpu-screen-recorder / wf-recorder / wl-screenrec / portal+PipeWire. System audio, mic, HW encode, MP4/WebM/GIF, cursor, countdown, pause if capable, basic ffmpeg trim. Settings card grows recorder rows only for detected backends. Webcam overlay optional.

## Phase 3 — Suite 2.0

Assistant send-screenshot (authorization switch, like News `assistant_can_subscribe`). Intelligent redaction. Glimpse semantic search (Glimpse already has embeddings). Tutorial capture. Scrolling capture only if a backend exists (omasnap scroll today). Drive sharing when Drive exists. No cloud in core.

Landed on this machine: `assistant_can_send` (default off) gates `capture.send_screenshot`. Editor Redact covers OCR line boxes with solid boxes (email/URL/phone/Luhn card/IPv4/`sk_|pk_|ghp_|github_pat_`) and marks the row sensitive, which drops FTS OCR and the `.ocr.txt` sidecar. Glimpse reader extracts stored OCR for PNGs and skips sensitive shots. `nyx-capture --scroll` opens the capture bar on Scroll; Capture stitches the frames itself (`nyx-capture-core::scroll`). Tutorial is a first-run dialog. Drive is skipped — no Drive app. Assistant’s grim ability is unchanged.

---

## Testing

```sh
cargo test -p nyx-capture-core
cargo test -p nyx-capture
cargo test -p nyx-capture-glimpse
cargo clippy -p nyx-capture-core -p nyx-capture -p nyx-capture-glimpse --all-targets -- -D warnings
```

Core tests: geometry scale, FTS query quoting (copy News `search.rs`), retention, OCR quality decisions with synthetic images, backend capability flags, annotation round-trip JSON.

No live compositor in unit tests. Overlay/editor get `CAPTURE_SNAPSHOT` like News/Music for stills.

Manual on Hyprland: region, window, fullscreen, two monitors, scaled monitor (image vs layout), delay, copy, pin, OCR on a terminal window, history search, history-off copy-only.

Do not claim UI done without exercising overlay → editor → copy on this desktop.

---

## Performance budget

| Path | Target |
| --- | --- |
| Cold `nyx-capture --region` to overlay visible | < 150 ms after grim returns |
| grim itself | whatever the box does; log it in M0 |
| Idle history window | no RapidOCR, no recorder |
| RapidOCR first load | async; UI stays usable |
| Annotation pan/zoom on 4K | 60 Hz on the still texture, marks redrawn in Cairo |

No persistent capture daemon.

---

## Out of scope until asked

- Editing Settings to call `nyx-capture` instead of `omarchy-capture-*`
- Replacing Assistant’s `abilities/capture` plugin
- Teaching Chat `--screenshot` to use Capture
- Extracting Photos’ editor into a shared crate
- Direct Wayland screencopy
- Cloud OCR, Drive, Clipboard app
- Qt / Electron

---

## Risks

| Risk | Response |
| --- | --- |
| gtk4-layer-shell missing on some sessions | slurp fallback; hide overlay-only options |
| grim absent (GNOME) | portal backend |
| rapidocr-core immature | spike; replace with `ort` + official models without changing the `OcrEngine` trait |
| leptess stale | Tesseract CLI is the default binding |
| Overlay steal/input on some compositors | still-image overlay, not live grab; portal otherwise |
| Glimpse sandbox blocks `~/.local` | same bind exception as News, plugin id exact |
| Scope bleed into Settings/Assistant | Phase 1 stays in capture crates + install lists + optional Glimpse bind |
| Annotation hex from Photos | replace with theme tokens |
