//! The window after a recording: preview, trim, export as compact MP4 or GIF, copy, upload.
//!
//! The recording itself stays untouched; exports are written next to it.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use shotty_media::{Codec, CompressOptions, GifOptions, Probe, Trim};

gpui_kit::actions!(clip, [CloseClip]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-w", CloseClip, Some("Clip")), KeyBinding::new("escape", CloseClip, Some("Clip"))]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    /// H.264, plays everywhere.
    Compact,
    /// HEVC, about half the size; older Windows/Firefox setups may not play it.
    Smallest,
    Gif,
}

impl Format {
    fn label(self) -> &'static str {
        match self {
            Format::Compact => "MP4",
            Format::Smallest => "MP4 · HEVC",
            Format::Gif => "GIF",
        }
    }
}

/// Output widths in pixels. ~1200–1600 is the sweet spot for sharing (and keeps H.264 at level 5.0).
const WIDTHS: [Option<u32>; 4] = [None, Some(1600), Some(1200), Some(800)];
const GIF_FPS: [u32; 3] = [10, 15, 20];
const STRIP: usize = 10;

enum Job {
    Idle,
    Running { what: &'static str, progress: Arc<AtomicU32>, cancel: Arc<AtomicBool> },
}

struct Output {
    path: PathBuf,
    bytes: u64,
    link: Option<SharedString>,
}

pub struct Clip {
    path: PathBuf,
    probe: Option<Probe>,
    preview: Option<Arc<RenderImage>>,
    strip: Vec<Arc<RenderImage>>,
    trim: Trim,
    /// Which trim handle the mouse is dragging.
    dragging: Option<bool>,
    strip_bounds: Rc<Cell<Bounds<Pixels>>>,
    format: Format,
    width: Option<u32>,
    gif_fps: u32,
    job: Job,
    output: Option<Output>,
    status: Option<SharedString>,
    focus: FocusHandle,
    preview_request: u64,
}

fn mb(bytes: u64) -> String {
    if bytes >= 1_000_000 { format!("{:.1} MB", bytes as f64 / 1e6) } else { format!("{} KB", (bytes / 1000).max(1)) }
}

fn secs(t: f64) -> String {
    format!("{}:{:04.1}", (t / 60.).floor() as u32, t % 60.)
}

impl Clip {
    fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let this = Self {
            path: path.clone(),
            probe: None,
            preview: None,
            strip: Vec::new(),
            trim: Trim { start: 0., end: 0. },
            dragging: None,
            strip_bounds: Rc::new(Cell::new(Bounds::default())),
            format: Format::Compact,
            width: None,
            gif_fps: 15,
            job: Job::Idle,
            output: None,
            status: None,
            focus: cx.focus_handle(),
            preview_request: 0,
        };
        // Probe, then the thumbnail strip, off the main thread.
        let task = cx.background_executor().spawn(async move {
            let probe = shotty_media::probe(&path)?;
            let frames = (0..STRIP)
                .filter_map(|i| {
                    let t = probe.duration * (i as f64 + 0.5) / STRIP as f64;
                    shotty_media::frame(&path, t, 220).ok()
                })
                .map(|f| crate::output::render_image(&f))
                .collect::<Vec<_>>();
            let first = shotty_media::frame(&path, 0.05_f64.min(probe.duration / 2.), 1600).ok().map(|f| crate::output::render_image(&f));
            Ok::<_, anyhow::Error>((probe, frames, first))
        });
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            let _ = this.update(cx, |this, cx| {
                match loaded {
                    Ok((probe, strip, first)) => {
                        this.trim = Trim { start: 0., end: probe.duration };
                        // Big Retina recordings get scaled down for sharing by default.
                        this.width = if probe.width > 1600 { Some(1600) } else { None };
                        this.probe = Some(probe);
                        this.strip = strip;
                        this.preview = first;
                    }
                    Err(e) => this.status = Some(format!("Can't read the video: {e:#}").into()),
                }
                cx.notify();
            });
        })
        .detach();
        this
    }

    fn duration(&self) -> f64 {
        self.probe.as_ref().map_or(0., |p| p.duration)
    }

    fn trimmed(&self) -> Option<Trim> {
        let d = self.duration();
        (self.trim.start > 0.05 || self.trim.end < d - 0.05).then_some(self.trim)
    }

    /// Loads the frame at `t` into the preview, dropping stale requests while dragging.
    fn show_frame(&mut self, t: f64, cx: &mut Context<Self>) {
        self.preview_request += 1;
        let request = self.preview_request;
        let path = self.path.clone();
        let task = cx.background_executor().spawn(async move { shotty_media::frame(&path, t, 1600) });
        cx.spawn(async move |this, cx| {
            let frame = task.await;
            let _ = this.update(cx, |this, cx| {
                if let (Ok(f), true) = (frame, request == this.preview_request) {
                    this.preview = Some(crate::output::render_image(&f));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn time_at(&self, x: Pixels) -> f64 {
        let b = self.strip_bounds.get();
        let f = ((x - b.origin.x).as_f32() / b.size.width.as_f32().max(1.)).clamp(0., 1.);
        f64::from(f) * self.duration()
    }

    fn strip_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let t = self.time_at(event.position.x);
        // Grab whichever handle is closer.
        let start = (t - self.trim.start).abs() <= (t - self.trim.end).abs();
        self.dragging = Some(start);
        self.drag_to(t, cx);
    }

    fn strip_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.dragging.is_some() && event.pressed_button == Some(MouseButton::Left) {
            let t = self.time_at(event.position.x);
            self.drag_to(t, cx);
        }
    }

    fn drag_to(&mut self, t: f64, cx: &mut Context<Self>) {
        let min_len = 0.2;
        match self.dragging {
            Some(true) => self.trim.start = t.min(self.trim.end - min_len).max(0.),
            Some(false) => self.trim.end = t.max(self.trim.start + min_len).min(self.duration()),
            None => return,
        }
        let shown = if self.dragging == Some(true) { self.trim.start } else { (self.trim.end - 0.05).max(0.) };
        self.output = None;
        self.show_frame(shown, cx);
        cx.notify();
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        if matches!(self.job, Job::Running { .. }) || self.probe.is_none() {
            return;
        }
        let (format, width, fps, trim, input) = (self.format, self.width, self.gif_fps, self.trimmed(), self.path.clone());
        let stem = input.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Shotty".into());
        let dir = input.parent().map(Path::to_path_buf).unwrap_or_default();
        let output = match format {
            Format::Gif => dir.join(format!("{stem}.gif")),
            Format::Compact => dir.join(format!("{stem} compact.mp4")),
            Format::Smallest => dir.join(format!("{stem} hevc.mp4")),
        };
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p, c) = (progress.clone(), cancel.clone());
        let what = if format == Format::Gif { "Making GIF" } else { "Compressing" };
        self.job = Job::Running { what, progress, cancel };
        self.output = None;
        self.status = None;
        let out = output.clone();
        let task = cx.background_executor().spawn(async move {
            let mut report = |f: f32| p.store((f * 1000.) as u32, Ordering::Relaxed);
            match format {
                Format::Gif => shotty_media::gif(
                    &input,
                    &out,
                    &GifOptions { fps, width, trim, loop_forever: true },
                    &mut report,
                    &c,
                ),
                Format::Compact | Format::Smallest => shotty_media::compress(
                    &input,
                    &out,
                    &CompressOptions {
                        codec: if format == Format::Smallest { Codec::Hevc } else { Codec::H264 },
                        max_width: width,
                        trim,
                        ..CompressOptions::default()
                    },
                    &mut report,
                    &c,
                ),
            }?;
            Ok::<_, anyhow::Error>(std::fs::metadata(&out)?.len())
        });
        self.poll(cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.job = Job::Idle;
                match result {
                    Ok(bytes) => {
                        if std::env::var_os("SHOTTY_DEMO").is_none() {
                            crate::native::copy_files_to_clipboard(std::slice::from_ref(&output));
                        }
                        this.output = Some(Output { path: output, bytes, link: None });
                    }
                    Err(e) if shotty_media::is_cancelled(&e) => {}
                    Err(e) => this.status = Some(format!("Export failed: {e:#}").into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Repaints while a job runs, so the progress bar moves.
    fn poll(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let running = this.update(cx, |this, cx| {
                    cx.notify();
                    matches!(this.job, Job::Running { .. })
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn cancel_job(&mut self) {
        if let Job::Running { cancel, .. } = &self.job {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Uploads the export if there is one, else the recording itself.
    fn upload(&mut self, cx: &mut Context<Self>) {
        if matches!(self.job, Job::Running { .. }) {
            return;
        }
        let path = self.output.as_ref().map_or_else(|| self.path.clone(), |o| o.path.clone());
        let (task, progress) = crate::share::upload(path, cx);
        self.job = Job::Running { what: "Uploading", progress, cancel: Arc::new(AtomicBool::new(false)) };
        self.poll(cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.job = Job::Idle;
                match result {
                    Ok(up) => {
                        let link: SharedString = up.url.into();
                        match this.output.as_mut() {
                            Some(o) => o.link = Some(link),
                            None => this.status = Some(format!("Link copied: {link}").into()),
                        }
                    }
                    Err(e) => this.status = Some(format!("Upload failed: {e:#}").into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn close(&mut self, _: &CloseClip, window: &mut Window, _: &mut Context<Self>) {
        self.cancel_job();
        window.remove_window();
    }

    fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Button {
        Button::new(id).label(label).small().when(selected, |b| b.primary()).when(!selected, |b| b.ghost())
    }
}

impl Render for Clip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let d = self.duration().max(0.001);

        let preview = div()
            .flex_1()
            .min_h_0()
            .p(px(12.))
            .pt(px(36.))
            .bg(rgb(0x141416))
            .flex()
            .items_center()
            .justify_center()
            .child(match &self.preview {
                Some(p) => img(ImageSource::Render(p.clone())).size_full().object_fit(ObjectFit::Contain).into_any_element(),
                None => div().text_color(white().opacity(0.5)).child("Loading…").into_any_element(),
            });

        // Thumbnail strip with the kept range highlighted and two handles.
        let bounds_cell = self.strip_bounds.clone();
        let (s, e) = ((self.trim.start / d) as f32, (self.trim.end / d) as f32);
        let strip = div()
            .id("strip")
            .relative()
            .h(px(52.))
            .w_full()
            .rounded(px(8.))
            .overflow_hidden()
            .bg(rgb(0x2a2a2e))
            .cursor(CursorStyle::ResizeLeftRight)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::strip_down))
            .on_mouse_move(cx.listener(Self::strip_move))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.dragging = None))
            .child(canvas(move |b, _, _| bounds_cell.set(b), |_, _, _, _| {}).absolute().size_full())
            .child(h_flex().size_full().children(self.strip.iter().map(|f| {
                img(ImageSource::Render(f.clone())).flex_1().h_full().object_fit(ObjectFit::Cover)
            })))
            .child(div().absolute().top_0().left_0().h_full().w(relative(s)).bg(black().opacity(0.62)))
            .child(div().absolute().top_0().right_0().h_full().w(relative(1. - e)).bg(black().opacity(0.62)))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .h_full()
                    .left(relative(s))
                    .w(relative(e - s))
                    .border_2()
                    .border_color(rgb(0xffcc00))
                    .rounded(px(8.)),
            );

        let info = match &self.probe {
            Some(p) => format!(
                "{} × {} · {:.0} fps · {} → {} ({:.1} s) · {}",
                p.width,
                p.height,
                p.fps,
                secs(self.trim.start),
                secs(self.trim.end),
                self.trim.end - self.trim.start,
                mb(p.size_bytes)
            ),
            None => String::new(),
        };

        let running = matches!(self.job, Job::Running { .. });
        let mut formats = h_flex().gap(px(4.));
        for f in [Format::Compact, Format::Smallest, Format::Gif] {
            formats = formats.child(
                Self::chip(SharedString::from(format!("fmt-{f:?}")), f.label(), self.format == f).on_click(cx.listener(move |this, _, _, cx| {
                    this.format = f;
                    this.output = None;
                    // GIFs are ~8× larger than MP4; start them small.
                    if f == Format::Gif && this.width.is_none_or(|w| w > 1200) {
                        this.width = Some(800);
                    }
                    cx.notify();
                })),
            );
        }
        let mut widths = h_flex().gap(px(4.));
        for (i, w) in WIDTHS.into_iter().enumerate() {
            let label = w.map_or("Full size".to_string(), |w| format!("{w} px"));
            widths = widths.child(Self::chip(("w", i), label, self.width == w).on_click(cx.listener(move |this, _, _, cx| {
                this.width = w;
                this.output = None;
                cx.notify();
            })));
        }
        let mut fps = h_flex().gap(px(4.));
        if self.format == Format::Gif {
            for (i, f) in GIF_FPS.into_iter().enumerate() {
                fps = fps.child(Self::chip(("fps", i), format!("{f} fps"), self.gif_fps == f).on_click(cx.listener(move |this, _, _, cx| {
                    this.gif_fps = f;
                    this.output = None;
                    cx.notify();
                })));
            }
        }

        let bottom = match (&self.job, &self.output) {
            (Job::Running { what, progress, .. }, _) => {
                let f = progress.load(Ordering::Relaxed) as f32 / 1000.;
                h_flex()
                    .gap(px(10.))
                    .child(div().text_sm().child(format!("{what}… {}%", (f * 100.).round())))
                    .child(
                        div()
                            .flex_1()
                            .h(px(6.))
                            .rounded_full()
                            .bg(theme.secondary)
                            .child(div().h_full().w(relative(f)).rounded_full().bg(theme.primary)),
                    )
                    .child(Button::new("cancel-job").label("Cancel").ghost().small().on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_job();
                        cx.notify();
                    })))
                    .into_any_element()
            }
            (Job::Idle, Some(out)) => {
                let saved = self.probe.as_ref().map(|p| 100. - out.bytes as f64 * 100. / p.size_bytes.max(1) as f64);
                let path = out.path.clone();
                h_flex()
                    .gap(px(8.))
                    .child(
                        div().text_sm().flex_1().child(format!(
                            "{} · {}{}{}",
                            out.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                            mb(out.bytes),
                            saved.map_or(String::new(), |s| format!(" ({:+.0} %)", -s)),
                            out.link.as_ref().map_or(String::new(), |l| format!(" · link copied: {l}")),
                        )),
                    )
                    .child(Button::new("reveal-out").icon(IconName::FolderOpen).ghost().small().tooltip("Show in Finder").on_click(
                        move |_, _, _| {
                            let _ = std::process::Command::new("open").arg("-R").arg(&path).spawn();
                        },
                    ))
                    .into_any_element()
            }
            (Job::Idle, None) => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(self.status.clone().unwrap_or_else(|| {
                    if self.format == Format::Gif && self.trim.end - self.trim.start > 15. {
                        "Long GIFs get big (about 8× an MP4). Trim it, or export an MP4 instead.".into()
                    } else {
                        "Drag the yellow edges to trim. The recording itself stays as it is.".into()
                    }
                }))
                .into_any_element(),
        };

        let original = self.path.clone();
        v_flex()
            .key_context("Clip")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::close))
            .size_full()
            .bg(theme.background)
            .child(preview)
            .child(
                v_flex()
                    .flex_none()
                    .p(px(12.))
                    .gap(px(10.))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(strip)
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(div().text_xs().text_color(theme.muted_foreground).flex_1().child(info))
                            .child(Button::new("quicktime").icon(IconName::Play).label("Play").ghost().small().on_click(move |_, _, _| {
                                let _ = std::process::Command::new("open").arg("-a").arg("QuickTime Player").arg(&original).spawn();
                            })),
                    )
                    .child(
                        h_flex()
                            .gap(px(14.))
                            .flex_wrap()
                            .child(formats)
                            .child(widths)
                            .child(fps)
                            .child(div().flex_1())
                            .child(
                                Button::new("upload")
                                    .icon(IconName::CloudUpload)
                                    .label("Upload")
                                    .ghost()
                                    .small()
                                    .disabled(running)
                                    .tooltip("Upload the export (or the recording) and copy the link")
                                    .on_click(cx.listener(|this, _, _, cx| this.upload(cx))),
                            )
                            .child(
                                Button::new("export")
                                    .label(if self.format == Format::Gif { "Make GIF" } else { "Export MP4" })
                                    .primary()
                                    .small()
                                    .disabled(running || self.probe.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| this.export(cx))),
                            ),
                    )
                    .child(div().min_h(px(26.)).flex().items_center().child(bottom)),
            )
    }
}

pub fn open(path: PathBuf, cx: &mut App) {
    let title = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let bounds = Bounds::centered(None, size(px(860.), px(660.)), cx);
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(14.))),
            }),
            window_min_size: Some(size(px(640.), px(480.))),
            ..Default::default()
        },
        cx,
        |window, cx| {
            let view = cx.new(|cx| Clip::new(path, cx));
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
            view
        },
    );
    if let Err(e) = opened {
        log::error!("could not open the clip window: {e:#}");
    }
    cx.activate(true);
}
