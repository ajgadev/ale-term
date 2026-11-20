//! Panels drawn on top of the terminal: the command palette and the search
//! bar. This module holds their state, layout and drawing; the app decides
//! what happens when a command is chosen or the query changes.

use ale_config::Palette;
use ale_render::Renderer;

use crate::actions::Action;
use crate::draw::color;
use crate::layout::{PaneId, Rect};

pub enum Overlay {
    None,
    Palette {
        query: String,
        /// Index into the filtered command list.
        selected: usize,
    },
    Search {
        pane: PaneId,
        query: String,
        /// The query has no match; shown in red.
        failed: bool,
    },
}

impl Overlay {
    pub fn is_open(&self) -> bool {
        !matches!(self, Overlay::None)
    }

    /// The text being edited, if any.
    pub fn query_mut(&mut self) -> Option<&mut String> {
        match self {
            Overlay::Palette { query, .. } | Overlay::Search { query, .. } => Some(query),
            Overlay::None => None,
        }
    }
}

const PALETTE_ROWS: usize = 12;

/// Where the palette and its rows are on screen.
pub struct PaletteLayout {
    pub frame: Rect,
    pub input: Rect,
    pub rows: Vec<Rect>,
    /// Index of the first command shown (the list scrolls with the
    /// selection).
    pub first: usize,
}

impl PaletteLayout {
    pub fn new(window: (f32, f32), cell: (f32, f32), scale: f32, count: usize, selected: usize) -> Self {
        let row_height = (cell.1 + 12.0 * scale).round();
        let width = (560.0 * scale).min(window.0 - 32.0 * scale).max(cell.0 * 10.0).round();
        let x = ((window.0 - width) / 2.0).round();
        let y = (window.1 * 0.16).round();
        let visible = count.min(PALETTE_ROWS);
        let first = selected.saturating_sub(PALETTE_ROWS - 1).min(count.saturating_sub(visible));
        let input = Rect::new(x, y, width, row_height);
        let rows = (0..visible)
            .map(|i| Rect::new(x, y + row_height * (i + 1) as f32, width, row_height))
            .collect();
        let frame = Rect::new(x, y, width, row_height * (visible + 1) as f32);
        Self { frame, input, rows, first }
    }

    /// The command index under a window position.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        self.rows.iter().position(|row| row.contains(x, y)).map(|i| i + self.first)
    }
}

/// A one-pixel-scaled outline around `rect`.
fn outline(r: &mut Renderer, rect: Rect, line: f32, c: ale_render::Color) {
    r.rect(rect.x, rect.y, rect.w, line, c);
    r.rect(rect.x, rect.y + rect.h - line, rect.w, line, c);
    r.rect(rect.x, rect.y, line, rect.h, c);
    r.rect(rect.x + rect.w - line, rect.y, line, rect.h, c);
}

pub fn draw_palette(
    r: &mut Renderer,
    query: &str,
    selected: usize,
    commands: &[&(Action, &str, &str)],
    window: (f32, f32),
    scale: f32,
    palette: &Palette,
) {
    let metrics = r.metrics();
    let layout = PaletteLayout::new(window, (metrics.width, metrics.height), scale, commands.len(), selected);
    let line = scale.round().max(1.0);
    let pad = (14.0 * scale).round();
    let text_dy = ((layout.input.h - metrics.height) / 2.0).round();

    // Dim everything behind the palette.
    let [br, bg, bb, _] = color(palette.background);
    r.rect(0.0, 0.0, window.0, window.1, [br, bg, bb, 110]);

    let frame = layout.frame;
    r.rect(frame.x, frame.y, frame.w, frame.h, color(palette.chrome));
    outline(r, frame.inflate(line), line, color(palette.border));

    // Input line with a caret.
    let input = layout.input;
    let text_width = input.w - pad * 2.0;
    let typed = if query.is_empty() {
        r.text("Type a command", false, input.x + pad, input.y + text_dy, text_width, color(palette.chrome_text_dim));
        0.0
    } else {
        r.text(query, false, input.x + pad, input.y + text_dy, text_width, color(palette.foreground))
    };
    r.rect(input.x + pad + typed, input.y + text_dy, line * 2.0, metrics.height, color(palette.accent));
    r.rect(input.x, input.y + input.h - line, input.w, line, color(palette.border));

    for (row, command) in layout.rows.iter().zip(commands.iter().skip(layout.first)) {
        let index = layout.first + (row.y - layout.rows[0].y).div_euclid(row.h) as usize;
        let (_, name, keys) = **command;
        if index == selected {
            r.rect(row.x, row.y, row.w, row.h, color(palette.selection));
        }
        let keys_width = r.text_width(keys);
        r.text(name, false, row.x + pad, row.y + text_dy, row.w - pad * 3.0 - keys_width, color(palette.foreground));
        r.text(keys, false, row.x + row.w - pad - keys_width, row.y + text_dy, keys_width, color(palette.chrome_text_dim));
    }
}

/// The search bar, in the top-right corner of the pane being searched.
pub fn draw_search(r: &mut Renderer, query: &str, failed: bool, pane: Rect, scale: f32, palette: &Palette) {
    let metrics = r.metrics();
    let line = scale.round().max(1.0);
    let pad = (10.0 * scale).round();
    let label = "Find: ";
    let width = (metrics.width * 40.0 + pad * 2.0).min(pane.w - pad * 2.0).max(metrics.width * 8.0);
    let height = (metrics.height + 12.0 * scale).round();
    let bar = Rect::new(pane.x + pane.w - width - pad, pane.y + pad, width, height);
    let text_y = bar.y + ((bar.h - metrics.height) / 2.0).round();

    r.rect(bar.x, bar.y, bar.w, bar.h, color(palette.chrome));
    outline(r, bar.inflate(line), line, color(if failed { palette.ansi[1] } else { palette.border }));

    let mut x = bar.x + pad;
    x += r.text(label, false, x, text_y, bar.w - pad * 2.0, color(palette.chrome_text_dim));
    // Show the end of a query that is too long for the bar.
    let room = ((bar.x + bar.w - pad - x) / metrics.width).floor().max(1.0) as usize - 1;
    let count = query.chars().count();
    let visible: String = query.chars().skip(count.saturating_sub(room)).collect();
    let text_color = if failed { palette.ansi[1] } else { palette.foreground };
    x += r.text(&visible, false, x, text_y, bar.x + bar.w - pad - x, color(text_color));
    r.rect(x, text_y, line * 2.0, metrics.height, color(palette.accent));
}

/// Escapes regex metacharacters so `text` is searched for literally.
pub fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if matches!(c, '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '#' | '&' | '-' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_scrolls_to_keep_the_selection_visible() {
        let layout = PaletteLayout::new((1000.0, 800.0), (8.0, 16.0), 1.0, 30, 0);
        assert_eq!((layout.first, layout.rows.len()), (0, PALETTE_ROWS));
        let layout = PaletteLayout::new((1000.0, 800.0), (8.0, 16.0), 1.0, 30, 20);
        assert_eq!(layout.first, 9);
        let layout = PaletteLayout::new((1000.0, 800.0), (8.0, 16.0), 1.0, 3, 2);
        assert_eq!((layout.first, layout.rows.len()), (0, 3));
        // Clicking the second visible row picks the right command.
        let layout = PaletteLayout::new((1000.0, 800.0), (8.0, 16.0), 1.0, 30, 20);
        let row = layout.rows[1];
        assert_eq!(layout.row_at(row.x + 5.0, row.y + 5.0), Some(10));
        assert_eq!(layout.row_at(0.0, 0.0), None);
    }

    #[test]
    fn regex_escape_makes_text_literal() {
        assert_eq!(regex_escape("a.b*(c)"), "a\\.b\\*\\(c\\)");
        assert_eq!(regex_escape("plain text"), "plain text");
    }
}
