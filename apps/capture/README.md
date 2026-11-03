# Capture

Screenshots, annotation, OCR and screen recording for Omarchy and Hyprland, with a
searchable history.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/design-nexus/nyx-capture/main/install.sh | bash -s -- capture
```

This installs grim, wl-clipboard, Tesseract (with English) and gtk4-layer-shell if they're
missing, builds with Cargo, and installs `nyx-capture` and its Glimpse plugin to
`~/.local/bin` with a launcher entry. To remove it, run the same line with `uninstall.sh`
in place of `install.sh`.

## Capturing

- **Region** drags on a frozen still of the screen, with a magnifier, the size, and snapping
  to windows on screen. **Window** picks a window; **Screen** takes the focused monitor.
  Without layer-shell, slurp picks the region.
- After a shot: open it in the editor (the default), copy it, save it, read its text, or pin
  it. Settings → Capture picks which, and the Save folder.
- **Scrolling capture** of a long region, through omasnap.
- Each shot is titled with the window that was focused, for history and Glimpse.

## Editing

- Arrow, line, rectangle, ellipse, freehand, highlighter, text and numbered markers in the
  theme's colours; blur, pixelate and a solid box for hiding things; crop.
- Select and move a mark, or Delete it. Ctrl+scroll zooms; Ctrl+0 fits.
- **Redact** reads the shot's text and covers emails, URLs, phone numbers, card numbers,
  addresses and keys with solid boxes; the shot is then marked sensitive (its text isn't
  indexed and Nyx Assistant isn't given it).
- The original file is never changed: edits are kept beside it. Everything handed out
  (copy, save, pin, the history thumbnail, Nyx Assistant) has the edits burned in.

## History

- Captures and recordings under day headings, searchable by their text and title; star
  the ones to keep.
- Ctrl+click or Shift+click selects several; Delete removes them, with Undo in the
  message that follows. Files go only when that message has gone.
- Unstarred screenshots older than the retention period (Settings → History) are removed
  on launch. Recordings are kept: they're in your Videos folder.
- Shots taken from a key binding go into history too, their text read in the background.
- With history off, nothing is kept beyond what you save.

## Text

Tesseract reads the text (Settings → OCR → Language, for any language with
`tesseract-data-*` installed). Automatic mode tries RapidOCR on text Tesseract reads poorly,
once its models are downloaded (Settings → OCR → RapidOCR models).

## Recording

- gpu-screen-recorder, wf-recorder, wl-screenrec, or PipeWire through the ScreenCast portal,
  whichever is installed, in that order. Settings shows only the rows that apply.
- The focused monitor or a dragged region; system sound, the mic, or both
  (wf-recorder and wl-screenrec record one of them: system sound first).
- MP4, WebM or GIF (converted with ffmpeg); an optional countdown; the pointer on or off.
- While recording, a bar at the top of the screen shows the time and has pause (with
  gpu-screen-recorder) and stop.
- A picture-in-picture webcam overlay through mpv and Omarchy's `WebcamOverlay` rules.
- Recordings are saved to your Videos folder. History can trim one with ffmpeg.

## Usage

```
nyx-capture                    History window
nyx-capture --region           Drag a region, copy the PNG
nyx-capture --window           Pick a window
nyx-capture --active-window    The focused window
nyx-capture --monitor [NAME]   One monitor (the focused one without NAME)
nyx-capture --fullscreen       The focused monitor
nyx-capture --delay SECS       Wait first
nyx-capture --ocr              Region, then copy its text
nyx-capture --ocr-file PATH    Print the text in an image file (--json: {text, engine})
nyx-capture --pin              Region, then pin it
nyx-capture --edit             Region, then the editor
nyx-capture --copy | --save [PATH]
nyx-capture --scroll           Scrolling capture (omasnap)
nyx-capture --record [--region] [--webcam]
nyx-capture --record --stop | --pause
nyx-capture --section ID       Open Recent, Starred, or settings
nyx-capture --toggle           Close the window if it's open, otherwise open it
```

Suggested Hyprland binds (install doesn't write them):

```lua
o.bind("SUPER + SHIFT + S", "Capture region", "nyx-capture --region")
o.bind("SUPER + SHIFT + W", "Capture window", "nyx-capture --window")
o.bind("PRINT", "Capture screen", "nyx-capture --fullscreen")
o.bind("SUPER + SHIFT + O", "Capture text", "nyx-capture --ocr")
```

| Key | Action |
| --- | --- |
| Ctrl+N | Capture a region |
| Ctrl+Shift+N | Capture a window |
| Ctrl+Shift+F | Capture the focused monitor |
| Ctrl+Shift+L | Scrolling capture |
| Ctrl+R | Start or stop a recording |
| Ctrl+Shift+R | Record a region |
| Ctrl+Alt+P | Pause or resume a recording |
| Ctrl+F | Search history |
| Ctrl+Click, Shift+Click | Select captures |
| Ctrl+A | Select every capture shown |
| Delete | Delete the selected captures |
| Esc | Clear the selection |
| Ctrl+B | Collapse the sidebar |
| Ctrl+, | Settings |
| F1 | Keyboard shortcuts |
| Ctrl+Q | Close |

In the editor: V select, A arrow, L line, R rectangle, E ellipse, P freehand,
H highlighter, T text, N marker, B blur, X pixelate, D solid box, C crop; Delete removes the
selected mark; Ctrl+Z / Ctrl+Shift+Z undo and redo; Ctrl+C copies; Ctrl+S saves; F1 lists
them.

## Glimpse and Nyx Assistant

`nyx-capture-glimpse` searches history text (and copies it) and runs `capture region`,
`capture window` and `capture screen`. Nyx Assistant can take a capture, search history,
read a capture's text and control recording over the session bus (sending it a screenshot
is off until Settings → Privacy allows it).

## Files

- History: `~/.local/share/nyx-capture/` (`capture.sqlite` and the captures)
- Thumbnails: `~/.cache/nyx-capture/thumbs/`
- Settings: `~/.config/nyx-capture/settings.toml`
