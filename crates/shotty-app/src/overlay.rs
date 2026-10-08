//! The full-screen selection overlay, one per display.
//!
//! Screenshots use a frozen frame (captured before the overlay opens), so what you select is exactly
//! what you get and the overlay can never end up in the picture. Drag selects an area; a plain
//! click or Space switches to picking the window under the cursor; Esc cancels.

use std::sync::Arc;

use gpui_kit::*;
use shotty_capture::{DisplayShot, Rect, WindowInfo};

use crate::{Event, events};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Area,
    Window,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Cancel,
    /// A rectangle in global points on one display.
    Area { display_id: u32, rect: Rect },
    Window { id: u32 },
}

pub struct Overlay {
    shot: Arc<DisplayShot>,
    picture: Arc<RenderImage>,
    /// On-screen windows, front to back, in global points.
    windows: Arc<Vec<WindowInfo>>,
    focus: FocusHandle,
    mode: Mode,
    mouse: Option<Point<Pixels>>,
    drag_from: Option<Point<Pixels>>,
}

const DIM: f32 = 0.42;
const ACCENT: u32 = 0x3b82f6;

impl Overlay {
    pub fn new(shot: Arc<DisplayShot>, windows: Arc<Vec<WindowInfo>>, cx: &mut Context<Self>) -> Self {
        let picture = crate::output::render_image(&shot.image);
        let mut this = Self { shot, picture, windows, focus: cx.focus_handle(), mode: Mode::Area, mouse: None, drag_from: None };
        // Screenshot fixtures: a fixed drag, or window mode with the cursor parked mid-screen.
        match std::env::var("SHOTTY_DEMO").as_deref() {
            Ok("area-drag") => (this.drag_from, this.mouse) = (Some(point(px(420.), px(260.))), Some(point(px(1180.), px(720.)))),
            Ok("area-window") => (this.mode, this.mouse) = (Mode::Window, Some(point(px(900.), px(600.)))),
            Ok("area-cross") => this.mouse = Some(point(px(900.), px(600.))),
            _ => {}
        }
        this
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn origin(&self) -> (f64, f64) {
        (self.shot.display.frame.x, self.shot.display.frame.y)
    }

    fn to_global(&self, p: Point<Pixels>) -> (f64, f64) {
        let (ox, oy) = self.origin();
        (ox + f64::from(p.x.as_f32()), oy + f64::from(p.y.as_f32()))
    }

    fn to_local(&self, r: Rect) -> Bounds<Pixels> {
        let (ox, oy) = self.origin();
        Bounds::new(point(px((r.x - ox) as f32), px((r.y - oy) as f32)), size(px(r.width as f32), px(r.height as f32)))
    }

    fn window_under_mouse(&self) -> Option<&WindowInfo> {
        let (x, y) = self.to_global(self.mouse?);
        self.windows.iter().find(|w| w.frame.contains(x, y))
    }

    /// The current drag rectangle in local points.
    fn selection(&self) -> Option<Bounds<Pixels>> {
        let (a, b) = (self.drag_from?, self.mouse?);
        let origin = point(a.x.min(b.x), a.y.min(b.y));
        Some(Bounds::new(origin, size((a.x - b.x).abs(), (a.y - b.y).abs())))
    }

    fn finish(&mut self, outcome: Outcome) {
        events().try_send(Event::Overlay(outcome)).ok();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.finish(Outcome::Cancel),
            "space" => {
                self.mode = if self.mode == Mode::Area { Mode::Window } else { Mode::Area };
                self.drag_from = None;
                cx.notify();
            }
            _ => {}
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.mouse = Some(event.position);
        if self.mode == Mode::Area {
            self.drag_from = Some(event.position);
        }
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            window.activate_window();
            window.focus(&self.focus, cx);
        }
        self.mouse = Some(event.position);
        cx.notify();
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.mouse = Some(event.position);
        let selection = self.selection().filter(|b| b.size.width > px(3.) && b.size.height > px(3.));
        self.drag_from = None;
        match selection {
            Some(b) => {
                let (x, y) = self.to_global(b.origin);
                let rect = Rect { x, y, width: f64::from(b.size.width.as_f32()), height: f64::from(b.size.height.as_f32()) };
                self.finish(Outcome::Area { display_id: self.shot.display.id, rect });
            }
            // A click without dragging takes the window under the cursor, in either mode.
            None => match self.window_under_mouse() {
                Some(w) => {
                    let id = w.id;
                    self.finish(Outcome::Window { id });
                }
                None => {
                    let rect = self.shot.display.frame;
                    self.finish(Outcome::Area { display_id: self.shot.display.id, rect });
                }
            },
        }
    }
}

fn dim(b: Bounds<Pixels>) -> Div {
    div().absolute().left(b.origin.x).top(b.origin.y).w(b.size.width).h(b.size.height).bg(black().opacity(DIM))
}

fn frame(b: Bounds<Pixels>, fill: Hsla) -> Div {
    div()
        .absolute()
        .left(b.origin.x - px(1.))
        .top(b.origin.y - px(1.))
        .w(b.size.width + px(2.))
        .h(b.size.height + px(2.))
        .border_1()
        .border_color(white().opacity(0.95))
        .bg(fill)
}

fn label(text: String, at: Point<Pixels>) -> Div {
    div()
        .absolute()
        .left(at.x)
        .top(at.y)
        .px(px(7.))
        .py(px(3.))
        .rounded(px(6.))
        .bg(black().opacity(0.72))
        .text_color(white())
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .child(text)
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let full = Bounds::new(point(px(0.), px(0.)), viewport);
        let scale = self.shot.scale() as f32;

        let mut root = div()
            .id("overlay")
            .size_full()
            .relative()
            .overflow_hidden()
            .track_focus(&self.focus)
            .cursor(CursorStyle::Crosshair)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(img(ImageSource::Render(self.picture.clone())).absolute().left_0().top_0().size_full());

        let selection = self.selection();
        let highlight = (selection.is_none() && self.mode == Mode::Window)
            .then(|| self.window_under_mouse().map(|w| self.to_local(w.frame)))
            .flatten();

        if let Some(b) = selection.or(highlight) {
            // Darken everything around the selection.
            let (l, t, r, btm) = (b.origin.x, b.origin.y, b.origin.x + b.size.width, b.origin.y + b.size.height);
            root = root
                .child(dim(Bounds::new(point(px(0.), px(0.)), size(viewport.width, t.max(px(0.))))))
                .child(dim(Bounds::new(point(px(0.), btm), size(viewport.width, (viewport.height - btm).max(px(0.))))))
                .child(dim(Bounds::new(point(px(0.), t), size(l.max(px(0.)), b.size.height))))
                .child(dim(Bounds::new(point(r, t), size((viewport.width - r).max(px(0.)), b.size.height))))
                .child(frame(b, if highlight.is_some() { rgb(ACCENT).into() } else { transparent_black() }.opacity(0.18)));
            let text = format!("{} × {}", (b.size.width.as_f32() * scale).round(), (b.size.height.as_f32() * scale).round());
            let below = btm + px(8.);
            let at = if below + px(26.) < viewport.height { point(l, below) } else { point(l + px(6.), btm - px(30.)) };
            root = root.child(label(text, at));
        } else {
            root = root.child(dim(full));
        }

        // Crosshair and a hint while nothing is being dragged.
        if let (Some(m), None) = (self.mouse, self.drag_from)
            && self.mode == Mode::Area
        {
            let line = white().opacity(0.55);
            root = root
                .child(div().absolute().left(px(0.)).top(m.y).w(viewport.width).h(px(1.)).bg(line))
                .child(div().absolute().left(m.x).top(px(0.)).w(px(1.)).h(viewport.height).bg(line));
            let (gx, gy) = self.to_global(m);
            let (ox, oy) = self.origin();
            let text = format!("{} , {}", ((gx - ox) as f32 * scale).round(), ((gy - oy) as f32 * scale).round());
            root = root.child(label(text, point(m.x + px(14.), m.y + px(14.))));
        }

        let hint = match self.mode {
            Mode::Area => "Drag to select · Click for window · Space: window mode · Esc: cancel",
            Mode::Window => "Click a window · Space: area mode · Esc: cancel",
        };
        root.child(
            div()
                .absolute()
                .bottom(px(28.))
                .left_0()
                .w_full()
                .flex()
                .justify_center()
                .child(label(hint.into(), point(px(0.), px(0.))).relative()),
        )
    }
}
