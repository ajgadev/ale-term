//! Rendering for ale-term.
//!
//! Everything on screen is one of three things: a solid rectangle, a glyph
//! or an image. The app describes a frame by calling [`Renderer::rect`],
//! [`Renderer::cell`] / [`Renderer::text`] and [`Renderer::image`], then
//! [`Renderer::present`] shows it.
//!
//! There are two renderers behind that one interface:
//!
//! * [`Backend::Cpu`] paints into a bitmap in ordinary memory, repainting
//!   only what changed. It uses the least memory and is the default.
//! * [`Backend::Gpu`] draws through the graphics card. It is faster for
//!   very large windows under heavy output, at the cost of the memory the
//!   graphics driver holds while the screen is changing.
//!
//! All coordinates are physical pixels with the origin at the top-left.
//! This crate knows nothing about terminals: it draws what it is told.

mod atlas;
mod font;
mod gpu;
#[cfg(not(target_os = "macos"))]
mod present_copy;
#[cfg(target_os = "macos")]
mod present_mac;
mod soft;
mod upload;

use std::sync::Arc;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

pub use font::CellMetrics;

/// Straight (non-premultiplied) RGBA, 8 bits per channel.
pub type Color = [u8; 4];

/// Makes an opaque color from RGB.
pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    [r, g, b, 255]
}

/// Font settings in physical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub family: String,
    pub size_px: f32,
    pub line_height: f32,
}

/// Which renderer to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Gpu,
}

/// A window we can draw into.
pub trait Window: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static {}
impl<T: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static> Window for T {}

pub(crate) type WindowRef = Arc<dyn Window>;

pub enum Renderer {
    Cpu(Box<soft::SoftRenderer>),
    Gpu(Box<gpu::GpuRenderer>),
}

/// Calls the same method on whichever renderer is in use.
macro_rules! each {
    ($self:expr, $r:ident => $call:expr) => {
        match $self {
            Renderer::Cpu($r) => $call,
            Renderer::Gpu($r) => $call,
        }
    };
}

impl Renderer {
    /// Sets up drawing into `window`, whose size in pixels is given.
    pub async fn new(
        window: Arc<impl Window>,
        width: u32,
        height: u32,
        font: &FontSpec,
        backend: Backend,
    ) -> Result<Renderer, String> {
        let window: WindowRef = window;
        match backend {
            Backend::Cpu => soft::SoftRenderer::new(window, width, height, font).map(|r| Renderer::Cpu(Box::new(r))),
            Backend::Gpu => {
                gpu::GpuRenderer::new(window, width, height, font).await.map(|r| Renderer::Gpu(Box::new(r)))
            }
        }
    }

    pub fn backend(&self) -> Backend {
        match self {
            Renderer::Cpu(_) => Backend::Cpu,
            Renderer::Gpu(_) => Backend::Gpu,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        each!(self, r => r.resize(width, height))
    }

    pub fn size(&self) -> (u32, u32) {
        each!(self, r => r.size())
    }

    /// Switches font. Every cached glyph is thrown away.
    pub fn set_font(&mut self, font: &FontSpec) {
        each!(self, r => r.set_font(font))
    }

    pub fn metrics(&self) -> CellMetrics {
        each!(self, r => r.metrics())
    }

    /// Starts a new frame filled with `clear`.
    pub fn begin(&mut self, clear: Color) {
        each!(self, r => r.begin(clear))
    }

    /// A solid rectangle.
    pub fn rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        each!(self, r => r.rect(x, y, width, height, color))
    }

    /// Draws the text of one terminal cell whose top-left corner is at
    /// (`x`, `y`). `text` is a single character, optionally followed by
    /// combining marks, or a short run the font may join into a ligature.
    pub fn cell(&mut self, text: &str, bold: bool, italic: bool, x: f32, y: f32, color: Color) {
        each!(self, r => r.cell(text, bold, italic, x, y, color))
    }

    /// Draws a line of text on the cell grid starting at (`x`, `y`), cut
    /// off at `max_width` pixels. Returns the width drawn.
    pub fn text(&mut self, text: &str, bold: bool, x: f32, y: f32, max_width: f32, color: Color) -> f32 {
        each!(self, r => r.text(text, bold, x, y, max_width, color))
    }

    /// Width in pixels that [`Renderer::text`] would need for `text`.
    pub fn text_width(&self, text: &str) -> f32 {
        each!(self, r => r.text_width(text))
    }

    /// Stores an image under `key`, replacing any image with that key.
    pub fn upload_image(&mut self, key: u64, width: u32, height: u32, rgba: &[u8]) {
        each!(self, r => r.upload_image(key, width, height, rgba))
    }

    pub fn remove_image(&mut self, key: u64) {
        each!(self, r => r.remove_image(key))
    }

    /// Removes every image whose key fails `keep`.
    pub fn retain_images(&mut self, keep: impl Fn(u64) -> bool) {
        each!(self, r => r.retain_images(keep))
    }

    /// Frees the oldest images until at most `budget` bytes are in use.
    pub fn limit_image_memory(&mut self, budget: usize) {
        each!(self, r => r.limit_image_memory(budget))
    }

    pub fn image_memory(&self) -> usize {
        each!(self, r => r.image_memory())
    }

    /// Draws part of an image. `uv` is the part to show as x, y, width and
    /// height in the 0..1 range. Does nothing if the image is not loaded.
    pub fn image(&mut self, key: u64, x: f32, y: f32, width: f32, height: f32, uv: [f32; 4]) {
        each!(self, r => r.image(key, x, y, width, height, uv))
    }

    /// Shows the frame in the window.
    ///
    /// Returns `true` when the frame should be described and presented
    /// again (the GPU renderer asks for this after rebuilding its glyph
    /// texture or recreating its surface).
    pub fn present(&mut self) -> bool {
        each!(self, r => r.present())
    }

    /// Reads back the picture currently on screen (CPU renderer only).
    /// Used by tests to check that incremental repainting leaves the
    /// window showing exactly what a full repaint would.
    pub fn capture_shown(&self) -> Option<(u32, u32, Vec<u8>)> {
        match self {
            Renderer::Cpu(r) => r.capture_shown(),
            Renderer::Gpu(_) => None,
        }
    }

    /// Draws the frame without showing it and returns it as RGBA pixels
    /// with its width and height. Used for screenshots in tests.
    pub fn capture(&mut self) -> (u32, u32, Vec<u8>) {
        each!(self, r => r.capture())
    }
}
