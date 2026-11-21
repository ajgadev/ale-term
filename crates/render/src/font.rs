//! Font loading, measuring and glyph rasterization.
//!
//! Wraps `cosmic-text`, which finds system fonts, picks fallbacks for
//! characters the main font lacks (emoji, CJK, symbols) and turns glyph
//! outlines into bitmaps.

use unicode_width::UnicodeWidthChar;

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight,
};

/// Size of one terminal cell in physical pixels. All whole numbers, so every
/// cell starts on a pixel boundary and text stays sharp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    /// Distance from the top of the cell to the text baseline.
    pub baseline: f32,
}

/// A rasterized glyph, ready to be copied into the atlas.
pub struct RasterGlyph {
    /// Offset from the cell's top-left corner to the bitmap's top-left.
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// True for color glyphs (emoji); false for masks to be tinted.
    pub color: bool,
    /// Pixels: for a mask, one coverage byte per pixel; for a color glyph,
    /// four bytes (RGBA) per pixel.
    pub data: Vec<u8>,
}

impl RasterGlyph {
    /// The pixels as RGBA, with a mask expanded to white plus alpha.
    pub fn rgba(&self) -> std::borrow::Cow<'_, [u8]> {
        if self.color {
            return std::borrow::Cow::Borrowed(&self.data);
        }
        let mut rgba = Vec::with_capacity(self.data.len() * 4);
        for alpha in &self.data {
            rgba.extend_from_slice(&[255, 255, 255, *alpha]);
        }
        std::borrow::Cow::Owned(rgba)
    }
}

pub struct FontStack {
    system: FontSystem,
    swash: SwashCache,
    /// Scratch buffer reused for every shaping call.
    buffer: Buffer,
    family: String,
    metrics: CellMetrics,
}

impl FontStack {
    /// `size_px` is the font size in physical pixels.
    pub fn new(family: &str, size_px: f32, line_height: f32) -> Self {
        let mut system = FontSystem::new();
        let buffer = Buffer::new(&mut system, Metrics::new(size_px, size_px));
        let mut stack = Self {
            system,
            swash: SwashCache::new(),
            buffer,
            family: String::new(),
            metrics: CellMetrics { width: 1.0, height: 1.0, baseline: 1.0 },
        };
        stack.set_font(family, size_px, line_height);
        stack
    }

    pub fn set_font(&mut self, family: &str, size_px: f32, line_height: f32) {
        self.family = family.to_owned();
        let cell_height = (size_px * line_height).round().max(1.0);
        self.buffer.set_metrics(Metrics::new(size_px, cell_height));
        self.buffer.set_size(None, None);

        // Measure a representative glyph to get the cell width and baseline.
        self.shape("M", false, false);
        let mut width = (size_px * 0.6).round();
        let mut baseline = (cell_height * 0.8).round();
        if let Some(run) = self.buffer.layout_runs().next() {
            baseline = run.line_y.round();
            if let Some(glyph) = run.glyphs.first() {
                width = glyph.w.round();
            }
        }
        self.metrics = CellMetrics { width: width.max(1.0), height: cell_height, baseline };
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    fn shape(&mut self, text: &str, bold: bool, italic: bool) {
        let mut attrs = Attrs::new().family(Family::Name(&self.family));
        if bold {
            attrs = attrs.weight(Weight::BOLD);
        }
        if italic {
            attrs = attrs.style(Style::Italic);
        }
        self.buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.system, false);
    }

    /// Rasterizes a short piece of text laid out on the cell grid: usually a
    /// single character, sometimes a character plus combining marks, or a
    /// run of punctuation that the font may join into a ligature.
    ///
    /// The text is shaped as a whole (so the font can substitute ligature
    /// glyphs), but each glyph is then placed at the cell its character
    /// belongs to rather than where the font's own spacing would put it.
    /// That keeps everything on the grid even though the cell width is the
    /// font's advance rounded to whole pixels. Returns one bitmap per glyph
    /// (none for whitespace).
    pub fn rasterize(&mut self, text: &str, bold: bool, italic: bool) -> Vec<RasterGlyph> {
        self.shape(text, bold, italic);
        let baseline = self.metrics.baseline as i32;
        let cell_width = self.metrics.width as i32;

        let mut out = Vec::new();
        let Self { system, swash, buffer, .. } = self;
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                // Which cell this glyph's character starts in.
                let cells_before: usize = text
                    .get(..glyph.start)
                    .map_or(0, |before| before.chars().map(|c| c.width().unwrap_or(0)).sum());
                // Cancel the font's own pen position, keeping only the
                // glyph's offset relative to it.
                let physical = glyph.physical((-glyph.x, 0.0), 1.0);
                let Some(image) = swash.get_image_uncached(system, physical.cache_key) else {
                    continue;
                };
                let (width, height) = (image.placement.width, image.placement.height);
                if width == 0 || height == 0 {
                    continue;
                }
                let (color, data) = match image.content {
                    SwashContent::Mask => (false, image.data),
                    SwashContent::Color => (true, image.data),
                    // Subpixel masks are never requested.
                    SwashContent::SubpixelMask => continue,
                };
                out.push(RasterGlyph {
                    left: cells_before as i32 * cell_width + physical.x + image.placement.left,
                    top: baseline + physical.y - image.placement.top,
                    width,
                    height,
                    color,
                    data,
                });
            }
        }
        out
    }
}
