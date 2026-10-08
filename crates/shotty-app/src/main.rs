//! Shotty: screenshots and screen recordings from the menu bar.
//!
//! Environment: `SHOTTY_CONFIG=<file>` uses another config file,
//! `SHOTTY_DEMO=area` opens the selection overlay right after start (for testing).

mod config;
mod editor;
mod hotkeys;
mod native;
mod output;
mod overlay;
mod recorder;

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use image::RgbaImage;
use objc2_app_kit::NSEventModifierFlags;
use shotty_capture::{DisplayInfo, DisplayShot, Rect, Recording, WindowInfo};

use crate::config::Config;
use crate::overlay::{Outcome, Overlay, Purpose};
use crate::recorder::{Phase, RecordAction};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    CaptureScreen,
    CaptureArea,
    Record,
    OpenFolder,
    Quit,
}

/// Everything that reaches the app from outside GPUI's own event handling.
pub enum Event {
    Command(Command),
    Menu(usize),
    Overlay(Outcome),
    Record(RecordAction),
}

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        MousePointer2, MoveUpRight, Circle, Highlighter, Type, ListOrdered, Grid3x3, Crop, Save, CloudUpload, Video,
        Scissors, MicOff, CircleStop, Link, Film
    ]
);

struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(b) = ExtraIcons.load(path)? {
            return Ok(Some(b));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut v = gpui_kit::assets::Assets.list(path)?;
        v.extend(ExtraIcons.list(path)?);
        v.sort();
        v.dedup();
        Ok(v)
    }
}

static EVENTS: OnceLock<async_channel::Sender<Event>> = OnceLock::new();

pub fn events() -> &'static async_channel::Sender<Event> {
    EVENTS.get().expect("event channel is set up in main")
}

/// An open selection overlay: one window per display plus the frozen frames behind them
/// (screenshots) or the displays it covers (recordings).
struct Session {
    windows: Vec<AnyWindowHandle>,
    shots: Vec<Arc<DisplayShot>>,
    displays: Vec<(DisplayInfo, f32)>,
    purpose: Purpose,
}

/// A recording from area selection until the file is written.
struct Recorder {
    display_id: u32,
    area: Rect,
    windows: recorder::Windows,
    mic: bool,
    phase: Phase,
    recording: Option<Recording>,
}

struct Shotty {
    config: Config,
    status: native::StatusItem,
    _hotkeys: hotkeys::Hotkeys,
    menu: Vec<Command>,
    session: Option<Session>,
    recorder: Option<Recorder>,
    busy: bool,
    failed_hotkeys: Vec<String>,
}

impl Global for Shotty {}

fn build_menu(config: &Config, failed: &[String], recording: bool) -> (Vec<native::MenuEntry>, Vec<Command>) {
    let key = |spec: &str| {
        let shown = hotkeys::display(spec);
        let mut mods = NSEventModifierFlags::empty();
        for (sym, flag) in [
            ('⌃', NSEventModifierFlags::Control),
            ('⌥', NSEventModifierFlags::Option),
            ('⇧', NSEventModifierFlags::Shift),
            ('⌘', NSEventModifierFlags::Command),
        ] {
            if shown.contains(sym) {
                mods |= flag;
            }
        }
        let key = shown.trim_start_matches(['⌃', '⌥', '⇧', '⌘']).to_lowercase();
        (!failed.iter().any(|f| f == spec)).then_some((key, mods))
    };
    let h = &config.hotkeys;
    let entries = vec![
        (Command::CaptureArea, "Capture Area", key(&h.capture_area), false),
        (Command::CaptureScreen, "Capture Screen", key(&h.capture_screen), false),
        (Command::Record, if recording { "Stop Recording" } else { "Record Screen" }, key(&h.record), false),
        (Command::OpenFolder, "Open Shotty Folder", None, true),
        (Command::Quit, "Quit Shotty", Some(("q".into(), NSEventModifierFlags::Command)), true),
    ];
    let commands = entries.iter().map(|e| e.0).collect();
    let menu = entries
        .into_iter()
        .map(|(_, title, key, separator_before)| native::MenuEntry { title: title.into(), key, separator_before })
        .collect();
    (menu, commands)
}

fn apply_theme(window: Option<&mut Window>, cx: &mut App) {
    match std::env::var("SHOTTY_THEME").as_deref() {
        Ok("dark") => Theme::change(ThemeMode::Dark, window, cx),
        Ok("light") => Theme::change(ThemeMode::Light, window, cx),
        _ => Theme::sync_system_appearance(window, cx),
    }
}

fn handle(event: Event, cx: &mut App) {
    match event {
        Event::Menu(i) => {
            if let Some(&command) = cx.global::<Shotty>().menu.get(i) {
                handle(Event::Command(command), cx);
            }
        }
        Event::Command(Command::CaptureArea) => start_area_capture(cx),
        Event::Command(Command::CaptureScreen) => capture_screen(cx),
        Event::Command(Command::Record) => match cx.global::<Shotty>().recorder.as_ref().map(|r| r.phase) {
            Some(Phase::Ready) => record_action(RecordAction::Start, cx),
            Some(Phase::Recording(_)) => record_action(RecordAction::Stop, cx),
            Some(_) => {}
            None => start_record_selection(cx),
        },
        Event::Record(action) => record_action(action, cx),
        Event::Command(Command::OpenFolder) => {
            let dir = cx.global::<Shotty>().config.save_dir.clone();
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::process::Command::new("open").arg(dir).spawn();
        }
        Event::Command(Command::Quit) => cx.quit(),
        Event::Overlay(outcome) => finish_overlay(outcome, cx),
    }
}

/// Freezes all displays in the background, then covers each with an overlay.
fn start_area_capture(cx: &mut App) {
    let state = cx.global_mut::<Shotty>();
    if state.session.is_some() || state.busy {
        return;
    }
    state.busy = true;
    log::info!("area capture: freezing displays");
    let task = cx.background_executor().spawn(async { (shotty_capture::capture_all_displays(), shotty_capture::windows()) });
    cx.spawn(async move |cx| {
        let (shots, windows) = task.await;
        cx.update(|cx| {
            cx.global_mut::<Shotty>().busy = false;
            match shots {
                Ok(shots) => {
                    let shots: Vec<Arc<DisplayShot>> = shots.into_iter().map(Arc::new).collect();
                    let displays = shots.iter().map(|s| (s.display.clone(), s.scale() as f32)).collect();
                    open_overlays(displays, shots, windows.unwrap_or_default(), cx)
                }
                Err(e) => eprintln!("shotty: capture failed: {e:#}"),
            }
        })
    })
    .detach();
}

/// Live selection for a recording: same overlay, but see-through instead of a frozen frame.
fn start_record_selection(cx: &mut App) {
    let state = cx.global_mut::<Shotty>();
    if state.session.is_some() || state.busy {
        return;
    }
    state.busy = true;
    let task = cx.background_executor().spawn(async {
        let displays = shotty_capture::displays()?;
        // The pixel density per display comes from a cheap capture-info query.
        let scaled = displays.into_iter().map(|d| (d.clone(), shotty_capture::scale_of(d.id).unwrap_or(2.0) as f32)).collect::<Vec<_>>();
        Ok::<_, anyhow::Error>((scaled, shotty_capture::windows().unwrap_or_default()))
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            cx.global_mut::<Shotty>().busy = false;
            match result {
                Ok((displays, windows)) => open_overlays(displays, Vec::new(), windows, cx),
                Err(e) => log::error!("recording unavailable: {e:#}"),
            }
        })
    })
    .detach();
}

/// Covers every display with a selection overlay. With `shots` the overlays show those frozen
/// frames (screenshot); without, they are see-through (recording).
fn open_overlays(mut displays: Vec<(DisplayInfo, f32)>, shots: Vec<Arc<DisplayShot>>, windows: Vec<WindowInfo>, cx: &mut App) {
    log::info!("opening {} overlays ({} windows known)", displays.len(), windows.len());
    let purpose = if shots.is_empty() { Purpose::Record } else { Purpose::Screenshot };
    let windows = Arc::new(windows);
    // The display under the mouse opens last, so it ends up as the key window and gets Esc/Space.
    let (mx, my) = native::mouse_location();
    displays.sort_by_key(|(d, _)| d.frame.contains(mx, my));
    let mut handles = Vec::new();
    for (display, scale) in &displays {
        let frame = display.frame;
        let display_id = display.id;
        let picture = shots.iter().find(|s| s.display.id == display_id).map(|s| output::render_image(&s.image));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(frame.width as f32), px(frame.height as f32)),
            ))),
            titlebar: None,
            focus: true,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id: Some(DisplayId::new(u64::from(display_id))),
            window_background: if picture.is_some() {
                WindowBackgroundAppearance::Opaque
            } else {
                WindowBackgroundAppearance::Transparent
            },
            ..Default::default()
        };
        let (display, scale, windows) = (display.clone(), *scale, windows.clone());
        let opened = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| Overlay::new(display, scale, picture, windows, cx));
            let focus = view.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
            view
        });
        match opened {
            Ok(handle) => {
                log::info!("overlay open on display {display_id}");
                let _ = handle.update(cx, |_, window, _| native::make_fullscreen_overlay(window, display_id));
                handles.push(handle.into());
            }
            Err(e) => eprintln!("shotty: overlay failed: {e:#}"),
        }
    }
    cx.global_mut::<Shotty>().session = Some(Session { windows: handles, shots, displays, purpose });
}

fn close_session(cx: &mut App) -> Option<Session> {
    let session = cx.global_mut::<Shotty>().session.take()?;
    for handle in &session.windows {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
    Some(session)
}

fn finish_overlay(outcome: Outcome, cx: &mut App) {
    let Some(session) = close_session(cx) else { return };
    if session.purpose == Purpose::Record {
        if let Outcome::Area { display_id, rect } = outcome
            && let Some((display, scale)) = session.displays.iter().find(|(d, _)| d.id == display_id)
        {
            prepare_recording(display, *scale, rect, cx);
        }
        return;
    }
    match outcome {
        Outcome::Cancel => {}
        Outcome::Area { display_id, rect } => {
            if let Some(shot) = session.shots.iter().find(|s| s.display.id == display_id) {
                let scale = shot.scale() as f32;
                deliver(shot.crop(rect), scale, cx);
            }
        }
        Outcome::Window { id } => {
            let scale = session.shots.first().map_or(2.0, |s| s.scale() as f32);
            let task = cx.background_executor().spawn(async move { shotty_capture::capture_window(id) });
            cx.spawn(async move |cx| {
                let image = task.await;
                cx.update(|cx| match image {
                    Ok(image) => deliver(image, scale, cx),
                    Err(e) => eprintln!("shotty: window capture failed: {e:#}"),
                })
            })
            .detach();
        }
    }
}

/// The display under the mouse, straight to the clipboard and the preview.
fn capture_screen(cx: &mut App) {
    let (mx, my) = native::mouse_location();
    let task = cx.background_executor().spawn(async move {
        let shots = shotty_capture::capture_all_displays()?;
        let index = shots.iter().position(|s| s.display.frame.contains(mx, my)).unwrap_or(0);
        Ok::<_, anyhow::Error>(shots.into_iter().nth(index))
    });
    cx.spawn(async move |cx| {
        let shot = task.await;
        cx.update(|cx| match shot {
            Ok(Some(shot)) => {
                let scale = shot.scale() as f32;
                deliver(shot.image, scale, cx)
            }
            Ok(None) => {}
            Err(e) => eprintln!("shotty: capture failed: {e:#}"),
        })
    })
    .detach();
}

/// Encodes and saves in the background, then copies to the clipboard and opens the preview.
fn deliver(image: RgbaImage, scale: f32, cx: &mut App) {
    let dir = cx.global::<Shotty>().config.save_dir.clone();
    let task = cx.background_executor().spawn(async move {
        let png = output::encode_png(&image)?;
        let path = output::save_png(&dir, &png)?;
        Ok::<_, anyhow::Error>((image, png, path))
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok((image, png, path)) => {
                if std::env::var_os("SHOTTY_DEMO").is_none() {
                    output::copy_png(png, cx);
                }
                editor::open(Arc::new(image), path, scale, cx);
            }
            Err(e) => eprintln!("shotty: saving failed: {e:#}"),
        })
    })
    .detach();
}

fn refresh_menu(cx: &mut App) {
    let state = cx.global_mut::<Shotty>();
    let recording = state.recorder.as_ref().is_some_and(|r| matches!(r.phase, Phase::Recording(_)));
    let (entries, menu) = build_menu(&state.config, &state.failed_hotkeys, recording);
    state.status.set_menu(&entries);
    state.status.set_recording(recording);
    state.menu = menu;
}

fn prepare_recording(display: &DisplayInfo, scale: f32, area: Rect, cx: &mut App) {
    if area.width < 16. || area.height < 16. {
        return;
    }
    let mic = cx.global::<Shotty>().config.recording.microphone;
    match recorder::open(display, area, scale, mic, cx) {
        Ok(windows) => {
            cx.global_mut::<Shotty>().recorder =
                Some(Recorder { display_id: display.id, area, windows, mic, phase: Phase::Ready, recording: None });
        }
        Err(e) => log::error!("could not open the recording controls: {e:#}"),
    }
}

fn set_phase(phase: Phase, cx: &mut App) {
    let Some(rec) = cx.global_mut::<Shotty>().recorder.as_mut() else { return };
    rec.phase = phase;
    let windows = recorder::Windows { bar: rec.windows.bar, frame: rec.windows.frame };
    windows.set_phase(phase, cx);
    if matches!(phase, Phase::Recording(_)) {
        windows.tick(cx);
    }
    refresh_menu(cx);
}

fn end_recording(cx: &mut App) {
    if let Some(rec) = cx.global_mut::<Shotty>().recorder.take() {
        rec.windows.close(cx);
    }
    refresh_menu(cx);
}

fn record_action(action: RecordAction, cx: &mut App) {
    let state = cx.global_mut::<Shotty>();
    let Some(rec) = state.recorder.as_mut() else { return };
    match (action, rec.phase) {
        (RecordAction::ToggleMic, Phase::Ready) => {
            rec.mic = !rec.mic;
            state.config.recording.microphone = rec.mic;
            let (mic, windows) = (rec.mic, recorder::Windows { bar: rec.windows.bar, frame: rec.windows.frame });
            windows.set_mic(mic, cx);
        }
        (RecordAction::Start, Phase::Ready) => {
            let r = &state.config.recording;
            let opts = shotty_capture::RecordOptions {
                display_id: rec.display_id,
                area: rec.area,
                fps: r.fps,
                cursor: r.show_cursor,
                clicks: r.show_clicks,
                microphone: rec.mic,
                system_audio: r.system_audio,
                path: output::unique_path(&state.config.save_dir, "Shotty", "mp4"),
            };
            set_phase(Phase::Starting, cx);
            let task = cx.background_executor().spawn(async move { Recording::start(&opts) });
            cx.spawn(async move |cx| {
                let started = task.await;
                cx.update(|cx| match started {
                    Ok(recording) => {
                        if let Some(rec) = cx.global_mut::<Shotty>().recorder.as_mut() {
                            rec.recording = Some(recording);
                        }
                        set_phase(Phase::Recording(std::time::Instant::now()), cx);
                    }
                    Err(e) => {
                        log::error!("recording failed to start: {e:#}");
                        end_recording(cx);
                    }
                })
            })
            .detach();
        }
        (RecordAction::Stop, Phase::Recording(_)) => {
            let Some(recording) = rec.recording.take() else { return };
            set_phase(Phase::Saving, cx);
            let task = cx.background_executor().spawn(async move { recording.stop() });
            cx.spawn(async move |cx| {
                let saved = task.await;
                cx.update(|cx| {
                    end_recording(cx);
                    match saved {
                        Ok(path) => recorded(path, cx),
                        Err(e) => log::error!("{e:#}"),
                    }
                })
            })
            .detach();
        }
        (RecordAction::Cancel, _) => {
            if let Some(recording) = rec.recording.take() {
                cx.background_executor().spawn(async move { recording.cancel() }).detach();
            }
            end_recording(cx);
        }
        _ => {}
    }
}

/// A finished recording: on the clipboard as a file, then into the clip window.
fn recorded(path: std::path::PathBuf, _cx: &mut App) {
    log::info!("recording saved: {}", path.display());
    if std::env::var_os("SHOTTY_DEMO").is_none() {
        native::copy_files_to_clipboard(std::slice::from_ref(&path));
    }
    if std::env::var_os("SHOTTY_DEMO").is_none() {
        let _ = std::process::Command::new("open").arg("-R").arg(&path).spawn();
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("shotty=info")).init();
    let (tx, rx) = async_channel::unbounded();
    EVENTS.set(tx.clone()).ok();
    gpui_kit::application().with_assets(AppAssets).run(move |cx| {
        gpui_kit::init(cx);
        if let Err(e) = cx.text_system().add_fonts(vec![Cow::Borrowed(shotty_annotate::FONT)]) {
            log::warn!("annotation font unavailable: {e:#}");
        }
        apply_theme(None, cx);
        cx.set_activation_policy(ActivationPolicy::Accessory);
        editor::bind_keys(cx);

        let config = Config::load();
        let h = &config.hotkeys;
        let bindings = [
            (Command::CaptureScreen, h.capture_screen.clone()),
            (Command::CaptureArea, h.capture_area.clone()),
            (Command::Record, h.record.clone()),
        ];
        let hotkeys = match hotkeys::register(&bindings, tx.clone()) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("shotty: hotkeys unavailable: {e:#}");
                cx.quit();
                return;
            }
        };
        let status = native::StatusItem::new();
        let (entries, menu) = build_menu(&config, &hotkeys.failed, false);
        status.set_menu(&entries);
        let failed_hotkeys = hotkeys.failed.clone();
        cx.set_global(Shotty {
            config,
            status,
            _hotkeys: hotkeys,
            menu,
            session: None,
            recorder: None,
            busy: false,
            failed_hotkeys,
        });

        cx.spawn(async move |cx| {
            while let Ok(event) = rx.recv().await {
                cx.update(|cx| handle(event, cx));
            }
        })
        .detach();

        match std::env::var("SHOTTY_DEMO").as_deref() {
            Ok(d) if d.starts_with("area") => tx.try_send(Event::Command(Command::CaptureArea)).ok(),
            Ok(d) if d.starts_with("screen") => tx.try_send(Event::Command(Command::CaptureScreen)).ok(),
            // Records a fixed area on the main display for ~3 s without any clicks.
            Ok("record-auto") => {
                let tx = tx.clone();
                cx.spawn(async move |cx| {
                    let display = shotty_capture::displays().ok().and_then(|d| d.into_iter().find(|d| d.frame.x == 0. && d.frame.y == 0.));
                    let Some(display) = display else { return };
                    let area = Rect { x: 300., y: 200., width: 800., height: 500. };
                    cx.update(|cx| prepare_recording(&display, 2.0, area, cx));
                    for (delay, action) in [(1000, RecordAction::Start), (3500, RecordAction::Stop)] {
                        cx.background_executor().timer(std::time::Duration::from_millis(delay)).await;
                        tx.try_send(Event::Record(action)).ok();
                    }
                })
                .detach();
                None
            }
            Ok(d) if d.starts_with("record") => tx.try_send(Event::Command(Command::Record)).ok(),
            _ => None,
        };
    });
}
