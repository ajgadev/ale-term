//! The CPU renderer: draws into a bitmap in ordinary memory and hands the
//! finished picture to the window system.
//!
//! It exists because of memory. On Apple GPUs any render pass makes the
//! driver hold about 90 MB while the screen is changing; drawing on the CPU
//! needs only the bitmap itself (4 bytes per pixel).
//!
//! To stay fast it avoids repainting what has not changed. The app still
//! describes the whole frame every time, as a list of drawing commands. The
//! window is divided into horizontal bands; for each band the commands that
//! touch it are hashed, and only bands whose hash differs from the previous
//! frame are repainted. Typing a character repaints one band; a blinking
//! status somewhere repaints just that strip.

use std::collections::{HashMap, VecDeque};
use unicode_width::UnicodeWidthChar;

use crate::font::{CellMetrics, FontStack};
#[cfg(not(target_os = "macos"))]
use crate::present_copy::Presenter;
#[cfg(target_os = "macos")]
use crate::present_mac::Presenter;
use crate::{Color, FontSpec, WindowRef};

/// Height of a repaint band in pixels.
const BAND: i32 = 32;

const KIND_RECT: u8 = 0;
const KIND_MASK: u8 = 1;
const KIND_COLOR: u8 = 2;
const KIND_IMAGE: u8 = 3;

/// One thing to draw.
#[derive(Clone, Copy)]
struct Command {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: Color,
    kind: u8,
    /// Index into `glyphs`, for glyph commands.
    glyph: u32,
    /// Image key, for image commands.
    image: u64,
    /// Part of the image to show (x, y, width, height in 0..1).
    uv: [f32; 4],
}

/// A rasterized glyph kept in ordinary memory.
struct Glyph {
    /// Offset from the cell's top-left corner.
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    color: bool,
    /// One coverage byte per pixel, or RGBA for color glyphs.
    data: Box<[u8]>,
}

struct Image {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[derive(Clone, Copy)]
struct GlyphRun {
    start: u32,
    len: u32,
}

/// A surface to paint into, handed out by the presenter.
pub(crate) struct Frame<'a> {
    /// One `0xFFRRGGBB` value per pixel, `stride` pixels per row.
    pub pixels: &'a mut [u32],
    pub stride: usize,
    /// Hash of what each band of this surface currently shows; updated as
    /// bands are repainted.
    pub painted: &'a mut Vec<u64>,
}

/// Everything needed to paint a frame: what to draw and with what.
struct Scene {
    width: i32,
    height: i32,
    commands: Vec<Command>,
    clear: Color,
    /// For each band, the indices of the commands that touch it.
    bands: Vec<Vec<u32>>,
    glyphs: Vec<Glyph>,
    images: HashMap<u64, Image>,
    /// Changes whenever glyph or image contents may have changed behind
    /// unchanged ids (new font, replaced image), which invalidates every
    /// band at once.
    epoch: u64,
}

pub struct SoftRenderer {
    scene: Scene,
    presenter: Presenter,
    /// Scratch: the band hashes of the frame being presented.
    hashes: Vec<u64>,

    font: FontStack,
    char_cache: HashMap<(char, u8), GlyphRun>,
    cluster_cache: HashMap<(Box<str>, u8), GlyphRun>,

    image_order: VecDeque<u64>,
    image_bytes: usize,
}

fn style_bits(bold: bool, italic: bool) -> u8 {
    bold as u8 | (italic as u8) << 1
}

/// A color as an opaque pixel value.
fn pack(color: Color) -> u32 {
    0xFF00_0000 | (color[0] as u32) << 16 | (color[1] as u32) << 8 | color[2] as u32
}

/// Blends `(r, g, b)` over `dst` with coverage `alpha` (0..=255).
#[inline(always)]
fn blend(dst: u32, r: u32, g: u32, b: u32, alpha: u32) -> u32 {
    let inverse = 255 - alpha;
    let mix = |d: u32, s: u32| (d * inverse + s * alpha + 127) / 255;
    0xFF00_0000 | mix((dst >> 16) & 255, r) << 16 | mix((dst >> 8) & 255, g) << 8 | mix(dst & 255, b)
}

/// Mixes a value into a running hash.
#[inline(always)]
fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29)
}

impl SoftRenderer {
    pub fn new(window: WindowRef, width: u32, height: u32, font: &FontSpec) -> Result<SoftRenderer, String> {
        let mut renderer = SoftRenderer {
            scene: Scene {
                width: 0,
                height: 0,
                commands: Vec::with_capacity(8192),
                clear: [0, 0, 0, 255],
                bands: Vec::new(),
                glyphs: Vec::new(),
                images: HashMap::new(),
                epoch: 1,
            },
            presenter: Presenter::new(window)?,
            hashes: Vec::new(),
            font: FontStack::new(&font.family, font.size_px, font.line_height),
            char_cache: HashMap::new(),
            cluster_cache: HashMap::new(),
            image_order: VecDeque::new(),
            image_bytes: 0,
        };
        renderer.resize(width, height);
        Ok(renderer)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1) as i32, height.max(1) as i32);
        if (width, height) == (self.scene.width, self.scene.height) {
            return;
        }
        self.scene.width = width;
        self.scene.height = height;
        let bands = ((height + BAND - 1) / BAND) as usize;
        self.scene.bands = vec![Vec::new(); bands];
        self.presenter.resize(width as u32, height as u32, bands);
    }

    pub fn size(&self) -> (u32, u32) {
        (self.scene.width as u32, self.scene.height as u32)
    }

    pub fn set_font(&mut self, font: &FontSpec) {
        self.font.set_font(&font.family, font.size_px, font.line_height);
        self.scene.glyphs.clear();
        self.char_cache.clear();
        self.cluster_cache.clear();
        self.scene.epoch += 1;
    }

    pub fn metrics(&self) -> CellMetrics {
        self.font.metrics()
    }

    // ---- Describing a frame -------------------------------------------

    pub fn begin(&mut self, clear: Color) {
        self.scene.commands.clear();
        self.scene.clear = clear;
    }

    pub fn rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        if width <= 0.0 || height <= 0.0 || color[3] == 0 {
            return;
        }
        // Round the edges, not the size, so neighbouring rectangles meet.
        let (x0, y0) = (x.round() as i32, y.round() as i32);
        let (x1, y1) = ((x + width).round() as i32, (y + height).round() as i32);
        self.scene.commands.push(Command {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
            color,
            kind: KIND_RECT,
            glyph: 0,
            image: 0,
            uv: [0.0; 4],
        });
    }

    pub fn cell(&mut self, text: &str, bold: bool, italic: bool, x: f32, y: f32, color: Color) {
        let style = style_bits(bold, italic);
        let mut chars = text.chars();
        let run = match (chars.next(), chars.next()) {
            (None, _) => return,
            (Some(c), None) => match self.char_cache.get(&(c, style)) {
                Some(run) => *run,
                None => {
                    let run = self.rasterize(text, bold, italic);
                    self.char_cache.insert((c, style), run);
                    run
                }
            },
            _ => match self.cluster_cache.get(&(Box::from(text), style)) {
                Some(run) => *run,
                None => {
                    let run = self.rasterize(text, bold, italic);
                    self.cluster_cache.insert((Box::from(text), style), run);
                    run
                }
            },
        };
        let (x, y) = (x.round() as i32, y.round() as i32);
        for index in run.start..run.start + run.len {
            let glyph = &self.scene.glyphs[index as usize];
            self.scene.commands.push(Command {
                x: x + glyph.left,
                y: y + glyph.top,
                w: glyph.width,
                h: glyph.height,
                color,
                kind: if glyph.color { KIND_COLOR } else { KIND_MASK },
                glyph: index,
                image: 0,
                uv: [0.0; 4],
            });
        }
    }

    pub fn text(&mut self, text: &str, bold: bool, x: f32, y: f32, max_width: f32, color: Color) -> f32 {
        let cell_width = self.font.metrics().width;
        let mut advance = 0.0;
        let mut utf8 = [0u8; 4];
        for c in text.chars() {
            let width = c.width().unwrap_or(0) as f32 * cell_width;
            if width == 0.0 {
                continue;
            }
            if advance + width > max_width {
                break;
            }
            if c != ' ' {
                self.cell(c.encode_utf8(&mut utf8), bold, false, x + advance, y, color);
            }
            advance += width;
        }
        advance
    }

    pub fn text_width(&self, text: &str) -> f32 {
        let cells: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
        cells as f32 * self.font.metrics().width
    }

    fn rasterize(&mut self, text: &str, bold: bool, italic: bool) -> GlyphRun {
        let start = self.scene.glyphs.len() as u32;
        for glyph in self.font.rasterize(text, bold, italic) {
            self.scene.glyphs.push(Glyph {
                left: glyph.left,
                top: glyph.top,
                width: glyph.width as i32,
                height: glyph.height as i32,
                color: glyph.color,
                data: glyph.data.into_boxed_slice(),
            });
        }
        GlyphRun { start, len: self.scene.glyphs.len() as u32 - start }
    }

    // ---- Images -------------------------------------------------------

    pub fn upload_image(&mut self, key: u64, width: u32, height: u32, rgba: &[u8]) {
        let bytes = (width * height * 4) as usize;
        if width == 0 || height == 0 || rgba.len() < bytes {
            return;
        }
        self.remove_image(key);
        self.scene.images.insert(key, Image { width, height, rgba: rgba[..bytes].to_vec() });
        self.image_order.push_back(key);
        self.image_bytes += bytes;
        self.scene.epoch += 1;
    }

    pub fn has_image(&self, key: u64) -> bool {
        self.scene.images.contains_key(&key)
    }

    pub fn remove_image(&mut self, key: u64) {
        if let Some(image) = self.scene.images.remove(&key) {
            self.image_bytes -= image.rgba.len();
            self.image_order.retain(|k| *k != key);
            self.scene.epoch += 1;
        }
    }

    pub fn retain_images(&mut self, keep: impl Fn(u64) -> bool) {
        let doomed: Vec<u64> = self.scene.images.keys().copied().filter(|key| !keep(*key)).collect();
        for key in doomed {
            self.remove_image(key);
        }
    }

    pub fn limit_image_memory(&mut self, budget: usize) {
        while self.image_bytes > budget {
            let Some(oldest) = self.image_order.front().copied() else { break };
            self.remove_image(oldest);
        }
    }

    pub fn image_memory(&self) -> usize {
        self.image_bytes
    }

    pub fn image(&mut self, key: u64, x: f32, y: f32, width: f32, height: f32, uv: [f32; 4]) {
        if width > 0.0 && height > 0.0 && self.scene.images.contains_key(&key) {
            let (x0, y0) = (x.round() as i32, y.round() as i32);
            let (x1, y1) = ((x + width).round() as i32, (y + height).round() as i32);
            self.scene.commands.push(Command {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
                color: [255; 4],
                kind: KIND_IMAGE,
                glyph: 0,
                image: key,
                uv,
            });
        }
    }

    // ---- Painting -----------------------------------------------------

    /// Shows the frame. Does nothing at all when it is identical to what
    /// is already on screen.
    pub fn present(&mut self) -> bool {
        self.scene.bucket();
        self.scene.hash_bands(&mut self.hashes);
        if self.presenter.shown() == Some(self.hashes.as_slice()) {
            return false;
        }
        let Some(frame) = self.presenter.begin() else { return false };
        // The surface we were given may be a frame or more behind; repaint
        // exactly the bands where it differs from what we want.
        for (band, hash) in self.hashes.iter().enumerate() {
            if frame.painted.get(band) != Some(hash) {
                self.scene.paint_band(frame.pixels, frame.stride, band);
                if let Some(painted) = frame.painted.get_mut(band) {
                    *painted = *hash;
                }
            }
        }
        self.presenter.finish();
        false
    }

    /// Returns what is on screen right now, as RGBA pixels with its width
    /// and height. Unlike [`SoftRenderer::capture`], which paints a fresh
    /// picture, this reads back the surface the window is showing, so it
    /// also reflects any mistake in deciding what needed repainting.
    pub fn capture_shown(&self) -> Option<(u32, u32, Vec<u8>)> {
        let (pixels, stride) = self.presenter.read_shown()?;
        let (width, height) = (self.scene.width as usize, self.scene.height as usize);
        let mut rgba = Vec::with_capacity(width * height * 4);
        for row in pixels.chunks(stride).take(height) {
            for pixel in &row[..width] {
                rgba.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, 255]);
            }
        }
        Some((width as u32, height as u32, rgba))
    }

    /// Returns the frame as RGBA pixels with its width and height.
    pub fn capture(&mut self) -> (u32, u32, Vec<u8>) {
        self.scene.bucket();
        let (width, height) = (self.scene.width as usize, self.scene.height as usize);
        let mut pixels = vec![0u32; width * height];
        for band in 0..self.scene.bands.len() {
            self.scene.paint_band(&mut pixels, width, band);
        }
        let mut rgba = Vec::with_capacity(pixels.len() * 4);
        for pixel in &pixels {
            rgba.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, 255]);
        }
        (width as u32, height as u32, rgba)
    }
}

impl Scene {
    /// Sorts the commands into the bands they touch.
    fn bucket(&mut self) {
        for band in &mut self.bands {
            band.clear();
        }
        let last_band = self.bands.len() as i32 - 1;
        for (index, command) in self.commands.iter().enumerate() {
            let (top, bottom) = (command.y, command.y + command.h - 1);
            let off_screen = bottom < 0 || top >= self.height || command.x >= self.width || command.x + command.w <= 0;
            if command.w <= 0 || command.h <= 0 || off_screen {
                continue;
            }
            for band in (top.max(0) / BAND)..=(bottom / BAND).min(last_band) {
                self.bands[band as usize].push(index as u32);
            }
        }
    }

    /// Computes a hash of each band's contents. Two frames with the same
    /// hash for a band look the same there.
    fn hash_bands(&self, out: &mut Vec<u64>) {
        out.clear();
        let seed = mix(self.epoch, pack(self.clear) as u64);
        for band in &self.bands {
            let mut hash = seed;
            for &index in band {
                let c = &self.commands[index as usize];
                hash = mix(hash, (c.x as u32 as u64) << 32 | c.y as u32 as u64);
                hash = mix(hash, (c.w as u32 as u64) << 32 | c.h as u32 as u64);
                hash = mix(hash, u32::from_le_bytes(c.color) as u64 | (c.kind as u64) << 32 | (c.glyph as u64) << 40);
                if c.kind == KIND_IMAGE {
                    hash = mix(hash, c.image);
                    hash = mix(hash, (c.uv[0].to_bits() as u64) << 32 | c.uv[1].to_bits() as u64);
                    hash = mix(hash, (c.uv[2].to_bits() as u64) << 32 | c.uv[3].to_bits() as u64);
                }
            }
            // Zero is reserved for "nothing painted yet".
            out.push(hash | 1);
        }
    }

    /// Repaints one band of `pixels` (which has `stride` pixels per row).
    fn paint_band(&self, pixels: &mut [u32], stride: usize, band: usize) {
        let top = band as i32 * BAND;
        let bottom = (top + BAND).min(self.height);
        let width = self.width;
        let clear = pack(self.clear);
        for y in top..bottom {
            pixels[y as usize * stride..y as usize * stride + width as usize].fill(clear);
        }

        for &index in &self.bands[band] {
            let c = self.commands[index as usize];
            // The part of the command inside this band and the window.
            let (x0, x1) = (c.x.max(0), (c.x + c.w).min(width));
            let (y0, y1) = (c.y.max(top), (c.y + c.h).min(bottom));
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            let (r, g, b, a) = (c.color[0] as u32, c.color[1] as u32, c.color[2] as u32, c.color[3] as u32);
            match c.kind {
                KIND_RECT => {
                    let solid = pack(c.color);
                    for y in y0..y1 {
                        let row = &mut pixels[y as usize * stride + x0 as usize..y as usize * stride + x1 as usize];
                        if a == 255 {
                            row.fill(solid);
                        } else {
                            for pixel in row {
                                *pixel = blend(*pixel, r, g, b, a);
                            }
                        }
                    }
                }
                KIND_MASK => {
                    let glyph = &self.glyphs[c.glyph as usize];
                    for y in y0..y1 {
                        let source = ((y - c.y) * glyph.width + (x0 - c.x)) as usize;
                        let coverage = &glyph.data[source..source + (x1 - x0) as usize];
                        let row = &mut pixels[y as usize * stride + x0 as usize..y as usize * stride + x1 as usize];
                        for (pixel, &cover) in row.iter_mut().zip(coverage) {
                            if cover != 0 {
                                let alpha = if a == 255 { cover as u32 } else { cover as u32 * a / 255 };
                                *pixel = blend(*pixel, r, g, b, alpha);
                            }
                        }
                    }
                }
                KIND_COLOR => {
                    let glyph = &self.glyphs[c.glyph as usize];
                    for y in y0..y1 {
                        let source = ((y - c.y) * glyph.width + (x0 - c.x)) as usize * 4;
                        let texels = &glyph.data[source..source + (x1 - x0) as usize * 4];
                        let row = &mut pixels[y as usize * stride + x0 as usize..y as usize * stride + x1 as usize];
                        for (pixel, texel) in row.iter_mut().zip(texels.as_chunks::<4>().0) {
                            let alpha = texel[3] as u32 * a / 255;
                            if alpha != 0 {
                                *pixel = blend(*pixel, texel[0] as u32, texel[1] as u32, texel[2] as u32, alpha);
                            }
                        }
                    }
                }
                _ => {
                    let Some(image) = self.images.get(&c.image) else { continue };
                    let (iw, ih) = (image.width as i32, image.height as i32);
                    let row_bytes = image.width as usize * 4;
                    // Smooth (bilinear) scaling: each destination pixel is a
                    // weighted mix of the four source pixels around the
                    // point it maps to. Weights are 0..=256.
                    let locate = |position: f32, size: i32| {
                        let centered = position - 0.5;
                        let base = centered.floor();
                        let weight = ((centered - base) * 256.0) as u32;
                        let low = (base as i32).clamp(0, size - 1) as usize;
                        let high = (base as i32 + 1).clamp(0, size - 1) as usize;
                        (low, high, weight)
                    };
                    for y in y0..y1 {
                        let v = c.uv[1] + ((y - c.y) as f32 + 0.5) / c.h as f32 * c.uv[3];
                        let (row0, row1, wy) = locate(v * ih as f32, ih);
                        for x in x0..x1 {
                            let u = c.uv[0] + ((x - c.x) as f32 + 0.5) / c.w as f32 * c.uv[2];
                            let (col0, col1, wx) = locate(u * iw as f32, iw);
                            let texel = |row: usize, col: usize| &image.rgba[row * row_bytes + col * 4..][..4];
                            let (a00, a01) = (texel(row0, col0), texel(row0, col1));
                            let (a10, a11) = (texel(row1, col0), texel(row1, col1));
                            let channel = |i: usize| {
                                let top = a00[i] as u32 * (256 - wx) + a01[i] as u32 * wx;
                                let bottom = a10[i] as u32 * (256 - wx) + a11[i] as u32 * wx;
                                (top * (256 - wy) + bottom * wy + 32768) >> 16
                            };
                            let pixel = &mut pixels[y as usize * stride + x as usize];
                            *pixel = blend(*pixel, channel(0), channel(1), channel(2), channel(3));
                        }
                    }
                }
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blending_endpoints_are_exact() {
        let dst = 0xFF_10_20_30;
        assert_eq!(blend(dst, 200, 100, 50, 0), dst);
        assert_eq!(blend(dst, 200, 100, 50, 255), 0xFFC8_6432);
        // Half coverage lands halfway, per channel.
        assert_eq!(blend(0xFF_00_00_00, 255, 255, 255, 128), 0xFF_80_80_80);
        assert_eq!(blend(0xFF_FF_FF_FF, 0, 0, 0, 128), 0xFF_7F_7F_7F);
        // The result is always opaque, whatever was there.
        assert_eq!(blend(0x00_00_00_00, 0, 0, 0, 0) >> 24, 0xFF);
    }

    #[test]
    fn hashing_is_order_and_value_sensitive() {
        let a = mix(mix(1, 2), 3);
        assert_ne!(a, mix(mix(1, 3), 2));
        assert_ne!(a, mix(mix(1, 2), 4));
        assert_eq!(a, mix(mix(1, 2), 3));
    }

    #[test]
    fn colors_pack_as_opaque_pixels() {
        assert_eq!(pack([0x12, 0x34, 0x56, 0xFF]), 0xFF_12_34_56);
        // Colors are stored opaque; transparency is applied when blending.
        assert_eq!(pack([0x12, 0x34, 0x56, 0x00]), 0xFF_12_34_56);
    }
}
