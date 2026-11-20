//! Drawing a terminal pane: turns the emulator's cell grid into rectangles
//! and glyphs for the renderer.

use ale_config::{Palette, Rgb};
use ale_core::TermState;
use ale_core::image::ImageCell;
use ale_core::vt::grid::Dimensions;
use ale_core::vt::index::Point;
use ale_core::vt::term::cell::Flags;
use ale_core::vt::term::color::Colors;
use ale_core::vt::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};
use ale_render::{CellMetrics, Color, Renderer};

use crate::boxdraw;
use crate::layout::Rect;

pub fn color(c: Rgb) -> Color {
    [c.r, c.g, c.b, 255]
}

pub struct PaneStyle<'a> {
    pub palette: &'a Palette,
    pub metrics: CellMetrics,
    /// Space between the pane's edge and its text.
    pub padding: f32,
    /// Whether this pane receives keyboard input (solid cursor) or not
    /// (hollow cursor).
    pub focused: bool,
    /// A link under the mouse to underline: first and last cell, inclusive.
    pub link: Option<(Point, Point)>,
    /// Shape runs of punctuation together so the font can form ligatures.
    pub ligatures: bool,
    /// Added to an image's id to form its key in the renderer, so images
    /// from different panes never collide.
    pub image_key_base: u64,
}

/// Resolves a terminal color to RGB. Colors a program redefined at runtime
/// (`overrides`) win over the theme.
pub fn resolve(c: AnsiColor, overrides: &Colors, palette: &Palette) -> Rgb {
    let from_override = |index: usize| overrides[index].map(|c| Rgb::new(c.r, c.g, c.b));
    match c {
        AnsiColor::Spec(c) => Rgb::new(c.r, c.g, c.b),
        AnsiColor::Indexed(index) => {
            from_override(index as usize).unwrap_or_else(|| palette.indexed(index))
        }
        AnsiColor::Named(name) => from_override(name as usize).unwrap_or_else(|| named(name, palette)),
    }
}

fn named(name: NamedColor, palette: &Palette) -> Rgb {
    let index = name as usize;
    match name {
        _ if index < 16 => palette.ansi[index],
        NamedColor::Background => palette.background,
        NamedColor::Cursor => palette.cursor,
        NamedColor::DimForeground => palette.foreground.mix(palette.background, 0.4),
        // The eight "dim" colors follow DimBlack in the same order as the
        // normal ones.
        _ if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&index) => {
            palette.ansi[index - NamedColor::DimBlack as usize].mix(palette.background, 0.4)
        }
        _ => palette.foreground,
    }
}

/// Foreground and background of a cell after applying reverse video, dim
/// and selection.
fn cell_colors(
    fg: AnsiColor,
    bg: AnsiColor,
    flags: Flags,
    selected: bool,
    overrides: &Colors,
    palette: &Palette,
) -> (Rgb, Rgb) {
    let mut fg = resolve(fg, overrides, palette);
    let mut bg = resolve(bg, overrides, palette);
    if flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if flags.contains(Flags::DIM) {
        fg = fg.mix(bg, 0.4);
    }
    if selected {
        bg = palette.selection;
    }
    (fg, bg)
}

/// Draws one pane into `rect`.
pub fn pane(r: &mut Renderer, term: &TermState, rect: Rect, style: &PaneStyle) {
    let palette = style.palette;
    let CellMetrics { width: cw, height: ch, baseline } = style.metrics;
    let origin_x = rect.x + style.padding;
    let origin_y = rect.y + style.padding;
    let columns = term.columns();
    let lines = term.screen_lines() as i32;

    let content = term.renderable_content();
    let overrides = content.colors;
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let default_bg = resolve(AnsiColor::Named(NamedColor::Background), overrides, palette);

    // A program may set its own default background (OSC 11); fill the whole
    // pane, padding included, so it does not look boxed in.
    if default_bg != palette.background {
        r.rect(rect.x, rect.y, rect.w, rect.h, color(default_bg));
    }

    let cursor = content.cursor;
    let cursor_row = cursor.point.line.0 + offset;
    let cursor_col = cursor.point.column.0;
    let cursor_visible = cursor.shape != CursorShape::Hidden && (0..lines).contains(&cursor_row);
    let solid_block = cursor_visible && style.focused && cursor.shape == CursorShape::Block;
    let mut cursor_cells = 1.0;

    // Pass 1: backgrounds. Neighbouring cells with the same background are
    // merged into one rectangle to keep the instance count down.
    let mut run: Option<(i32, usize, usize, Rgb)> = None; // row, first col, end col, color
    let mut images: Vec<ImageRun> = Vec::new();
    let flush = |r: &mut Renderer, run: &mut Option<(i32, usize, usize, Rgb)>| {
        if let Some((row, start, end, bg)) = run.take() {
            let x = origin_x + start as f32 * cw;
            r.rect(x, origin_y + row as f32 * ch, (end - start) as f32 * cw, ch, color(bg));
        }
    };
    for cell in content.display_iter {
        let row = cell.point.line.0 + offset;
        let col = cell.point.column.0;
        if row == cursor_row && col == cursor_col && cell.flags.contains(Flags::WIDE_CHAR) {
            cursor_cells = 2.0;
        }
        // Cells tagged as part of an image: gather each row's run of cells.
        if let Some(image_cell) = cell.hyperlink().and_then(|link| ImageCell::decode(link.uri())) {
            match images.last_mut() {
                Some(last) if last.cell == image_cell && last.row == row && last.end == col => last.end = col + 1,
                _ => images.push(ImageRun { cell: image_cell, row, start: col, end: col + 1 }),
            }
        }
        let selected = selection.is_some_and(|s| s.contains(cell.point));
        let (_, bg) = cell_colors(cell.fg, cell.bg, cell.flags, selected, overrides, palette);
        let extends = matches!(run, Some((r_row, _, end, r_bg)) if r_row == row && end == col && r_bg == bg);
        if extends {
            if let Some(run) = run.as_mut() {
                run.2 = col + 1;
            }
            continue;
        }
        flush(r, &mut run);
        if bg != default_bg {
            run = Some((row, col, col + 1, bg));
        }
    }
    flush(r, &mut run);

    // Images go over the backgrounds and under the cursor and text. Each
    // run of tagged cells shows one horizontal strip of its image.
    for image in &images {
        let ImageCell { id, row: image_row, width_cells, height_cells } = image.cell;
        if width_cells <= 0.0 || height_cells <= 0.0 {
            continue;
        }
        // The strip is one cell tall, except the last one, which may be
        // shorter; likewise the run may be wider than what is left to show.
        let strip_cells = (height_cells - image_row as f32).min(1.0);
        let shown_cells = ((image.end - image.start) as f32).min(width_cells);
        if strip_cells <= 0.0 {
            continue;
        }
        r.image(
            style.image_key_base + id as u64,
            origin_x + image.start as f32 * cw,
            origin_y + image.row as f32 * ch,
            (shown_cells * cw).round(),
            (strip_cells * ch).round(),
            [0.0, image_row as f32 / height_cells, shown_cells / width_cells, strip_cells / height_cells],
        );
    }

    // The cursor sits between backgrounds and text so a block cursor can
    // show the character under it.
    let cursor_color = color(resolve(AnsiColor::Named(NamedColor::Cursor), overrides, palette));
    if cursor_visible && cursor_col < columns {
        let x = origin_x + cursor_col as f32 * cw;
        let y = origin_y + cursor_row as f32 * ch;
        let w = cw * cursor_cells;
        let thin = (cw / 8.0).round().max(1.0);
        match cursor.shape {
            _ if !style.focused => {
                // Hollow box: this pane is not receiving input.
                r.rect(x, y, w, thin, cursor_color);
                r.rect(x, y + ch - thin, w, thin, cursor_color);
                r.rect(x, y, thin, ch, cursor_color);
                r.rect(x + w - thin, y, thin, ch, cursor_color);
            }
            CursorShape::Beam => r.rect(x, y, thin * 2.0, ch, cursor_color),
            CursorShape::Underline => r.rect(x, y + ch - thin * 2.0, w, thin * 2.0, cursor_color),
            _ => r.rect(x, y, w, ch, cursor_color),
        }
    }

    // Pass 2: text and decorations.
    let content = term.renderable_content();
    let mut cluster = String::new();
    let mut run = LigatureRun::default();
    for cell in content.display_iter {
        let flags = cell.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::HIDDEN) {
            continue;
        }
        let row = cell.point.line.0 + offset;
        let col = cell.point.column.0;
        let x = origin_x + col as f32 * cw;
        let y = origin_y + row as f32 * ch;

        let selected = selection.is_some_and(|s| s.contains(cell.point));
        let (fg, bg) = cell_colors(cell.fg, cell.bg, flags, selected, overrides, palette);
        let under_block_cursor = solid_block && row == cursor_row && col == cursor_col;
        let fg = color(if under_block_cursor { bg } else { fg });

        let width = if flags.contains(Flags::WIDE_CHAR) { cw * 2.0 } else { cw };
        let thin = (ch / 16.0).round().max(1.0);
        if flags.intersects(Flags::ALL_UNDERLINES) {
            r.rect(x, y + (baseline + thin * 2.0).min(ch - thin), width, thin, fg);
        }
        if style.link.is_some_and(|(start, end)| start <= cell.point && cell.point <= end) {
            r.rect(x, y + (baseline + thin * 2.0).min(ch - thin), width, thin, color(palette.accent));
        }
        if flags.contains(Flags::STRIKEOUT) {
            r.rect(x, y + (baseline * 0.68).round(), width, thin, fg);
        }

        let c = cell.c;
        let (bold, italic) = (flags.contains(Flags::BOLD), flags.contains(Flags::ITALIC));
        // Punctuation is collected into runs and drawn when the run ends.
        if style.ligatures && is_ligature_char(c) && cell.zerowidth().is_none() {
            if !run.extends(row, col, fg, bold, italic) {
                run.flush(r);
                run.start(row, x, y, fg, bold, italic);
            }
            run.push(c, col);
            continue;
        }
        run.flush(r);
        if c == ' ' || c == '\0' {
            continue;
        }
        if boxdraw::draw(r, c, x, y, cw, ch, fg) {
            continue;
        }
        match cell.zerowidth() {
            // A base character followed by combining marks.
            Some(marks) => {
                cluster.clear();
                cluster.push(c);
                cluster.extend(marks);
                r.cell(&cluster, bold, italic, x, y, fg);
            }
            None => {
                let mut utf8 = [0u8; 4];
                r.cell(c.encode_utf8(&mut utf8), bold, italic, x, y, fg);
            }
        }
    }
    run.flush(r);
}

/// Characters that programming fonts combine into ligatures (`->`, `!=`,
/// `>=`, `::`, ...).
fn is_ligature_char(c: char) -> bool {
    matches!(
        c,
        '!' | '#' | '$' | '%' | '&' | '*' | '+' | '-' | '.' | '/' | ':' | ';' | '<' | '=' | '>' | '?'
            | '@' | '\\' | '^' | '|' | '~' | '_'
    )
}

/// Neighbouring punctuation cells with the same style, drawn as one piece
/// of text so the font can substitute a ligature.
#[derive(Default)]
struct LigatureRun {
    text: String,
    row: i32,
    /// Column after the last character in the run.
    next_col: usize,
    x: f32,
    y: f32,
    color: Color,
    bold: bool,
    italic: bool,
}

impl LigatureRun {
    /// Longest run shaped at once. Long rules such as `--------` are split
    /// so they reuse a few cached pieces instead of one entry per length.
    const MAX: usize = 8;

    fn extends(&self, row: i32, col: usize, color: Color, bold: bool, italic: bool) -> bool {
        !self.text.is_empty()
            && self.text.len() < Self::MAX
            && (self.row, self.next_col) == (row, col)
            && (self.color, self.bold, self.italic) == (color, bold, italic)
    }

    fn start(&mut self, row: i32, x: f32, y: f32, color: Color, bold: bool, italic: bool) {
        (self.row, self.x, self.y) = (row, x, y);
        (self.color, self.bold, self.italic) = (color, bold, italic);
    }

    fn push(&mut self, c: char, col: usize) {
        self.text.push(c);
        self.next_col = col + 1;
    }

    fn flush(&mut self, r: &mut Renderer) {
        if !self.text.is_empty() {
            r.cell(&self.text, self.bold, self.italic, self.x, self.y, self.color);
            self.text.clear();
        }
    }
}

/// Cells on one screen row that show the same row of the same image.
struct ImageRun {
    cell: ImageCell,
    /// Screen row.
    row: i32,
    /// First column and the column after the last.
    start: usize,
    end: usize,
}
