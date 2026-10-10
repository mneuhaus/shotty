//! The Shottr-style editor that opens after a screenshot.
//!
//! The capture is already saved and on the clipboard when this opens; annotating is optional.
//! Shapes live in image pixels (`shotty_annotate`); the canvas draws them live, export renders
//! them with tiny-skia at full resolution.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme, Icon, Selectable as _, Sizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use image::RgbaImage;
use shotty_annotate::{self as ann, Area, Color, Document, PALETTE, Pt, Shape, pt};

gpui_kit::actions!(editor, [CopyImage, SaveImage, Undo, Redo, CloseWindow, Upload, CopyColor]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", CopyImage, Some("Editor")),
        KeyBinding::new("cmd-s", SaveImage, Some("Editor")),
        KeyBinding::new("cmd-z", Undo, Some("Editor")),
        KeyBinding::new("cmd-shift-z", Redo, Some("Editor")),
        KeyBinding::new("cmd-w", CloseWindow, Some("Editor")),
        KeyBinding::new("cmd-u", Upload, Some("Editor")),
        // Shadows gpui-kit's focus-next binding on Tab.
        KeyBinding::new("tab", CopyColor, Some("Editor")),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Select,
    Arrow,
    Line,
    Rect,
    Ellipse,
    Highlight,
    Text,
    Step,
    Pixelate,
    Crop,
}

impl Tool {
    const ALL: [Tool; 10] = [
        Tool::Select,
        Tool::Arrow,
        Tool::Line,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Highlight,
        Tool::Text,
        Tool::Step,
        Tool::Pixelate,
        Tool::Crop,
    ];

    fn key(self) -> &'static str {
        match self {
            Tool::Select => "v",
            Tool::Arrow => "a",
            Tool::Line => "l",
            Tool::Rect => "r",
            Tool::Ellipse => "o",
            Tool::Highlight => "h",
            Tool::Text => "t",
            Tool::Step => "n",
            Tool::Pixelate => "b",
            Tool::Crop => "c",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select & move",
            Tool::Arrow => "Arrow",
            Tool::Line => "Line",
            Tool::Rect => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Highlight => "Highlighter",
            Tool::Text => "Text",
            Tool::Step => "Numbered step",
            Tool::Pixelate => "Pixelate",
            Tool::Crop => "Crop",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Tool::Select => IconName::MousePointer2.into(),
            Tool::Arrow => IconName::MoveUpRight.into(),
            Tool::Line => IconName::Minus.into(),
            Tool::Rect => IconName::Square.into(),
            Tool::Ellipse => IconName::Circle.into(),
            Tool::Highlight => IconName::Highlighter.into(),
            Tool::Text => IconName::Type.into(),
            Tool::Step => IconName::ListOrdered.into(),
            Tool::Pixelate => IconName::Grid3x3.into(),
            Tool::Crop => IconName::Crop.into(),
        }
    }
}

/// Stroke presets in points; multiplied by the capture's pixel density.
const WIDTHS: [(f32, f32, f32); 3] = [(2.0, 14.0, 10.0), (3.5, 18.0, 13.0), (6.0, 26.0, 17.0)];

enum Drag {
    Draw { from: Pt, to: Pt },
    Move { index: usize, last: Pt },
    Crop { from: Pt, to: Pt },
}

pub struct Editor {
    base: Arc<RgbaImage>,
    picture: Arc<RenderImage>,
    path: PathBuf,
    /// Pixels per point of the capture (2 on Retina).
    density: f32,
    doc: Document,
    undo: Vec<Document>,
    redo: Vec<Document>,
    tool: Tool,
    color: Color,
    width: usize,
    drag: Option<Drag>,
    selected: Option<usize>,
    /// The label being typed: index into `doc.shapes`.
    typing: Option<usize>,
    patches: HashMap<[i32; 4], Arc<RenderImage>>,
    focus: FocusHandle,
    /// A short message over the picture (see `flash`).
    status: Option<SharedString>,
    status_id: u64,
    /// The color under the cursor, shown in the toolbar; Tab copies it.
    hover: Option<[u8; 3]>,
    discard_armed: bool,
    /// Image → window mapping from the last layout: origin and points per image pixel.
    view: (Point<Pixels>, f32),
}

const TOOLBAR: f32 = 52.;

impl Editor {
    pub fn new(base: Arc<RgbaImage>, path: PathBuf, density: f32, cx: &mut Context<Self>) -> Self {
        let picture = crate::output::render_image(&base);
        let demo = std::env::var("SHOTTY_DEMO").unwrap_or_default();
        let doc = match demo.as_str() {
            "screen-shapes" => demo_doc(density),
            "editor" | "editor-style" => sample_doc(),
            _ => Document::default(),
        };
        Self {
            base,
            picture,
            path,
            density,
            doc,
            undo: Vec::new(),
            redo: Vec::new(),
            tool: Tool::Arrow,
            color: PALETTE[0],
            width: 1,
            drag: None,
            selected: None,
            typing: None,
            patches: HashMap::new(),
            focus: cx.focus_handle(),
            status: (demo == "editor").then(|| "Copied #0A84FF".into()),
            status_id: 0,
            hover: demo.starts_with("editor").then_some([0x0a, 0x84, 0xff]),
            discard_armed: false,
            view: (point(px(0.), px(0.)), 1.),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn dirty(&self) -> bool {
        !self.doc.shapes.is_empty() || self.doc.crop.is_some()
    }

    fn stroke(&self) -> f32 {
        WIDTHS[self.width].0 * self.density
    }

    fn text_size(&self) -> f32 {
        WIDTHS[self.width].1 * self.density
    }

    fn step_radius(&self) -> f32 {
        WIDTHS[self.width].2 * self.density
    }

    /// Shows `text` over the picture; it goes away after a few seconds unless replaced.
    fn flash(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let id = self.show_status(text, cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| {
                if this.status_id == id {
                    this.status = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Shows `text` over the picture until the next message.
    fn show_status(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) -> u64 {
        self.status = Some(text.into());
        self.status_id += 1;
        cx.notify();
        self.status_id
    }

    fn checkpoint(&mut self) {
        self.undo.push(self.doc.clone());
        self.redo.clear();
        self.discard_armed = false;
    }

    fn to_image(&self, p: Point<Pixels>) -> Pt {
        let (origin, k) = self.view;
        pt((p.x - origin.x).as_f32() / k, (p.y - origin.y).as_f32() / k)
    }

    fn to_view(&self, p: Pt) -> Point<Pixels> {
        let (origin, k) = self.view;
        point(origin.x + px(p.x * k), origin.y + px(p.y * k))
    }

    fn area_to_view(&self, a: Area) -> Bounds<Pixels> {
        let k = self.view.1;
        Bounds::new(self.to_view(pt(a.x, a.y)), size(px(a.w * k), px(a.h * k)))
    }

    /// The shape a drawing drag would produce right now.
    fn shape_for(&self, from: Pt, to: Pt) -> Option<Shape> {
        let (color, width) = (self.color, self.stroke());
        let area = Area::spanning(from, to);
        Some(match self.tool {
            Tool::Arrow => Shape::Arrow { from, to, color, width },
            Tool::Line => Shape::Line { from, to, color, width },
            Tool::Rect => Shape::Rect { area, color, width },
            Tool::Ellipse => Shape::Ellipse { area, color, width },
            Tool::Highlight => Shape::Highlight { area, color },
            Tool::Pixelate => Shape::Pixelate { area },
            _ => return None,
        })
    }

    fn next_step(&self) -> u32 {
        self.doc.shapes.iter().filter_map(|s| if let Shape::Step { number, .. } = s { Some(*number) } else { None }).max().unwrap_or(0)
            + 1
    }

    fn finish_typing(&mut self) {
        if let Some(i) = self.typing.take()
            && matches!(self.doc.shapes.get(i), Some(Shape::Text { text, .. }) if text.trim().is_empty())
        {
            self.doc.shapes.remove(i);
        }
    }

    fn set_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        self.finish_typing();
        self.tool = tool;
        self.selected = None;
        cx.notify();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.finish_typing();
        let p = self.to_image(event.position);
        let slop = 6. / self.view.1;
        match self.tool {
            Tool::Select => {
                self.selected = self.doc.shapes.iter().rposition(|s| s.hit(p, slop));
                if let Some(index) = self.selected {
                    self.checkpoint();
                    self.drag = Some(Drag::Move { index, last: p });
                }
            }
            Tool::Text => {
                self.checkpoint();
                let size = self.text_size();
                let at = pt(p.x, p.y - size * 0.7);
                self.doc.shapes.push(Shape::Text { at, text: String::new(), color: self.color, size });
                self.typing = Some(self.doc.shapes.len() - 1);
            }
            Tool::Step => {
                self.checkpoint();
                let number = self.next_step();
                self.doc.shapes.push(Shape::Step { center: p, number, color: self.color, radius: self.step_radius() });
            }
            Tool::Crop => self.drag = Some(Drag::Crop { from: p, to: p }),
            _ => self.drag = Some(Drag::Draw { from: p, to: p }),
        }
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let p = self.to_image(event.position);
        let hover = (p.x >= 0. && p.y >= 0.)
            .then(|| self.base.get_pixel_checked(p.x as u32, p.y as u32))
            .flatten()
            .map(|px| [px[0], px[1], px[2]]);
        if hover.is_some() && hover != self.hover {
            self.hover = hover;
            cx.notify();
        }
        match &mut self.drag {
            Some(Drag::Draw { to, .. } | Drag::Crop { to, .. }) => *to = p,
            Some(Drag::Move { index, last }) => {
                let (dx, dy) = (p.x - last.x, p.y - last.y);
                *last = p;
                let index = *index;
                if let Some(shape) = self.doc.shapes.get_mut(index) {
                    shape.translate(dx, dy);
                }
            }
            None => return,
        }
        cx.notify();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        match self.drag.take() {
            Some(Drag::Draw { from, to }) => {
                let min = 4. / self.view.1;
                let big_enough = (to.x - from.x).abs() > min || (to.y - from.y).abs() > min;
                if big_enough && let Some(shape) = self.shape_for(from, to) {
                    self.checkpoint();
                    self.doc.shapes.push(shape);
                }
            }
            Some(Drag::Crop { from, to }) => {
                let area = Area::spanning(from, to);
                self.checkpoint();
                self.doc.crop = (area.w * self.view.1 > 8. && area.h * self.view.1 > 8.)
                    .then(|| clamp_area(area, self.base.width(), self.base.height()));
            }
            Some(Drag::Move { .. }) | None => {}
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let plain = !ks.modifiers.platform && !ks.modifiers.control && !ks.modifiers.alt;
        if let Some(i) = self.typing {
            if let Some(Shape::Text { text, .. }) = self.doc.shapes.get_mut(i) {
                match ks.key.as_str() {
                    "escape" => self.finish_typing(),
                    "enter" if ks.modifiers.shift => text.push('\n'),
                    "enter" => self.finish_typing(),
                    "backspace" => {
                        text.pop();
                    }
                    _ if plain => {
                        if let Some(c) = &ks.key_char {
                            text.push_str(c);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        match ks.key.as_str() {
            "escape" => {
                if self.drag.take().is_some() || self.selected.take().is_some() {
                } else if self.tool == Tool::Crop && self.doc.crop.is_some() {
                    self.checkpoint();
                    self.doc.crop = None;
                } else if self.dirty() && !self.discard_armed {
                    self.discard_armed = true;
                    self.flash("Unsaved annotations: press Esc again to discard", cx);
                } else {
                    window.remove_window();
                    return;
                }
            }
            "backspace" | "delete" if plain => {
                if let Some(i) = self.selected.take() {
                    self.checkpoint();
                    self.doc.shapes.remove(i);
                }
            }
            "enter" if self.tool == Tool::Crop => self.set_tool(Tool::Arrow, cx),
            key if plain && !ks.modifiers.shift => {
                if let Some(tool) = Tool::ALL.into_iter().find(|t| t.key() == key) {
                    self.set_tool(tool, cx);
                } else if let Some(n) = key.parse::<usize>().ok().filter(|n| (1..=PALETTE.len()).contains(n)) {
                    self.color = PALETTE[n - 1];
                } else {
                    return;
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Copies the color under the cursor as "#2A4A66".
    fn copy_color(&mut self, _: &CopyColor, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(hex) = self.hover.map(hex) {
            cx.write_to_clipboard(ClipboardItem::new_string(hex.clone()));
            self.flash(format!("Copied {hex}"), cx);
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.typing = None;
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.doc, prev));
            self.selected = None;
            cx.notify();
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.doc, next));
            self.selected = None;
            cx.notify();
        }
    }

    /// Renders the annotated picture off the main thread, then hands the PNG to `then`.
    fn export(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Vec<u8>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.finish_typing();
        let (base, doc) = (self.base.clone(), self.doc.clone());
        let task = cx.background_executor().spawn(async move { crate::output::encode_png(&ann::render(&base, &doc)) });
        cx.spawn_in(window, async move |this, cx| {
            let png = task.await;
            let _ = this.update_in(cx, |this, window, cx| match png {
                Ok(png) => then(this, png, window, cx),
                Err(e) => this.flash(format!("Export failed: {e:#}"), cx),
            });
        })
        .detach();
    }

    /// Copies the picture as image and as file, so Finder pastes a file. The capture file is updated
    /// first, so that file shows the annotations too.
    fn copy(&mut self, _: &CopyImage, window: &mut Window, cx: &mut Context<Self>) {
        self.export(window, cx, |this, png, window, cx| {
            if let Err(e) = std::fs::write(&this.path, &png) {
                return this.flash(format!("Saving failed: {e}"), cx);
            }
            crate::native::copy_image_file(&this.path, &png);
            window.remove_window();
        });
    }

    fn save(&mut self, _: &SaveImage, window: &mut Window, cx: &mut Context<Self>) {
        self.export(window, cx, |this, png, _, cx| {
            match std::fs::write(&this.path, &png) {
                Ok(()) => this.flash(format!("Saved to {}", this.path.display()), cx),
                Err(e) => this.flash(format!("Saving failed: {e}"), cx),
            }
            this.discard_armed = false;
            this.undo.clear();
        });
    }

    fn close(&mut self, _: &CloseWindow, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }

    /// Saves the annotated picture over the capture file, uploads it and puts the link on the clipboard.
    fn upload(&mut self, _: &Upload, window: &mut Window, cx: &mut Context<Self>) {
        self.export(window, cx, |this, png, window, cx| {
            if let Err(e) = std::fs::write(&this.path, &png) {
                return this.flash(format!("Saving failed: {e}"), cx);
            }
            this.discard_armed = false;
            let (task, progress) = crate::share::upload(this.path.clone(), cx);
            this.show_status("Uploading…", cx);
            cx.spawn_in(window, async move |this, cx| {
                let mut task = std::pin::pin!(task);
                // Repaint the percentage while the upload runs.
                let result = loop {
                    let tick = cx.background_executor().timer(std::time::Duration::from_millis(200));
                    match futures::future::select(task.as_mut(), tick).await {
                        futures::future::Either::Left((result, _)) => break result,
                        futures::future::Either::Right(_) => {
                            let pct = (crate::share::fraction(&progress) * 100.).round();
                            let _ = this.update(cx, |this, cx| {
                                this.show_status(format!("Uploading… {pct}%"), cx);
                            });
                        }
                    }
                };
                let _ = this.update(cx, |this, cx| match result {
                    Ok(up) => this.flash(format!("Link copied: {}", up.url), cx),
                    Err(e) => this.flash(format!("Upload failed: {e:#}"), cx),
                });
            })
            .detach();
        });
    }

    fn patch(&mut self, area: Area) -> Option<Arc<RenderImage>> {
        let key = [area.x as i32, area.y as i32, area.w as i32, area.h as i32];
        if let Some(p) = self.patches.get(&key) {
            return Some(p.clone());
        }
        let (_, _, patch) = ann::pixelate(&self.base, area)?;
        let image = crate::output::render_image(&patch);
        if self.patches.len() > 64 {
            self.patches.clear();
        }
        self.patches.insert(key, image.clone());
        Some(image)
    }

    /// Shottr's layout: actions, tools and style on the left, readouts on the right; the empty
    /// part drags the window.
    fn toolbar(&self, width: f32, zoom: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, border) = (theme.foreground, theme.muted_foreground, theme.title_bar_border);
        let action = |id: &'static str, icon: IconName, tip: &'static str| Button::new(id).icon(icon).ghost().small().tooltip(tip);
        let actions = h_flex()
            .gap(px(2.))
            .child(action("copy", IconName::Copy, "Copy and close  ⌘C").on_click(cx.listener(|this, _, window, cx| this.copy(&CopyImage, window, cx))))
            .child(action("save", IconName::Save, "Save  ⌘S").on_click(cx.listener(|this, _, window, cx| this.save(&SaveImage, window, cx))))
            .child(
                action("upload", IconName::CloudUpload, "Upload and copy link  ⌘U")
                    .on_click(cx.listener(|this, _, window, cx| this.upload(&Upload, window, cx))),
            );
        let mut tools = h_flex().gap(px(2.));
        for tool in Tool::ALL {
            tools = tools.child(
                Button::new(SharedString::from(format!("tool-{:?}", tool)))
                    .icon(tool.icon())
                    .ghost()
                    .small()
                    .selected(self.tool == tool)
                    .tooltip(format!("{}  ({})", tool.label(), tool.key().to_uppercase()))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tool(tool, cx))),
            );
        }
        let divider = || div().w(px(1.)).h(px(20.)).mx(px(6.)).bg(border);

        // Readouts, dropped from the left as the window gets narrow.
        let size = {
            let (w, h) = self.doc.crop.map_or((self.base.width() as f32, self.base.height() as f32), |c| (c.w.round(), c.h.round()));
            stat(format!("{w} × {h} px"), "Image size", fg, muted)
        };
        let color = self.hover.map(|c| {
            h_flex()
                .gap(px(8.))
                .child(div().size(px(18.)).rounded_full().bg(rgb(u32::from_be_bytes([0, c[0], c[1], c[2]]))).border_1().border_color(border))
                .child(stat(hex(c), "Tab to copy", fg, muted))
        });
        let mut readouts = h_flex().gap(px(14.)).pr(px(14.));
        if width > 940. && let Some(color) = color {
            readouts = readouts.child(color).child(divider());
        }
        if width > 820. {
            readouts = readouts.child(size).child(divider());
        }
        if width > 720. {
            readouts = readouts.child(stat(format!("{}%", (zoom * 100.).round()), "Zoom", fg, muted));
        }

        h_flex()
            .id("toolbar")
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h(px(TOOLBAR))
            // The stage lies underneath; it must not see clicks on the toolbar.
            .occlude()
            .pl(px(82.))
            .border_b_1()
            .border_color(border)
            .bg(theme.title_bar)
            .child(actions)
            .child(divider())
            .child(tools)
            .child(divider())
            .child(self.style_picker(cx))
            .child(div().id("drag").flex_1().h_full().on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move()))
            .child(readouts)
    }

    /// Current color and stroke; opens the palette and the three sizes.
    fn style_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = cx.entity().downgrade();
        let ring = cx.theme().foreground;
        let swatch = |color: Color, size: f32| div().size(px(size)).rounded_full().bg(rgb(color.to_hex()));
        Popover::new("style")
            .anchor(Anchor::TopLeft)
            .default_open(std::env::var("SHOTTY_DEMO").as_deref() == Ok("editor-style"))
            .trigger(
                Button::new("style-button")
                    .ghost()
                    .small()
                    .tooltip("Color and size  (1–7)")
                    .child(swatch(self.color, 16.).border_1().border_color(ring.opacity(0.25))),
            )
            .content(move |_, _, cx| {
                let Some(editor) = editor.upgrade() else { return v_flex() };
                let (current, current_width) = {
                    let e = editor.read(cx);
                    (e.color, e.width)
                };
                let mut colors = h_flex().gap(px(8.));
                for (i, color) in PALETTE.into_iter().enumerate() {
                    let editor = editor.clone();
                    colors = colors.child(
                        swatch(color, 22.)
                            .id(("color", i))
                            .border_2()
                            .border_color(if color == current { ring } else { ring.opacity(0.15) })
                            .cursor_pointer()
                            .on_click(move |_, window, cx| {
                                editor.update(cx, |e, cx| {
                                    e.color = color;
                                    cx.notify();
                                });
                                window.refresh();
                            }),
                    );
                }
                let mut widths = h_flex().gap(px(4.));
                for (i, (w, _, _)) in WIDTHS.into_iter().enumerate() {
                    let editor = editor.clone();
                    widths = widths.child(
                        div()
                            .id(("width", i))
                            .w(px(40.))
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .when(current_width == i, |d| d.bg(ring.opacity(0.12)))
                            .cursor_pointer()
                            .on_click(move |_, window, cx| {
                                editor.update(cx, |e, cx| {
                                    e.width = i;
                                    cx.notify();
                                });
                                window.refresh();
                            })
                            .child(div().w(px(22.)).h(px(w)).rounded_full().bg(rgb(current.to_hex()))),
                    );
                }
                v_flex().gap(px(10.)).p(px(4.)).child(colors).child(widths)
            })
    }
    /// Text labels and step badges are real text, laid out by GPUI with the export font.
    fn label_element(&self, shape: &Shape, caret: bool) -> Option<AnyElement> {
        let k = self.view.1;
        match shape {
            Shape::Text { at, text, color, size } => {
                let (padx, pady) = ann::pill_padding(*size);
                let shown = if caret { format!("{text}▏") } else { text.clone() };
                let p = self.to_view(*at);
                Some(
                    div()
                        .absolute()
                        .left(p.x)
                        .top(p.y)
                        .px(px(padx * k))
                        .py(px(pady * k))
                        .rounded(px(size * 0.32 * k))
                        .bg(rgb(color.to_hex()))
                        .text_color(rgb(color.contrast().to_hex()))
                        .font_family(ann::FONT_FAMILY)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(size * k))
                        .line_height(px(size * 1.22 * k))
                        .whitespace_nowrap()
                        .children(shown.split('\n').map(|l| div().child(l.to_string())).collect::<Vec<_>>())
                        .into_any_element(),
                )
            }
            Shape::Step { center, number, color, radius } => {
                let r = radius * k;
                let c = self.to_view(*center);
                Some(
                    div()
                        .absolute()
                        .left(c.x - px(r))
                        .top(c.y - px(r))
                        .size(px(r * 2.))
                        .rounded_full()
                        .bg(rgb(color.to_hex()))
                        .border(px(radius * 0.14 * k))
                        .border_color(white())
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(rgb(color.contrast().to_hex()))
                        .font_family(ann::FONT_FAMILY)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(ann::step_font_size(*radius) * k))
                        .child(number.to_string())
                        .into_any_element(),
                )
            }
            _ => None,
        }
    }
}

/// "#2A4A66"
fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// A value over a small caption, like Shottr's toolbar readouts.
fn stat(value: impl Into<SharedString>, caption: &'static str, fg: Hsla, muted: Hsla) -> Div {
    v_flex()
        .child(div().text_size(px(12.)).line_height(px(15.)).text_color(fg).whitespace_nowrap().child(value.into()))
        .child(div().text_size(px(10.)).line_height(px(13.)).text_color(muted).whitespace_nowrap().child(caption))
}

/// The annotations of the website example, on `docs/assets/sample-before.png`.
fn sample_doc() -> Document {
    let [red, _, yellow, _, blue, ..] = PALETTE;
    Document {
        shapes: vec![
            Shape::Pixelate { area: Area { x: 838., y: 263., w: 292., h: 43. } },
            Shape::Highlight { area: Area { x: 378., y: 443., w: 372., h: 24. }, color: yellow },
            Shape::Text { at: pt(772., 421.), text: "Turn this on".into(), color: red, size: 26. },
            Shape::Arrow { from: pt(924., 442.), to: pt(1072., 442.), color: red, width: 6. },
            Shape::Rect { area: Area { x: 374., y: 577., w: 165., h: 63. }, color: blue, width: 5. },
            Shape::Step { center: pt(1138., 420.), number: 1, color: red, radius: 17. },
            Shape::Step { center: pt(548., 579.), number: 2, color: blue, radius: 17. },
        ],
        crop: None,
    }
}

/// A fixture with one of every shape, for screenshots of the editor.
fn demo_doc(d: f32) -> Document {
    let c = PALETTE;
    Document {
        shapes: vec![
            Shape::Pixelate { area: Area { x: 20. * d, y: 60. * d, w: 180. * d, h: 60. * d } },
            Shape::Arrow { from: pt(450. * d, 350. * d), to: pt(650. * d, 210. * d), color: c[0], width: 3.5 * d },
            Shape::Rect { area: Area { x: 660. * d, y: 150. * d, w: 300. * d, h: 110. * d }, color: c[0], width: 3.5 * d },
            Shape::Ellipse { area: Area { x: 100. * d, y: 400. * d, w: 250. * d, h: 130. * d }, color: c[4], width: 3.5 * d },
            Shape::Highlight { area: Area { x: 450. * d, y: 450. * d, w: 350. * d, h: 30. * d }, color: c[2] },
            Shape::Text { at: pt(500. * d, 280. * d), text: "Click here".into(), color: c[0], size: 18. * d },
            Shape::Step { center: pt(410. * d, 360. * d), number: 1, color: c[4], radius: 13. * d },
        ],
        crop: None,
    }
}

fn clamp_area(a: Area, w: u32, h: u32) -> Area {
    let x0 = a.x.clamp(0., w as f32);
    let y0 = a.y.clamp(0., h as f32);
    let x1 = (a.x + a.w).clamp(0., w as f32);
    let y1 = (a.y + a.h).clamp(0., h as f32);
    Area { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
}

fn hsla_of(c: Color, alpha: f32) -> Hsla {
    let mut h: Hsla = rgb(c.to_hex()).into();
    h.a = alpha;
    h
}

/// Paints strokes and fills for the geometric shapes; `k` maps image pixels to points.
fn paint_shapes(shapes: &[Shape], origin: Point<Pixels>, k: f32, window: &mut Window) {
    let at = |p: Pt| point(origin.x + px(p.x * k), origin.y + px(p.y * k));
    for shape in shapes {
        match shape {
            Shape::Arrow { from, to, color, width } => {
                let (shaft_end, head) = ann::arrow_head(*from, *to, *width);
                let mut b = PathBuilder::stroke(px(width * k));
                b.move_to(at(*from));
                b.line_to(at(shaft_end));
                if let Ok(path) = b.build() {
                    window.paint_path(path, hsla_of(*color, 1.));
                }
                let mut b = PathBuilder::fill();
                b.move_to(at(head[0]));
                b.line_to(at(head[1]));
                b.line_to(at(head[2]));
                b.close();
                if let Ok(path) = b.build() {
                    window.paint_path(path, hsla_of(*color, 1.));
                }
            }
            Shape::Line { from, to, color, width } => {
                let mut b = PathBuilder::stroke(px(width * k));
                b.move_to(at(*from));
                b.line_to(at(*to));
                if let Ok(path) = b.build() {
                    window.paint_path(path, hsla_of(*color, 1.));
                }
            }
            Shape::Rect { area, color, width } => {
                let outer = area.grow(width / 2.);
                let bounds = Bounds::new(at(pt(outer.x, outer.y)), size(px(outer.w * k), px(outer.h * k)));
                window.paint_quad(quad(
                    bounds,
                    px(width * 1.4 * k),
                    transparent_black(),
                    px(width * k),
                    hsla_of(*color, 1.),
                    BorderStyle::Solid,
                ));
            }
            Shape::Ellipse { area, color, width } => {
                let (start, curves) = ann::ellipse_curves(*area);
                let mut b = PathBuilder::stroke(px(width * k));
                b.move_to(at(start));
                for (c1, c2, end) in curves {
                    b.cubic_bezier_to(at(end), at(c1), at(c2));
                }
                b.close();
                if let Ok(path) = b.build() {
                    window.paint_path(path, hsla_of(*color, 1.));
                }
            }
            Shape::Highlight { area, color } => {
                let bounds = Bounds::new(at(pt(area.x, area.y)), size(px(area.w * k), px(area.h * k)));
                window.paint_quad(fill(bounds, hsla_of(*color, ann::HIGHLIGHT_ALPHA)));
            }
            Shape::Text { .. } | Shape::Step { .. } | Shape::Pixelate { .. } => {}
        }
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::env::var("SHOTTY_DEMO").as_deref() == Ok("screen-upload") && self.status.is_none() {
            self.status = Some("demo".into());
            window.dispatch_action(Box::new(Upload), cx);
        }
        // Fit the picture (at most 1:1 in points) edge to edge below the toolbar.
        let viewport = window.viewport_size();
        let (iw, ih) = (self.base.width() as f32, self.base.height() as f32);
        let avail = (viewport.width.as_f32(), viewport.height.as_f32() - TOOLBAR);
        let k = (avail.0 / iw).min(avail.1 / ih).min(1. / self.density).max(0.01);
        let (dw, dh) = (iw * k, ih * k);
        let origin = point(px((avail.0 - dw) / 2.), px(TOOLBAR + (avail.1 - dh) / 2.));
        self.view = (origin, k);

        let mut shapes = self.doc.shapes.clone();
        let mut crop = self.doc.crop;
        match &self.drag {
            Some(Drag::Draw { from, to }) => shapes.extend(self.shape_for(*from, *to)),
            Some(Drag::Crop { from, to }) => crop = Some(Area::spanning(*from, *to)),
            _ => {}
        }

        let mut stage = div()
            .id("stage")
            .absolute()
            .left_0()
            .top_0()
            .size_full()
            .cursor(if self.tool == Tool::Select { CursorStyle::Arrow } else { CursorStyle::Crosshair })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(
                img(ImageSource::Render(self.picture.clone()))
                    .absolute()
                    .left(origin.x)
                    .top(origin.y)
                    .w(px(dw))
                    .h(px(dh)),
            );

        for area in shapes.iter().filter_map(|s| if let Shape::Pixelate { area } = s { Some(*area) } else { None }) {
            let clamped = clamp_area(area, self.base.width(), self.base.height());
            if let Some(patch) = self.patch(clamped) {
                let b = self.area_to_view(clamped);
                stage = stage.child(img(ImageSource::Render(patch)).absolute().left(b.origin.x).top(b.origin.y).w(b.size.width).h(b.size.height));
            }
        }

        let painted = shapes.clone();
        stage = stage.child(
            canvas(|_, _, _| (), move |_, _, window, _| paint_shapes(&painted, origin, k, window)).absolute().size_full(),
        );
        for (i, shape) in shapes.iter().enumerate() {
            if let Some(label) = self.label_element(shape, self.typing == Some(i)) {
                stage = stage.child(label);
            }
        }

        if let Some(i) = self.selected
            && let Some(shape) = self.doc.shapes.get(i)
        {
            let b = self.area_to_view(shape.bounds()).dilate(px(4.));
            stage = stage.child(
                div().absolute().left(b.origin.x).top(b.origin.y).w(b.size.width).h(b.size.height).border_1().border_dashed().border_color(rgb(0x0a84ff)),
            );
        }

        if let Some(c) = crop {
            let c = clamp_area(c, self.base.width(), self.base.height());
            let b = self.area_to_view(c);
            let shade = black().opacity(if self.tool == Tool::Crop { 0.55 } else { 0.8 });
            let (l, t, r, btm) = (origin.x, origin.y, origin.x + px(dw), origin.y + px(dh));
            let (cl, ct, cr, cb) = (b.origin.x, b.origin.y, b.origin.x + b.size.width, b.origin.y + b.size.height);
            for (x0, y0, x1, y1) in [(l, t, r, ct), (l, cb, r, btm), (l, ct, cl, cb), (cr, ct, r, cb)] {
                if x1 > x0 && y1 > y0 {
                    stage = stage.child(div().absolute().left(x0).top(y0).w(x1 - x0).h(y1 - y0).bg(shade));
                }
            }
            stage = stage.child(div().absolute().left(cl).top(ct).w(b.size.width).h(b.size.height).border_1().border_color(white()));
        }

        if let Some(text) = self.status.clone() {
            stage = stage.child(
                div().absolute().bottom(px(16.)).left_0().w_full().flex().justify_center().child(
                    div()
                        .max_w(px(dw.max(240.) - 32.))
                        .px(px(12.))
                        .py(px(6.))
                        .rounded(px(8.))
                        .bg(black().opacity(0.78))
                        .text_color(white())
                        .text_size(px(12.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(text),
                ),
            );
        }

        // Stage and toolbar share window coordinates, as do mouse events and the shape canvas.
        let backdrop = cx.theme().muted;
        div()
            .key_context("Editor")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::upload))
            .on_action(cx.listener(Self::copy_color))
            .relative()
            .size_full()
            .bg(backdrop)
            .child(stage)
            .child(self.toolbar(viewport.width.as_f32(), k * self.density, cx))
    }
}

/// Opens the editor sized to the picture, at most ~80 % of the main display.
pub fn open(image: Arc<RgbaImage>, path: PathBuf, density: f32, cx: &mut App) {
    let display = cx.primary_display().map(|d| d.bounds().size).unwrap_or(size(px(1440.), px(900.)));
    let (w, h) = (image.width() as f32 / density, image.height() as f32 / density);
    let fit = (display.width.as_f32() * 0.8 / w).min((display.height.as_f32() * 0.8 - TOOLBAR) / h).min(1.);
    let win = size(px((w * fit).max(720.)), px((h * fit + TOOLBAR).max(320.)));
    let title = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let demo = std::env::var_os("SHOTTY_DEMO").is_some();
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, win, cx))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(18.), px(19.))),
            }),
            window_min_size: Some(size(px(620.), px(260.))),
            // Demo windows must not take the keyboard from whoever is working.
            focus: !demo,
            ..Default::default()
        },
        cx,
        |window, cx| {
            let view = cx.new(|cx| Editor::new(image, path, density, cx));
            let focus = view.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
            view
        },
    );
    if let Err(e) = opened {
        log::error!("could not open the editor: {e:#}");
    }
    if !demo {
        cx.activate(true);
    }
}
