//! Turning a captured image into a file, a clipboard entry and something GPUI can draw.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use gpui_kit::RenderImage;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{Frame, ImageEncoder, RgbaImage};

pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
        .write_image(image.as_raw(), image.width(), image.height(), image::ExtendedColorType::Rgba8)
        .context("encoding PNG")?;
    Ok(out)
}

/// "Shotty 2026-10-08 at 12.34.56.png", with " 2", " 3" … appended on a clash.
pub fn unique_path(dir: &Path, stem_prefix: &str, ext: &str) -> PathBuf {
    let stem = format!("{stem_prefix} {}", chrono::Local::now().format("%Y-%m-%d at %H.%M.%S"));
    let mut path = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} {n}.{ext}"));
        n += 1;
    }
    path
}

pub fn save_png(dir: &Path, png: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = unique_path(dir, "Shotty", "png");
    std::fs::write(&path, png).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// GPUI draws BGRA; our images are RGBA.
pub fn render_image(image: &RgbaImage) -> Arc<RenderImage> {
    let mut bgra = image.clone();
    shotty_capture::swap_red_blue(&mut bgra);
    bgra_render_image(bgra)
}

/// A picture that is already in BGRA byte order (a frozen display).
pub fn bgra_render_image(bgra: RgbaImage) -> Arc<RenderImage> {
    Arc::new(RenderImage::new([Frame::new(bgra)]))
}
