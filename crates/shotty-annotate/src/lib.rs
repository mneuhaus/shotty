//! Annotations on top of a screenshot, and rendering them into the final picture.
//!
//! Everything is in image pixels. The editor draws the same shapes live with GPUI; the geometry
//! helpers here (`arrow_head`, `ellipse_curves`, `pixelate`, `text_size`) are shared so the
//! preview and the exported PNG agree.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use image::RgbaImage;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

pub const FONT: &[u8] = include_bytes!("../assets/Barlow-SemiBold.ttf");
pub const FONT_FAMILY: &str = "Barlow";

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

pub const fn pt(x: f32, y: f32) -> Pt {
    Pt { x, y }
}

/// An axis-aligned rectangle with non-negative size.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Area {
    pub fn spanning(a: Pt, b: Pt) -> Self {
        Self { x: a.x.min(b.x), y: a.y.min(b.y), w: (a.x - b.x).abs(), h: (a.y - b.y).abs() }
    }

    pub fn contains(&self, p: Pt) -> bool {
        p.x >= self.x && p.y >= self.y && p.x <= self.x + self.w && p.y <= self.y + self.h
    }

    pub fn grow(&self, by: f32) -> Self {
        Self { x: self.x - by, y: self.y - by, w: self.w + 2. * by, h: self.h + 2. * by }
    }

    /// Clamped to `width`×`height`, in whole pixels; `None` if nothing is left.
    pub fn clamp_px(&self, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
        let x0 = self.x.max(0.).round() as u32;
        let y0 = self.y.max(0.).round() as u32;
        let x1 = ((self.x + self.w).round().max(0.) as u32).min(width);
        let y1 = ((self.y + self.h).round().max(0.) as u32).min(height);
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn hex(v: u32) -> Self {
        Self { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }

    pub fn to_hex(self) -> u32 {
        (u32::from(self.r) << 16) | (u32::from(self.g) << 8) | u32::from(self.b)
    }

    /// Black or white, whichever reads better on this color.
    pub fn contrast(self) -> Self {
        let luma = 0.299 * f32::from(self.r) + 0.587 * f32::from(self.g) + 0.114 * f32::from(self.b);
        if luma > 170. { Self::hex(0x111111) } else { Self::hex(0xffffff) }
    }
}

pub const PALETTE: [Color; 7] = [
    Color::hex(0xff3b30),
    Color::hex(0xff9500),
    Color::hex(0xffcc00),
    Color::hex(0x34c759),
    Color::hex(0x0a84ff),
    Color::hex(0xbf5af2),
    Color::hex(0x1c1c1e),
];

/// Opacity of the highlighter marker.
pub const HIGHLIGHT_ALPHA: f32 = 0.38;

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Arrow { from: Pt, to: Pt, color: Color, width: f32 },
    Line { from: Pt, to: Pt, color: Color, width: f32 },
    Rect { area: Area, color: Color, width: f32 },
    Ellipse { area: Area, color: Color, width: f32 },
    Highlight { area: Area, color: Color },
    /// A label: text on a filled rounded pill, `at` is the pill's top-left corner.
    Text { at: Pt, text: String, color: Color, size: f32 },
    /// A numbered circle for step-by-step instructions.
    Step { center: Pt, number: u32, color: Color, radius: f32 },
    Pixelate { area: Area },
}

impl Shape {
    pub fn bounds(&self) -> Area {
        match self {
            Shape::Arrow { from, to, width, .. } | Shape::Line { from, to, width, .. } => {
                Area::spanning(*from, *to).grow(width * 2.)
            }
            Shape::Rect { area, .. } | Shape::Ellipse { area, .. } | Shape::Highlight { area, .. } | Shape::Pixelate { area } => {
                *area
            }
            Shape::Text { at, text, size, .. } => {
                let (w, h) = pill_size(text, *size);
                Area { x: at.x, y: at.y, w, h }
            }
            Shape::Step { center, radius, .. } => {
                Area { x: center.x - radius, y: center.y - radius, w: radius * 2., h: radius * 2. }
            }
        }
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        let mv = |p: &mut Pt| {
            p.x += dx;
            p.y += dy;
        };
        match self {
            Shape::Arrow { from, to, .. } | Shape::Line { from, to, .. } => {
                mv(from);
                mv(to);
            }
            Shape::Rect { area, .. } | Shape::Ellipse { area, .. } | Shape::Highlight { area, .. } | Shape::Pixelate { area } => {
                area.x += dx;
                area.y += dy;
            }
            Shape::Text { at, .. } => mv(at),
            Shape::Step { center, .. } => mv(center),
        }
    }

    /// Whether `p` hits the shape, with `slop` pixels of tolerance for thin strokes.
    pub fn hit(&self, p: Pt, slop: f32) -> bool {
        match self {
            Shape::Arrow { from, to, width, .. } | Shape::Line { from, to, width, .. } => {
                segment_distance(p, *from, *to) <= width / 2. + slop
            }
            Shape::Rect { area, width, .. } | Shape::Ellipse { area, width, .. } => {
                let outer = area.grow(width / 2. + slop);
                let inner = area.grow(-(width / 2. + slop));
                outer.contains(p) && !(inner.w > 0. && inner.h > 0. && inner.contains(p))
            }
            _ => self.bounds().grow(slop).contains(p),
        }
    }
}

fn segment_distance(p: Pt, a: Pt, b: Pt) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0. { 0. } else { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0., 1.) };
    let (cx, cy) = (a.x + t * dx, a.y + t * dy);
    ((p.x - cx).powi(2) + (p.y - cy).powi(2)).sqrt()
}

/// The arrow's shaft end and its filled head (tip, left, right).
pub fn arrow_head(from: Pt, to: Pt, width: f32) -> (Pt, [Pt; 3]) {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let (ux, uy) = (dx / len, dy / len);
    let head = (width * 3.6 + 6.).min(len * 0.6);
    let half = head * 0.58;
    let base = pt(to.x - ux * head, to.y - uy * head);
    let shaft_end = pt(to.x - ux * head * 0.5, to.y - uy * head * 0.5);
    (shaft_end, [to, pt(base.x - uy * half, base.y + ux * half), pt(base.x + uy * half, base.y - ux * half)])
}

/// An ellipse as a start point and four cubic Bézier segments (control 1, control 2, end).
pub fn ellipse_curves(a: Area) -> (Pt, [(Pt, Pt, Pt); 4]) {
    const K: f32 = 0.552_284_8;
    let (rx, ry) = (a.w / 2., a.h / 2.);
    let (cx, cy) = (a.x + rx, a.y + ry);
    let (ox, oy) = (rx * K, ry * K);
    (
        pt(cx + rx, cy),
        [
            (pt(cx + rx, cy + oy), pt(cx + ox, cy + ry), pt(cx, cy + ry)),
            (pt(cx - ox, cy + ry), pt(cx - rx, cy + oy), pt(cx - rx, cy)),
            (pt(cx - rx, cy - oy), pt(cx - ox, cy - ry), pt(cx, cy - ry)),
            (pt(cx + ox, cy - ry), pt(cx + rx, cy - oy), pt(cx + rx, cy)),
        ],
    )
}

/// Pixel block size for a pixelated area: coarse enough that text becomes unreadable.
pub fn pixel_block(area: Area) -> u32 {
    ((area.w.min(area.h) / 6.).clamp(10., 28.)) as u32
}

/// The pixelated replacement for `area` (clamped to the image), and where it goes.
pub fn pixelate(base: &RgbaImage, area: Area) -> Option<(u32, u32, RgbaImage)> {
    let (x, y, w, h) = area.clamp_px(base.width(), base.height())?;
    let block = pixel_block(area);
    let src = image::imageops::crop_imm(base, x, y, w, h).to_image();
    let small = image::imageops::resize(&src, w.div_ceil(block), h.div_ceil(block), image::imageops::FilterType::Triangle);
    let big = image::imageops::resize(&small, w, h, image::imageops::FilterType::Nearest);
    Some((x, y, big))
}

fn font() -> FontRef<'static> {
    FontRef::try_from_slice(FONT).expect("embedded font is valid")
}

/// Width and line height of `text` at `size` pixels (widest line for multi-line text).
pub fn text_size(text: &str, size: f32) -> (f32, f32) {
    let font = font();
    let scaled = font.as_scaled(PxScale::from(size));
    let line_height = size * 1.22;
    let width = text
        .lines()
        .map(|line| {
            let mut w = 0.;
            let mut prev = None;
            for c in line.chars() {
                let id = scaled.glyph_id(c);
                if let Some(p) = prev {
                    w += scaled.kern(p, id);
                }
                w += scaled.h_advance(id);
                prev = Some(id);
            }
            w
        })
        .fold(0., f32::max);
    (width, line_height * text.lines().count().max(1) as f32)
}

pub fn pill_padding(size: f32) -> (f32, f32) {
    (size * 0.5, size * 0.22)
}

pub fn pill_size(text: &str, size: f32) -> (f32, f32) {
    let (w, h) = text_size(text, size);
    let (px, py) = pill_padding(size);
    (w + 2. * px, h + 2. * py)
}

pub fn step_font_size(radius: f32) -> f32 {
    radius * 1.15
}

#[derive(Clone, Debug, Default)]
pub struct Document {
    pub shapes: Vec<Shape>,
    pub crop: Option<Area>,
}

fn paint(color: Color, alpha: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(color.r, color.g, color.b, (alpha * 255.).round() as u8);
    p.anti_alias = true;
    p
}

fn stroke(width: f32) -> Stroke {
    Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() }
}

fn rounded_rect(a: Area, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(a.w / 2.).min(a.h / 2.);
    let k = 0.552_284_8 * r;
    let (x0, y0, x1, y1) = (a.x, a.y, a.x + a.w, a.y + a.h);
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.cubic_to(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.cubic_to(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0);
    pb.close();
    pb.finish()
}

/// Draws `text` with its first line's top at `top`, blending glyph coverage into the pixmap.
fn draw_text(pixmap: &mut Pixmap, text: &str, left: f32, top: f32, size: f32, color: Color) {
    let font = font();
    let scaled = font.as_scaled(PxScale::from(size));
    let line_height = size * 1.22;
    let baseline_shift = (line_height - (scaled.ascent() - scaled.descent())) / 2. + scaled.ascent();
    let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
    let data = pixmap.data_mut();
    for (row, line) in text.lines().enumerate() {
        let mut x = left;
        let baseline = top + row as f32 * line_height + baseline_shift;
        let mut prev = None;
        for c in line.chars() {
            let id = scaled.glyph_id(c);
            if let Some(p) = prev {
                x += scaled.kern(p, id);
            }
            let glyph = id.with_scale_and_position(PxScale::from(size), ab_glyph::point(x, baseline));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let b = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                    if px < 0 || py < 0 || px >= w || py >= h || coverage <= 0. {
                        return;
                    }
                    let i = ((py * w + px) * 4) as usize;
                    let a = coverage.min(1.);
                    // Premultiplied source-over.
                    for (k, c) in [color.r, color.g, color.b].into_iter().enumerate() {
                        data[i + k] = (f32::from(c) * a + f32::from(data[i + k]) * (1. - a)).round() as u8;
                    }
                    data[i + 3] = (255. * a + f32::from(data[i + 3]) * (1. - a)).round() as u8;
                });
            }
            x += scaled.h_advance(id);
            prev = Some(id);
        }
    }
}

fn to_pixmap(image: &RgbaImage) -> Pixmap {
    let mut data = image.as_raw().clone();
    for px in data.chunks_exact_mut(4) {
        let a = u16::from(px[3]);
        for c in &mut px[..3] {
            *c = ((u16::from(*c) * a + 127) / 255) as u8;
        }
    }
    Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(image.width(), image.height()).expect("non-empty image"))
        .expect("buffer matches size")
}

fn from_pixmap(pixmap: Pixmap) -> RgbaImage {
    let (w, h) = (pixmap.width(), pixmap.height());
    let mut data = pixmap.take();
    for px in data.chunks_exact_mut(4) {
        let a = u16::from(px[3]);
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    RgbaImage::from_raw(w, h, data).expect("buffer matches size")
}

/// Renders one shape onto the pixmap. `base` is the untouched capture (pixelation samples it).
pub fn draw_shape(pixmap: &mut Pixmap, base: &RgbaImage, shape: &Shape) {
    let t = Transform::identity();
    match shape {
        Shape::Arrow { from, to, color, width } => {
            let (shaft_end, head) = arrow_head(*from, *to, *width);
            let mut pb = PathBuilder::new();
            pb.move_to(from.x, from.y);
            pb.line_to(shaft_end.x, shaft_end.y);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint(*color, 1.), &stroke(*width), t, None);
            }
            let mut pb = PathBuilder::new();
            pb.move_to(head[0].x, head[0].y);
            pb.line_to(head[1].x, head[1].y);
            pb.line_to(head[2].x, head[2].y);
            pb.close();
            if let Some(path) = pb.finish() {
                pixmap.fill_path(&path, &paint(*color, 1.), FillRule::Winding, t, None);
                let mut s = stroke(width * 0.5);
                s.line_join = LineJoin::Round;
                pixmap.stroke_path(&path, &paint(*color, 1.), &s, t, None);
            }
        }
        Shape::Line { from, to, color, width } => {
            let mut pb = PathBuilder::new();
            pb.move_to(from.x, from.y);
            pb.line_to(to.x, to.y);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint(*color, 1.), &stroke(*width), t, None);
            }
        }
        Shape::Rect { area, color, width } => {
            if let Some(path) = rounded_rect(*area, width * 0.9) {
                pixmap.stroke_path(&path, &paint(*color, 1.), &stroke(*width), t, None);
            }
        }
        Shape::Ellipse { area, color, width } => {
            let (start, curves) = ellipse_curves(*area);
            let mut pb = PathBuilder::new();
            pb.move_to(start.x, start.y);
            for (c1, c2, end) in curves {
                pb.cubic_to(c1.x, c1.y, c2.x, c2.y, end.x, end.y);
            }
            pb.close();
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint(*color, 1.), &stroke(*width), t, None);
            }
        }
        Shape::Highlight { area, color } => {
            if let Some(rect) = tiny_skia::Rect::from_xywh(area.x, area.y, area.w, area.h) {
                pixmap.fill_rect(rect, &paint(*color, HIGHLIGHT_ALPHA), t, None);
            }
        }
        Shape::Text { at, text, color, size } => {
            let (w, h) = pill_size(text, *size);
            if let Some(path) = rounded_rect(Area { x: at.x, y: at.y, w, h }, size * 0.32) {
                pixmap.fill_path(&path, &paint(*color, 1.), FillRule::Winding, t, None);
            }
            let (px, py) = pill_padding(*size);
            draw_text(pixmap, text, at.x + px, at.y + py, *size, color.contrast());
        }
        Shape::Step { center, number, color, radius } => {
            if let Some(circle) = PathBuilder::from_circle(center.x, center.y, *radius) {
                pixmap.fill_path(&circle, &paint(*color, 1.), FillRule::Winding, t, None);
                pixmap.stroke_path(&circle, &paint(Color::hex(0xffffff), 1.), &stroke(radius * 0.14), t, None);
            }
            let text = number.to_string();
            let size = step_font_size(*radius);
            let (w, h) = text_size(&text, size);
            draw_text(pixmap, &text, center.x - w / 2., center.y - h / 2., size, color.contrast());
        }
        Shape::Pixelate { area } => {
            if let Some((x, y, patch)) = pixelate(base, *area) {
                let patch = to_pixmap(&patch);
                pixmap.draw_pixmap(x as i32, y as i32, patch.as_ref(), &tiny_skia::PixmapPaint::default(), t, None);
            }
        }
    }
}

/// The final picture: all shapes drawn in order, then cropped.
pub fn render(base: &RgbaImage, doc: &Document) -> RgbaImage {
    let mut pixmap = to_pixmap(base);
    // Pixelation goes first so later shapes (arrows pointing at a redacted spot) stay visible.
    for shape in doc.shapes.iter().filter(|s| matches!(s, Shape::Pixelate { .. })) {
        draw_shape(&mut pixmap, base, shape);
    }
    for shape in doc.shapes.iter().filter(|s| !matches!(s, Shape::Pixelate { .. })) {
        draw_shape(&mut pixmap, base, shape);
    }
    let out = from_pixmap(pixmap);
    match doc.crop.and_then(|c| c.clamp_px(out.width(), out.height())) {
        Some((x, y, w, h)) => image::imageops::crop_imm(&out, x, y, w, h).to_image(),
        None => out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> RgbaImage {
        RgbaImage::from_pixel(200, 120, image::Rgba([240, 240, 240, 255]))
    }

    #[test]
    fn untouched_document_is_identical() {
        let base = canvas();
        assert_eq!(render(&base, &Document::default()), base);
    }

    #[test]
    fn rect_stroke_lands_on_its_edge_only() {
        let base = canvas();
        let red = PALETTE[0];
        let doc = Document {
            shapes: vec![Shape::Rect { area: Area { x: 20., y: 20., w: 100., h: 60. }, color: red, width: 6. }],
            crop: None,
        };
        let out = render(&base, &doc);
        assert_eq!(out.get_pixel(70, 20).0, [red.r, red.g, red.b, 255], "top edge is red");
        assert_eq!(out.get_pixel(70, 50).0, [240, 240, 240, 255], "inside untouched");
    }

    #[test]
    fn crop_and_pixelate() {
        let mut base = canvas();
        for x in 0..200 {
            base.put_pixel(x, 50, image::Rgba([0, 0, 0, 255]));
        }
        let doc = Document {
            shapes: vec![Shape::Pixelate { area: Area { x: 0., y: 40., w: 200., h: 30. } }],
            crop: Some(Area { x: 10., y: 10., w: 50., h: 80. }),
        };
        let out = render(&base, &doc);
        assert_eq!((out.width(), out.height()), (50, 80));
        // The sharp black line is smeared into grey blocks.
        assert!(out.get_pixel(5, 40).0[0] > 20, "line no longer crisp black");
    }

    #[test]
    fn text_pill_has_ink_and_matching_bounds() {
        let base = canvas();
        let shape = Shape::Text { at: pt(10., 10.), text: "Hi".into(), color: PALETTE[4], size: 30. };
        let b = shape.bounds();
        assert!(b.w > 40. && b.h > 30.);
        let out = render(&base, &Document { shapes: vec![shape], crop: None });
        let inner = (b.x as u32 + 4, b.y as u32 + 4);
        assert_eq!(out.get_pixel(inner.0, inner.1).0[..3], [PALETTE[4].r, PALETTE[4].g, PALETTE[4].b]);
        let white = (0..b.w as u32).flat_map(|x| (0..b.h as u32).map(move |y| (x, y))).any(|(x, y)| {
            let p = out.get_pixel(10 + x, 10 + y).0;
            p[0] > 230 && p[1] > 230 && p[2] > 230
        });
        assert!(white, "white text drawn on the pill");
    }

    #[test]
    fn hit_testing_lines_and_frames() {
        let line = Shape::Line { from: pt(0., 0.), to: pt(100., 0.), color: PALETTE[0], width: 4. };
        assert!(line.hit(pt(50., 3.), 2.));
        assert!(!line.hit(pt(50., 20.), 2.));
        let rect = Shape::Rect { area: Area { x: 0., y: 0., w: 100., h: 100. }, color: PALETTE[0], width: 4. };
        assert!(rect.hit(pt(0., 50.), 3.));
        assert!(!rect.hit(pt(50., 50.), 3.), "inside of an outline is not a hit");
    }
}
