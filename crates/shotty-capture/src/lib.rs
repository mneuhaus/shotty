//! Screen capture for Shotty on top of ScreenCaptureKit.
//!
//! Coordinates are global CoreGraphics points (origin top-left of the main display, y down),
//! the same space GPUI uses for display bounds. Images come back at full pixel resolution.
//!
//! The hotkey path stays clear of `SCShareableContent` (≈50 ms per call): displays and windows come
//! from CoreGraphics, and displays are frozen in parallel with `captureImageInRect`.

use std::sync::mpsc;

use anyhow::{Context, Result, anyhow, bail};
use image::RgbaImage;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::CFArray;
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode, CGGetActiveDisplayList, CGWindowListCopyWindowInfo,
    CGWindowListOption, kCGNullWindowID,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
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

    fn from_cf(r: objc2_core_foundation::CGRect) -> Self {
        Self { x: r.origin.x, y: r.origin.y, width: r.size.width, height: r.size.height }
    }

    fn to_cg(self) -> CGRect {
        CGRect::new(self.x, self.y, self.width, self.height)
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

/// A captured display: the pixels plus where it sits, so a selection in points maps onto pixels.
pub struct DisplayShot {
    pub display: DisplayInfo,
    /// The capture in BGRA byte order (what the GPU wants), despite the `RgbaImage` type.
    pub bgra: RgbaImage,
}

impl DisplayShot {
    /// Pixels per point of this capture (2.0 on Retina).
    pub fn scale(&self) -> f64 {
        self.bgra.width() as f64 / self.display.frame.width
    }

    /// Crops a rectangle given in global points out of the frozen capture, as RGBA.
    pub fn crop(&self, area: Rect) -> RgbaImage {
        let s = self.scale();
        let f = self.display.frame;
        let x = ((area.x - f.x) * s).round().max(0.0) as u32;
        let y = ((area.y - f.y) * s).round().max(0.0) as u32;
        let w = ((area.width * s).round() as u32).min(self.bgra.width().saturating_sub(x)).max(1);
        let h = ((area.height * s).round() as u32).min(self.bgra.height().saturating_sub(y)).max(1);
        let mut out = image::imageops::crop_imm(&self.bgra, x, y, w, h).to_image();
        swap_red_blue(&mut out);
        out
    }

    /// The whole display as RGBA.
    pub fn to_rgba(&self) -> RgbaImage {
        let mut out = self.bgra.clone();
        swap_red_blue(&mut out);
        out
    }
}

/// BGRA ↔ RGBA in place.
pub fn swap_red_blue(image: &mut RgbaImage) {
    for px in image.pixels_mut() {
        px.0.swap(0, 2);
    }
}

fn content() -> Result<SCShareableContent> {
    SCShareableContent::get().map_err(|e| anyhow!("screen content unavailable (Screen Recording permission?): {e}"))
}

/// The active displays, from CoreGraphics (microseconds, unlike `SCShareableContent`).
pub fn displays() -> Result<Vec<DisplayInfo>> {
    let mut ids = [0u32; 32];
    let mut count = 0u32;
    let err = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if err.0 != 0 {
        bail!("listing displays failed (CGError {})", err.0);
    }
    Ok(ids[..count as usize].iter().map(|&id| DisplayInfo { id, frame: Rect::from_cf(CGDisplayBounds(id)) }).collect())
}

/// Pixels per point of a display: its current mode's pixel width over its width in points.
pub fn scale_of(display_id: u32) -> Result<f64> {
    let mode = CGDisplayCopyDisplayMode(display_id).context("display is gone")?;
    let points = CGDisplayBounds(display_id).size.width;
    let pixels = CGDisplayMode::pixel_width(Some(&mode)) as f64;
    Ok(if points > 0.0 && pixels > 0.0 { pixels / points } else { 2.0 })
}

fn number(dict: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<f64> {
    dict.objectForKey(&NSString::from_str(key))?.downcast::<NSNumber>().ok().map(|n| n.doubleValue())
}

fn string(dict: &NSDictionary<NSString, AnyObject>, key: &str) -> String {
    dict.objectForKey(&NSString::from_str(key))
        .and_then(|s| s.downcast::<NSString>().ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Normal app windows that are on screen, front to back, without Shotty's own.
pub fn windows() -> Result<Vec<WindowInfo>> {
    let own = f64::from(std::process::id());
    let options = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let list = CGWindowListCopyWindowInfo(options, kCGNullWindowID).context("window list unavailable")?;
    // CFArray of CFDictionary is toll-free bridged to NSArray of NSDictionary.
    let list: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe { &*(&*list as *const CFArray).cast() };
    Ok(list
        .iter()
        .filter_map(|w: Retained<NSDictionary<NSString, AnyObject>>| {
            if number(&w, "kCGWindowLayer")? != 0.0 || number(&w, "kCGWindowOwnerPID")? == own {
                return None;
            }
            if number(&w, "kCGWindowAlpha").is_some_and(|a| a <= 0.0) {
                return None;
            }
            let bounds = w.objectForKey(&NSString::from_str("kCGWindowBounds"))?.downcast::<NSDictionary>().ok()?;
            let bounds: &NSDictionary<NSString, AnyObject> = unsafe { &*(&*bounds as *const NSDictionary).cast() };
            let frame = Rect {
                x: number(bounds, "X")?,
                y: number(bounds, "Y")?,
                width: number(bounds, "Width")?,
                height: number(bounds, "Height")?,
            };
            let id = number(&w, "kCGWindowNumber")? as u32;
            (frame.width >= 40.0 && frame.height >= 40.0).then(|| WindowInfo {
                id,
                frame,
                title: string(&w, "kCGWindowName"),
                app: string(&w, "kCGWindowOwnerName"),
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

fn read_bgra(image: &screencapturekit::CGImage) -> Result<RgbaImage> {
    let (w, h) = (image.width() as u32, image.height() as u32);
    let data = image.bgra_data().map_err(|e| anyhow!("reading pixels: {e}"))?;
    RgbaImage::from_raw(w, h, data).context("pixel buffer has the wrong size")
}

/// A display frozen by [`freeze_displays`]. The picture is fixed; its pixels are still being read
/// back from the GPU and arrive with [`Frozen::pixels`].
pub struct Frozen {
    pub display: DisplayInfo,
    pixels: mpsc::Receiver<Result<RgbaImage>>,
}

impl Frozen {
    /// Waits for the pixels (≈20–30 ms after freezing on a Retina display).
    pub fn pixels(self) -> Result<DisplayShot> {
        let bgra = self.pixels.recv().map_err(|_| anyhow!("screenshot failed"))??;
        Ok(DisplayShot { display: self.display, bgra })
    }
}

/// Freezes every display at once and returns as soon as the pictures are taken (≈15 ms), before
/// their pixels are read back, so the selection overlay can open right away.
pub fn freeze_displays() -> Result<Vec<Frozen>> {
    let mut frozen = Vec::new();
    let mut taken = Vec::new();
    for display in displays()? {
        let (taken_tx, taken_rx) = mpsc::channel();
        let (pixels_tx, pixels_rx) = mpsc::channel();
        let rect = display.frame.to_cg();
        std::thread::spawn(move || match SCScreenshotManager::capture_image_in_rect(rect) {
            Ok(image) => {
                let _ = taken_tx.send(Ok(()));
                let _ = pixels_tx.send(read_bgra(&image));
            }
            Err(e) => {
                let _ = taken_tx.send(Err(anyhow!("screenshot failed (Screen Recording permission?): {e}")));
            }
        });
        taken.push(taken_rx);
        frozen.push(Frozen { display, pixels: pixels_rx });
    }
    for rx in taken {
        rx.recv().map_err(|_| anyhow!("capture thread died"))??;
    }
    Ok(frozen)
}

/// Captures every display at once.
pub fn capture_all_displays() -> Result<Vec<DisplayShot>> {
    freeze_displays()?.into_iter().map(Frozen::pixels).collect()
}

/// Captures one display.
pub fn capture_display(display: &DisplayInfo) -> Result<DisplayShot> {
    let image = SCScreenshotManager::capture_image_in_rect(display.frame.to_cg()).map_err(|e| anyhow!("screenshot failed: {e}"))?;
    Ok(DisplayShot { display: display.clone(), bgra: read_bgra(&image)? })
}

/// Wakes ScreenCaptureKit up once (the first capture in a process takes several times longer).
pub fn warm_up() {
    let _ = SCScreenshotManager::capture_image_in_rect(CGRect::new(0.0, 0.0, 8.0, 8.0));
}

/// Captures a single window on its own (transparent corners, no desktop behind it).
pub fn capture_window(window_id: u32) -> Result<RgbaImage> {
    let content = content()?;
    let window = content.windows().into_iter().find(|w| w.window_id() == window_id).context("window is gone")?;
    let filter = SCContentFilter::create().with_window(&window).build().map_err(|e| anyhow!("{e}"))?;
    shoot(&filter, Rect::from_cg(window.frame()))
}

mod record;
pub use record::{RecordOptions, Recording};
