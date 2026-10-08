//! Screen recording UI: a frame around the chosen area and a small control bar below it.
//!
//! After selecting the area the bar waits ("Record", microphone toggle, cancel), so the selection
//! gesture never ends up in the video. Shotty's own windows are excluded from the recording.

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::base::h_flex;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Selectable as _, Sizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use shotty_capture::{DisplayInfo, Rect};

use crate::{Event, events};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordAction {
    Start,
    Stop,
    Cancel,
    ToggleMic,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Ready,
    Starting,
    Recording(Instant),
    Saving,
}

pub struct RecordBar {
    pub phase: Phase,
    pub mic: bool,
    /// Size of the recorded area in pixels, for the readout.
    pixels: (u32, u32),
    focus: FocusHandle,
}

fn send(action: RecordAction) {
    events().try_send(Event::Record(action)).ok();
}

fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

impl RecordBar {
    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match (event.keystroke.key.as_str(), self.phase) {
            ("enter" | "space", Phase::Ready) => send(RecordAction::Start),
            ("enter" | "space", Phase::Recording(_)) => send(RecordAction::Stop),
            ("escape", _) => send(RecordAction::Cancel),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for RecordBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let red = rgb(0xff3b30);
        let pill = h_flex()
            .key_context("RecordBar")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .h(px(44.))
            .px(px(8.))
            .gap(px(8.))
            .rounded(px(22.))
            .bg(rgb(0x1f1f23))
            .border_1()
            .border_color(white().opacity(0.12))
            .text_color(white())
            .text_sm();
        let pill = match self.phase {
            Phase::Ready => pill
                .child(
                    Button::new("record")
                        .label("Record")
                        .icon(IconName::CircleStop)
                        .danger()
                        .small()
                        .tooltip("Start recording  ↩")
                        .on_click(|_, _, _| send(RecordAction::Start)),
                )
                .child(
                    Button::new("mic")
                        .icon(if self.mic { IconName::Mic } else { IconName::MicOff })
                        .ghost()
                        .small()
                        .selected(self.mic)
                        .tooltip(if self.mic { "Microphone on" } else { "Microphone off" })
                        .on_click(|_, _, _| send(RecordAction::ToggleMic)),
                )
                .child(div().text_xs().text_color(white().opacity(0.6)).child(format!("{} × {}", self.pixels.0, self.pixels.1))),
            Phase::Starting => pill.child(div().px(px(8.)).child("Starting…")),
            Phase::Recording(since) => pill
                .child(div().size(px(10.)).ml(px(6.)).rounded_full().bg(red))
                .child(div().min_w(px(44.)).font_weight(FontWeight::MEDIUM).child(clock(since.elapsed())))
                .when(self.mic, |p| p.child(Icon::new(IconName::Mic).small().text_color(white().opacity(0.6))))
                .child(
                    Button::new("stop")
                        .label("Stop")
                        .icon(IconName::Square)
                        .primary()
                        .small()
                        .tooltip("Stop  ↩ or ⇧⌘5")
                        .on_click(|_, _, _| send(RecordAction::Stop)),
                ),
            Phase::Saving => pill.child(div().px(px(8.)).child("Saving…")),
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(pill.child(
                Button::new("cancel")
                    .icon(IconName::Close)
                    .ghost()
                    .small()
                    .tooltip("Cancel  Esc")
                    .on_click(|_, _, _| send(RecordAction::Cancel)),
            ))
    }
}

/// The frame drawn just outside the recorded area (it never appears in the video).
pub struct RecordFrame {
    pub recording: bool,
}

impl Render for RecordFrame {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let color = if self.recording { rgb(0xff3b30).into() } else { white().opacity(0.9) };
        div().size_full().border_2().border_color(color).when(!self.recording, |d| d.border_dashed())
    }
}

pub struct Windows {
    pub bar: WindowHandle<RecordBar>,
    pub frame: WindowHandle<RecordFrame>,
}

const BAR: (f32, f32) = (330., 56.);
const FRAME_GAP: f32 = 3.;

fn popup(display: &DisplayInfo, bounds: Bounds<Pixels>, focus: bool, background: WindowBackgroundAppearance) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: None,
        focus,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: Some(DisplayId::new(u64::from(display.id))),
        window_background: background,
        ..Default::default()
    }
}

/// Opens the frame and the control bar for `area` (global points) on `display`.
pub fn open(display: &DisplayInfo, area: Rect, scale: f32, mic: bool, cx: &mut App) -> anyhow::Result<Windows> {
    let local = |x: f64, y: f64| point(px((x - display.frame.x) as f32), px((y - display.frame.y) as f32));
    let g = f64::from(FRAME_GAP);
    let frame_bounds = Bounds::new(
        local(area.x - g, area.y - g),
        size(px((area.width + 2. * g) as f32), px((area.height + 2. * g) as f32)),
    );
    let frame = cx.open_window(popup(display, frame_bounds, false, WindowBackgroundAppearance::Transparent), |_, cx| {
        cx.new(|_| RecordFrame { recording: false })
    })?;
    frame.update(cx, |_, window, _| {
        crate::native::make_borderless(window);
        crate::native::ignore_mouse(window);
    })?;

    // Below the area if it fits, else above it, else inside along its bottom edge.
    let (bw, bh) = (f64::from(BAR.0), f64::from(BAR.1));
    let x = (area.x + area.width / 2. - bw / 2.).clamp(display.frame.x + 8., display.frame.x + display.frame.width - bw - 8.);
    let below = area.y + area.height + g + 10.;
    let above = area.y - g - 10. - bh;
    let y = if below + bh < display.frame.y + display.frame.height - 8. {
        below
    } else if above > display.frame.y + 30. {
        above
    } else {
        area.y + area.height - bh - 16.
    };
    let pixels = ((area.width * f64::from(scale)).round() as u32, (area.height * f64::from(scale)).round() as u32);
    let bar = cx.open_window(
        popup(display, Bounds::new(local(x, y), size(px(BAR.0), px(BAR.1))), true, WindowBackgroundAppearance::Transparent),
        |window, cx| {
            let view = cx.new(|cx| RecordBar { phase: Phase::Ready, mic, pixels, focus: cx.focus_handle() });
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
            view
        },
    )?;
    bar.update(cx, |_, window, _| crate::native::make_borderless(window))?;
    Ok(Windows { bar, frame })
}

impl Windows {
    pub fn set_phase(&self, phase: Phase, cx: &mut App) {
        let _ = self.bar.update(cx, |bar, _, cx| {
            bar.phase = phase;
            cx.notify();
        });
        let recording = matches!(phase, Phase::Recording(_));
        let _ = self.frame.update(cx, |frame, _, cx| {
            frame.recording = recording;
            cx.notify();
        });
    }

    pub fn set_mic(&self, mic: bool, cx: &mut App) {
        let _ = self.bar.update(cx, |bar, _, cx| {
            bar.mic = mic;
            cx.notify();
        });
    }

    /// Repaints the running clock until the bar is gone.
    pub fn tick(&self, cx: &mut App) {
        let bar = self.bar;
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let alive = bar.update(cx, |bar, _, cx| {
                    cx.notify();
                    matches!(bar.phase, Phase::Recording(_))
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn close(&self, cx: &mut App) {
        let _ = self.bar.update(cx, |_, window, _| window.remove_window());
        let _ = self.frame.update(cx, |_, window, _| window.remove_window());
    }
}
