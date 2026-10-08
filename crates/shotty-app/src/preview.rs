//! The window that opens after a screenshot: the picture, plus copy / reveal / close.
//! (Becomes the annotation editor.)

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::assets::IconName;
use gpui_kit::*;
use image::RgbaImage;

gpui_kit::actions!(preview, [CopyImage, CloseWindow]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", CopyImage, Some("Preview")),
        KeyBinding::new("escape", CloseWindow, Some("Preview")),
        KeyBinding::new("cmd-w", CloseWindow, Some("Preview")),
    ]);
}

pub struct Preview {
    image: Arc<RgbaImage>,
    picture: Arc<RenderImage>,
    png: Arc<Vec<u8>>,
    path: PathBuf,
    focus: FocusHandle,
}

impl Preview {
    pub fn new(image: Arc<RgbaImage>, png: Arc<Vec<u8>>, path: PathBuf, cx: &mut Context<Self>) -> Self {
        let picture = crate::output::render_image(&image);
        Self { image, picture, png, path, focus: cx.focus_handle() }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn copy(&mut self, _: &CopyImage, window: &mut Window, cx: &mut Context<Self>) {
        crate::output::copy_png(self.png.to_vec(), cx);
        window.remove_window();
    }

    fn close(&mut self, _: &CloseWindow, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }
}

/// Opens the preview sized to the picture (in points), at most ~75 % of the screen.
pub fn open(image: Arc<RgbaImage>, png: Arc<Vec<u8>>, path: PathBuf, scale: f32, cx: &mut App) {
    let display = cx.primary_display().map(|d| d.bounds().size).unwrap_or(size(px(1440.), px(900.)));
    let toolbar = 52.;
    let (w, h) = (image.width() as f32 / scale, image.height() as f32 / scale);
    let fit = (display.width.as_f32() * 0.75 / w).min((display.height.as_f32() * 0.75 - toolbar) / h).min(1.);
    let win = size(px((w * fit).max(420.)), px((h * fit).max(160.) + toolbar));
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, win, cx))),
            titlebar: Some(TitlebarOptions {
                title: Some(path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default().into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(14.))),
            }),
            window_min_size: Some(size(px(360.), px(200.))),
            ..Default::default()
        },
        cx,
        |window, cx| {
            let view = cx.new(|cx| Preview::new(image, png, path, cx));
            let focus = view.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
            view
        },
    );
    if let Err(e) = opened {
        eprintln!("shotty: could not open preview: {e:#}");
    }
    cx.activate(true);
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let path = self.path.clone();
        let dims = format!("{} × {}", self.image.width(), self.image.height());
        v_flex()
            .key_context("Preview")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::close))
            .size_full()
            .bg(theme.background)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p(px(12.))
                    .pt(px(34.))
                    .bg(rgb(0x1c1c1f))
                    .child(img(ImageSource::Render(self.picture.clone())).size_full().object_fit(ObjectFit::Contain)),
            )
            .child(
                h_flex()
                    .h(px(52.))
                    .px(px(12.))
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().text_sm().text_color(theme.muted_foreground).child(dims))
                    .child(div().flex_1())
                    .child(
                        Button::new("reveal").icon(IconName::FolderOpen).ghost().small().tooltip("Show in Finder").on_click(
                            move |_, _, _| {
                                let _ = std::process::Command::new("open").arg("-R").arg(&path).spawn();
                            },
                        ),
                    )
                    .child(
                        Button::new("copy")
                            .icon(IconName::Copy)
                            .label("Copy & Close")
                            .primary()
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.copy(&CopyImage, window, cx))),
                    ),
            )
    }
}
