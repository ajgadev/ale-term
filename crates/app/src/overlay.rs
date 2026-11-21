//! Panels drawn on top of the terminal: the command palette and the search
//! bar. This module holds their state, layout and drawing; the app decides
//! what happens when a command is chosen or the query changes.

use std::path::PathBuf;

use ale_config::{Palette, TabTemplate};
use ale_render::Renderer;

use crate::draw::color;
use crate::layout::{PaneId, Rect};

/// What a list overlay is choosing from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKind {
    /// The command palette.
    Commands,
    /// Open workspaces and workspace templates.
    Workspaces,
    /// Destination for the current tab.
    MoveTab,
    /// Ended agent sessions that can be resumed.
    Archive,
}

impl ListKind {
    pub fn placeholder(self) -> &'static str {
        match self {
            ListKind::Commands => "Type a command",
            ListKind::Workspaces => "Switch to a workspace",
            ListKind::MoveTab => "Move tab to workspace",
            ListKind::Archive => "Resume an archived session",
        }
    }
}

/// What a one-line text prompt is asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    RenameTab,
    RenameWorkspace,
    NewWorkspace,
}

impl InputKind {
    pub fn placeholder(self) -> &'static str {
        match self {
            InputKind::RenameTab => "Tab title (empty for automatic)",
            InputKind::RenameWorkspace => "Workspace name",
            InputKind::NewWorkspace => "Name for the new workspace",
        }
    }
}

/// A yes/no question with some detail lines.
pub struct Confirm {
    pub title: String,
    pub lines: Vec<String>,
    /// What Enter will do, e.g. "Enter: run  ·  Esc: cancel".
    pub hint: String,
    /// The per-project template awaiting approval: project root, the file's
    /// contents and the tabs it defines. `None` for purely informational
    /// messages.
    pub template: Option<(PathBuf, String, Vec<TabTemplate>)>,
    /// Accepting installs the agent status hooks; dismissing declines them.
    pub install_hooks: bool,
}

pub enum Overlay {
    None,
    Palette {
        kind: ListKind,
        query: String,
        /// Index into the filtered list.
        selected: usize,
    },
    Input {
        kind: InputKind,
        query: String,
    },
    Confirm(Box<Confirm>),
    /// The settings screen: which setting is selected and, while a text
    /// setting is being typed, the text so far.
    Settings {
        selected: usize,
        editing: Option<String>,
    },
    /// A dropdown menu under a button.
    Menu(Box<crate::menu::Menu>),
    /// The board of agents, with the selected card's column and row.
    Board {
        selected: (usize, usize),
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

    /// True for overlays that cover the window and take every click.
    pub fn is_modal(&self) -> bool {
        matches!(
            self,
            Overlay::Palette { .. }
                | Overlay::Input { .. }
                | Overlay::Confirm(_)
                | Overlay::Settings { .. }
                | Overlay::Menu(_)
        )
    }

    /// The text being edited, if any.
    pub fn query_mut(&mut self) -> Option<&mut String> {
        match self {
            Overlay::Palette { query, .. } | Overlay::Input { query, .. } | Overlay::Search { query, .. } => {
                Some(query)
            }
            Overlay::Settings { editing, .. } => editing.as_mut(),
            Overlay::None | Overlay::Confirm(_) | Overlay::Board { .. } | Overlay::Menu(_) => None,
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

/// Draws a list overlay: an input line and the rows matching it. Each row
/// is a title and a right-aligned detail. With no rows it is a plain text
/// prompt.
#[allow(clippy::too_many_arguments)]
pub fn draw_palette(
    r: &mut Renderer,
    placeholder: &str,
    query: &str,
    selected: usize,
    rows: &[(String, String)],
    window: (f32, f32),
    scale: f32,
    palette: &Palette,
) {
    let metrics = r.metrics();
    let layout = PaletteLayout::new(window, (metrics.width, metrics.height), scale, rows.len(), selected);
    let line = scale.round().max(1.0);
    let pad = (14.0 * scale).round();
    let text_dy = ((layout.input.h - metrics.height) / 2.0).round();

    dim_background(r, window, palette);
    let frame = layout.frame;
    r.rect(frame.x, frame.y, frame.w, frame.h, color(palette.chrome));
    outline(r, frame.inflate(line), line, color(palette.border));

    // Input line with a caret.
    let input = layout.input;
    let text_width = input.w - pad * 2.0;
    let typed = if query.is_empty() {
        r.text(placeholder, false, input.x + pad, input.y + text_dy, text_width, color(palette.chrome_text_dim));
        0.0
    } else {
        r.text(query, false, input.x + pad, input.y + text_dy, text_width, color(palette.foreground))
    };
    r.rect(input.x + pad + typed, input.y + text_dy, line * 2.0, metrics.height, color(palette.accent));
    if !layout.rows.is_empty() {
        r.rect(input.x, input.y + input.h - line, input.w, line, color(palette.border));
    }

    for (offset, (row, (title, detail))) in layout.rows.iter().zip(rows.iter().skip(layout.first)).enumerate() {
        if layout.first + offset == selected {
            r.rect(row.x, row.y, row.w, row.h, color(palette.selection));
        }
        // The detail gives way to the title when space is short.
        let title_width = r.text_width(title);
        let detail_room = (row.w - pad * 3.0 - title_width).max(0.0);
        let detail = fit_end(detail, detail_room, metrics.width);
        let detail_width = r.text_width(&detail);
        r.text(title, false, row.x + pad, row.y + text_dy, row.w - pad * 2.0, color(palette.foreground));
        r.text(&detail, false, row.x + row.w - pad - detail_width, row.y + text_dy, detail_width, color(palette.chrome_text_dim));
    }
}

/// Keeps the end of `text` (the interesting part of a path) when it does
/// not fit in `width` pixels.
fn fit_end(text: &str, width: f32, cell_width: f32) -> String {
    let room = (width / cell_width).floor() as usize;
    let count = text.chars().count();
    if count <= room {
        return text.to_owned();
    }
    if room < 2 {
        return String::new();
    }
    let tail: String = text.chars().skip(count - (room - 1)).collect();
    format!("…{tail}")
}

fn dim_background(r: &mut Renderer, window: (f32, f32), palette: &Palette) {
    let [red, green, blue, _] = color(palette.background);
    r.rect(0.0, 0.0, window.0, window.1, [red, green, blue, 110]);
}

pub fn draw_confirm(r: &mut Renderer, confirm: &Confirm, window: (f32, f32), scale: f32, palette: &Palette) {
    let metrics = r.metrics();
    let line = scale.round().max(1.0);
    let pad = (18.0 * scale).round();
    let row = (metrics.height + 6.0 * scale).round();
    let width = (640.0 * scale).min(window.0 - 32.0 * scale).max(metrics.width * 10.0).round();
    // Title, a gap, the lines, a gap, the hint.
    let height = pad * 2.0 + row * (confirm.lines.len() as f32 + 2.0) + pad;
    let frame = Rect::new(((window.0 - width) / 2.0).round(), (window.1 * 0.16).round(), width, height);
    let text_width = frame.w - pad * 2.0;

    dim_background(r, window, palette);
    r.rect(frame.x, frame.y, frame.w, frame.h, color(palette.chrome));
    outline(r, frame.inflate(line), line, color(palette.border));

    let mut y = frame.y + pad;
    r.text(&confirm.title, true, frame.x + pad, y, text_width, color(palette.foreground));
    y += row + pad / 2.0;
    for text in &confirm.lines {
        r.text(text, false, frame.x + pad, y, text_width, color(palette.foreground));
        y += row;
    }
    y += pad / 2.0;
    r.text(&confirm.hint, false, frame.x + pad, y, text_width, color(palette.chrome_text_dim));
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
    fn long_details_keep_their_end() {
        assert_eq!(fit_end("~/Dev/project", 200.0, 8.0), "~/Dev/project");
        assert_eq!(fit_end("~/Dev/project", 64.0, 8.0), "…project");
        assert_eq!(fit_end("~/Dev/project", 8.0, 8.0), "");
    }

    #[test]
    fn regex_escape_makes_text_literal() {
        assert_eq!(regex_escape("a.b*(c)"), "a\\.b\\*\\(c\\)");
        assert_eq!(regex_escape("plain text"), "plain text");
    }
}
