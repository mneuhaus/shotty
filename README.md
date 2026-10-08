# Shotty

Screenshots and screen recordings from the macOS menu bar, in Rust + [GPUI](https://www.gpui.rs) (via GPUI Kit). A personal replacement for CleanShot X, styled after Shottr.

- **⇧⌘4 Capture Area**: the screen freezes, drag an area, click a window, or Space for window mode. The PNG goes to the clipboard and `~/Pictures/Shotty`, then the editor opens.
- **⇧⌘3 Capture Screen**: the display under the mouse.
- **⇧⌘5 Record Screen**: drag an area or click a window or the desktop, then press Record in the bar below the area. Stop with the bar, ⇧⌘5 or the red menu bar icon. Afterwards trim the clip and export it as a compact MP4, an HEVC MP4 or a GIF.

## Editor

Tools: `V` select & move, `A` arrow, `L` line, `R` rectangle, `O` ellipse, `H` highlighter, `T` text, `N` numbered step, `B` pixelate, `C` crop. `1`–`7` pick a color.

| Key | Action |
|---|---|
| ⌘C | Copy and close |
| ⌘S | Save over the capture file |
| ⌘U | Save, upload to artifacts.neuhaus.nrw, copy the link |
| ⌘Z / ⇧⌘Z | Undo / redo |
| ⌫ | Delete the selected shape |
| Esc | Close (twice when there are unsaved annotations) |

## Uploads

Uploads go to [artifacts.neuhaus.nrw](https://artifacts.neuhaus.nrw) as unlisted drafts with a 128-bit random link. The link points at a viewer page with link previews and a download button. Uploads use the artifacts CLI's token (`$ARTIFACTS_TOKEN` or `~/.config/artifacts/token`, mode 600). Links never expire. The menu bar has "Copy Last Upload Link" and "Delete Last Upload…"; every upload is listed in `~/Library/Application Support/Shotty/history.json`.

## Install

```sh
swift tools/make-icon.swift          # only after changing the icon
tools/bundle-macos.sh --install      # release build, signed, copied to /Applications
```

The bundle is signed with the first "Apple Development" identity in the keychain (or `$SHOTTY_SIGN`). macOS ties the Screen Recording permission to that signature, so it survives rebuilds. With an ad-hoc signature macOS would ask again after every build.

Video export needs ffmpeg (`brew install ffmpeg`, or set `SHOTTY_FFMPEG`).

Quit CleanShot X (or remove its shortcuts) before using ⇧⌘3/4/5 here. Shortcuts that are already taken show up in the menu without their key.

## Config

`~/Library/Application Support/Shotty/config.toml` is written with the defaults on first start:

```toml
save_dir = "/Users/you/Pictures/Shotty"

[hotkeys]
capture_screen = "shift+super+Digit3"
capture_area = "shift+super+Digit4"
record = "shift+super+Digit5"

[recording]
fps = 30
show_cursor = true
show_clicks = true
microphone = false
system_audio = false

[upload]
url = "https://artifacts.neuhaus.nrw"
copy = "page"            # or "direct" for the file URL (Markdown embedding)
# token_path = "/path/to/token"
```

## Layout

| Crate | |
|---|---|
| `shotty-app` | GPUI app: menu bar item, hotkeys, selection overlay, editor, recording bar, clip window |
| `shotty-capture` | ScreenCaptureKit: displays, windows, screenshots, `SCRecordingOutput` recordings |
| `shotty-annotate` | Annotation model and full-resolution rendering (tiny-skia, Barlow font) |
| `shotty-media` | ffmpeg: probe, frames, trim, MP4 compression, GIF |
| `shotty-share` | artifacts.neuhaus.nrw upload client |

## Development

`SHOTTY_CONFIG=<file>` uses another config, `SHOTTY_SUPPORT=<dir>` another history location. `SHOTTY_DEMO` drives the UI without input, for screenshots and smoke tests: `area`, `area-drag`, `area-window`, `area-cross`, `screen`, `screen-shapes`, `screen-upload` and `record-auto`. Demo runs never touch the clipboard.
