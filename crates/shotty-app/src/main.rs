//! Shotty: screenshots and screen recordings from the menu bar.
//!
//! Environment: `SHOTTY_CONFIG=<file>` uses another config file,
//! `SHOTTY_DEMO=area` opens the selection overlay right after start (for testing).

mod config;
mod hotkeys;
mod native;
mod output;
mod overlay;
mod preview;

use std::sync::{Arc, OnceLock};

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use image::RgbaImage;
use objc2_app_kit::NSEventModifierFlags;
use shotty_capture::{DisplayShot, WindowInfo};

use crate::config::Config;
use crate::overlay::{Outcome, Overlay};

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
}

static EVENTS: OnceLock<async_channel::Sender<Event>> = OnceLock::new();

pub fn events() -> &'static async_channel::Sender<Event> {
    EVENTS.get().expect("event channel is set up in main")
}

/// An open selection overlay: one window per display plus the frozen frames behind them.
struct Session {
    windows: Vec<AnyWindowHandle>,
    shots: Vec<Arc<DisplayShot>>,
}

struct Shotty {
    config: Config,
    status: native::StatusItem,
    _hotkeys: hotkeys::Hotkeys,
    menu: Vec<Command>,
    session: Option<Session>,
    busy: bool,
}

impl Global for Shotty {}

fn build_menu(config: &Config, failed: &[String]) -> (Vec<native::MenuEntry>, Vec<Command>) {
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
        (Command::Record, "Record Screen", key(&h.record), false),
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
        Event::Command(Command::Record) => eprintln!("shotty: recording is not built yet"),
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
                Ok(shots) => open_overlays(shots, windows.unwrap_or_default(), cx),
                Err(e) => eprintln!("shotty: capture failed: {e:#}"),
            }
        })
    })
    .detach();
}

fn open_overlays(shots: Vec<DisplayShot>, windows: Vec<WindowInfo>, cx: &mut App) {
    log::info!("opening {} overlays ({} windows known)", shots.len(), windows.len());
    let windows = Arc::new(windows);
    let mut shots: Vec<Arc<DisplayShot>> = shots.into_iter().map(Arc::new).collect();
    // The display under the mouse opens last, so it ends up as the key window and gets Esc/Space.
    let (mx, my) = native::mouse_location();
    shots.sort_by_key(|s| s.display.frame.contains(mx, my));
    let mut handles = Vec::new();
    for shot in &shots {
        let frame = shot.display.frame;
        let display_id = shot.display.id;
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
            window_background: WindowBackgroundAppearance::Opaque,
            ..Default::default()
        };
        let (shot, windows) = (shot.clone(), windows.clone());
        let opened = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| Overlay::new(shot, windows, cx));
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
    cx.global_mut::<Shotty>().session = Some(Session { windows: handles, shots });
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
                output::copy_png(png.clone(), cx);
                preview::open(Arc::new(image), Arc::new(png), path, scale, cx);
            }
            Err(e) => eprintln!("shotty: saving failed: {e:#}"),
        })
    })
    .detach();
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("shotty=info")).init();
    let (tx, rx) = async_channel::unbounded();
    EVENTS.set(tx.clone()).ok();
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        apply_theme(None, cx);
        cx.set_activation_policy(ActivationPolicy::Accessory);
        preview::bind_keys(cx);

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
        let (entries, menu) = build_menu(&config, &hotkeys.failed);
        status.set_menu(&entries);
        cx.set_global(Shotty { config, status, _hotkeys: hotkeys, menu, session: None, busy: false });

        cx.spawn(async move |cx| {
            while let Ok(event) = rx.recv().await {
                cx.update(|cx| handle(event, cx));
            }
        })
        .detach();

        if std::env::var("SHOTTY_DEMO").is_ok_and(|d| d.starts_with("area")) {
            tx.try_send(Event::Command(Command::CaptureArea)).ok();
        }
    });
}
