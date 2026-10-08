//! Screen capture for Shotty on top of ScreenCaptureKit.
//!
//! Coordinates are global CoreGraphics points (origin top-left of the main display, y down),
//! the same space GPUI uses for display bounds. Images come back at full pixel resolution.

use anyhow::{Context, Result, anyhow};
use image::RgbaImage;
use screencapturekit::prelude::*;
use screencapturekit::screenshot_manager::{CGImageExt, SCScreenshotManager};
use screencapturekit::shareable_content::SCShareableContentInfo;

/// A rectangle in global points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    fn from_cg(r: CGRect) -> Self {
        Self { x: r.origin.x, y: r.origin.y, width: r.size.width, height: r.size.height }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

#[derive(Clone, Debug)]
pub struct DisplayInfo {
    pub id: u32,
    pub frame: Rect,
}

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub id: u32,
    pub frame: Rect,
    pub title: String,
    pub app: String,
}

/// A captured display: the image plus where it sits, so a selection in points maps onto pixels.
pub struct DisplayShot {
    pub display: DisplayInfo,
    pub image: RgbaImage,
}

impl DisplayShot {
    /// Pixels per point of this capture (2.0 on Retina).
    pub fn scale(&self) -> f64 {
        self.image.width() as f64 / self.display.frame.width
    }

    /// Crops a rectangle given in global points out of the frozen capture.
    pub fn crop(&self, area: Rect) -> RgbaImage {
        let s = self.scale();
        let f = self.display.frame;
        let x = ((area.x - f.x) * s).round().max(0.0) as u32;
        let y = ((area.y - f.y) * s).round().max(0.0) as u32;
        let w = ((area.width * s).round() as u32).min(self.image.width().saturating_sub(x)).max(1);
        let h = ((area.height * s).round() as u32).min(self.image.height().saturating_sub(y)).max(1);
        image::imageops::crop_imm(&self.image, x, y, w, h).to_image()
    }
}

fn content() -> Result<SCShareableContent> {
    SCShareableContent::get().map_err(|e| anyhow!("screen content unavailable (Screen Recording permission?): {e}"))
}

pub fn displays() -> Result<Vec<DisplayInfo>> {
    Ok(content()?.displays().iter().map(|d| DisplayInfo { id: d.display_id(), frame: Rect::from_cg(d.frame()) }).collect())
}

/// Normal app windows that are on screen, front to back, without Shotty's own.
pub fn windows() -> Result<Vec<WindowInfo>> {
    let own = std::process::id() as i32;
    Ok(content()?
        .windows()
        .iter()
        .filter(|w| w.is_on_screen() && w.window_layer() == 0)
        .filter_map(|w| {
            let app = w.owning_application()?;
            if app.process_id() == own {
                return None;
            }
            let frame = Rect::from_cg(w.frame());
            (frame.width >= 40.0 && frame.height >= 40.0).then(|| WindowInfo {
                id: w.window_id(),
                frame,
                title: w.title().unwrap_or_default(),
                app: app.application_name(),
            })
        })
        .collect())
}

fn to_rgba(image: &screencapturekit::CGImage) -> Result<RgbaImage> {
    let (w, h) = (image.width() as u32, image.height() as u32);
    let data = image.rgba_data().map_err(|e| anyhow!("reading pixels: {e}"))?;
    RgbaImage::from_raw(w, h, data).context("pixel buffer has the wrong size")
}

fn shoot(filter: &SCContentFilter, frame: Rect) -> Result<RgbaImage> {
    let scale = SCShareableContentInfo::for_filter(filter).map_or(2.0, |i| i.point_pixel_scale() as f64);
    let config = SCStreamConfiguration::new()
        .with_width((frame.width * scale).round() as u32)
        .with_height((frame.height * scale).round() as u32)
        .with_shows_cursor(false);
    let image = SCScreenshotManager::capture_image(filter, &config).map_err(|e| anyhow!("screenshot failed: {e}"))?;
    to_rgba(&image)
}

/// Captures every display at once, so the selection overlay can show a frozen frame.
pub fn capture_all_displays() -> Result<Vec<DisplayShot>> {
    let content = content()?;
    content
        .displays()
        .iter()
        .map(|d| {
            let filter = SCContentFilter::create().with_display(d).with_excluding_windows(&[]).build().map_err(|e| anyhow!("{e}"))?;
            let info = DisplayInfo { id: d.display_id(), frame: Rect::from_cg(d.frame()) };
            Ok(DisplayShot { image: shoot(&filter, info.frame)?, display: info })
        })
        .collect()
}

/// Captures a single window on its own (transparent corners, no desktop behind it).
pub fn capture_window(window_id: u32) -> Result<RgbaImage> {
    let content = content()?;
    let window = content.windows().into_iter().find(|w| w.window_id() == window_id).context("window is gone")?;
    let filter = SCContentFilter::create().with_window(&window).build().map_err(|e| anyhow!("{e}"))?;
    shoot(&filter, Rect::from_cg(window.frame()))
}
