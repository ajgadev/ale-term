//! The board: every agent as a card, in columns by what it needs.
//!
//! Like the sidebar, the app supplies plain data (a [`Model`]) and this
//! module lays it out, draws it and answers what is under the mouse.

use ale_agents::AgentState;
use ale_config::Palette;
use ale_render::Renderer;

use crate::draw::color;
use crate::layout::Rect;
use crate::sidebar::{Target, state_color};

pub const COLUMNS: usize = 4;

/// Column titles, left to right.
pub const TITLES: [&str; COLUMNS] = ["Needs you", "Working", "Done", "Idle"];

/// Which column an agent in `state` belongs to. Errors and rate limits
/// need the user as much as a prompt does; an agent waiting on its
/// subagents is still at work.
pub fn column_of(state: AgentState) -> usize {
    match state {
        AgentState::NeedsYou | AgentState::Error | AgentState::RateLimited => 0,
        AgentState::Working | AgentState::Waiting => 1,
        AgentState::Done => 2,
        AgentState::Idle => 3,
    }
}

pub struct Card {
    pub target: Target,
    pub state: AgentState,
    /// The agent's name, e.g. "Claude Code".
    pub agent: String,
    pub title: String,
    /// The workspace it runs in.
    pub project: String,
    /// What it is doing or waiting for.
    pub status: String,
    pub elapsed: String,
    /// Image key of the agent's icon (see `icons`).
    pub icon: u64,
}

#[derive(Default)]
pub struct Model {
    pub columns: [Vec<Card>; COLUMNS],
    /// The card picked with the keyboard: column and position in it.
    pub selected: (usize, usize),
}

impl Model {
    pub fn is_empty(&self) -> bool {
        self.columns.iter().all(Vec::is_empty)
    }

    pub fn selected_card(&self) -> Option<&Card> {
        self.columns.get(self.selected.0)?.get(self.selected.1)
    }
}

/// Moves the selection by one step. Left and right skip empty columns and
/// keep the row as close as the new column allows. Takes the number of
/// cards per column; returns the new (column, row).
pub fn step(counts: [usize; COLUMNS], selected: (usize, usize), dx: isize, dy: isize) -> (usize, usize) {
    let (mut column, mut row) = selected;
    // Start from a real card if the current position is empty.
    if counts.get(column).copied().unwrap_or(0) == 0 {
        match (0..COLUMNS).find(|c| counts[*c] > 0) {
            Some(first) => (column, row) = (first, 0),
            None => return (0, 0),
        }
    }
    if dx != 0 {
        let mut next = column as isize + dx;
        while (0..COLUMNS as isize).contains(&next) && counts[next as usize] == 0 {
            next += dx;
        }
        if (0..COLUMNS as isize).contains(&next) {
            column = next as usize;
        }
    }
    let last = counts[column].saturating_sub(1);
    row = (row as isize + dy).clamp(0, last as isize) as usize;
    (column, row.min(last))
}

/// Sizes in points.
const PAD: f32 = 16.0;
const GAP: f32 = 4.0;

struct CardBox {
    rect: Rect,
    column: usize,
    row: usize,
}

/// Places the column headers and the cards that fit.
fn layout(model: &Model, area: Rect, cell: (f32, f32), scale: f32) -> (Vec<Rect>, Vec<CardBox>) {
    let pad = (PAD * scale).round();
    let gap = (GAP * scale).round();
    let column_width = ((area.w - pad * (COLUMNS as f32 + 1.0)) / COLUMNS as f32).floor().max(cell.0 * 8.0);
    let header_height = cell.1 + gap * 4.0;
    let card_height = cell.1 * 3.0 + gap * 6.0;

    let mut headers = Vec::new();
    let mut cards = Vec::new();
    for (column, column_cards) in model.columns.iter().enumerate() {
        let x = area.x + pad + column as f32 * (column_width + pad);
        headers.push(Rect::new(x, area.y + pad, column_width, header_height));
        let mut y = area.y + pad + header_height + gap * 2.0;
        for row in 0..column_cards.len() {
            if y + card_height > area.y + area.h {
                break;
            }
            cards.push(CardBox { rect: Rect::new(x, y, column_width, card_height), column, row });
            y += card_height + gap * 3.0;
        }
    }
    (headers, cards)
}

/// The card under a window position.
pub fn hit(model: &Model, area: Rect, cell: (f32, f32), scale: f32, x: f32, y: f32) -> Option<Target> {
    let (_, cards) = layout(model, area, cell, scale);
    let found = cards.iter().find(|card| card.rect.contains(x, y))?;
    Some(model.columns[found.column][found.row].target)
}

fn fit(text: &str, width: f32, cell_width: f32) -> String {
    let room = (width / cell_width).floor().max(0.0) as usize;
    if text.chars().count() <= room {
        return text.to_owned();
    }
    if room == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(room - 1).collect();
    out.push('…');
    out
}

pub fn draw(r: &mut Renderer, model: &Model, area: Rect, scale: f32, palette: &Palette) {
    let metrics = r.metrics();
    let cell = (metrics.width, metrics.height);
    let gap = (GAP * scale).round();
    let line = scale.round().max(1.0);
    let dim = color(palette.chrome_text_dim);
    let fg = color(palette.foreground);

    r.rect(area.x, area.y, area.w, area.h, color(palette.background));
    let (headers, cards) = layout(model, area, cell, scale);

    let header_states = [AgentState::NeedsYou, AgentState::Working, AgentState::Done, AgentState::Idle];
    for (column, header) in headers.iter().enumerate() {
        let text_y = header.y + ((header.h - metrics.height) / 2.0).round();
        let dot = color(state_color(header_states[column], palette));
        r.text("●", false, header.x, text_y, cell.0 * 2.0, dot);
        let used = r.text(TITLES[column], true, header.x + cell.0 * 2.0, text_y, header.w, fg);
        let count = model.columns[column].len().to_string();
        r.text(&count, false, header.x + cell.0 * 3.0 + used, text_y, header.w, dim);
        r.rect(header.x, header.y + header.h - line, header.w, line, color(palette.border));
    }

    for card_box in &cards {
        let card = &model.columns[card_box.column][card_box.row];
        let rect = card_box.rect;
        let selected = model.selected == (card_box.column, card_box.row);
        r.rect(rect.x, rect.y, rect.w, rect.h, color(palette.chrome));
        let edge = if selected { palette.accent } else { palette.border };
        r.rect(rect.x, rect.y, rect.w, line, color(edge));
        r.rect(rect.x, rect.y + rect.h - line, rect.w, line, color(edge));
        r.rect(rect.x + rect.w - line, rect.y, line, rect.h, color(edge));
        // The left edge carries the state's color.
        r.rect(rect.x, rect.y, line * 3.0, rect.h, color(state_color(card.state, palette)));

        let inner_x = rect.x + gap * 3.0 + line * 3.0;
        let inner_width = rect.x + rect.w - gap * 3.0 - inner_x;
        let first = rect.y + gap * 1.5;
        let second = first + metrics.height + gap * 1.5;
        let third = second + metrics.height + gap * 1.5;

        let elapsed_width = r.text_width(&card.elapsed);
        r.text(&fit(&card.agent, inner_width - elapsed_width - cell.0, cell.0), false, inner_x, first, inner_width, dim);
        r.text(&card.elapsed, false, inner_x + inner_width - elapsed_width, first, elapsed_width, dim);
        r.text(&fit(&card.title, inner_width, cell.0), true, inner_x, second, inner_width, fg);
        let detail = format!("{} · {}", card.project, card.status);
        let detail_color =
            if card.state == AgentState::Working { dim } else { color(state_color(card.state, palette)) };
        r.text(&fit(&detail, inner_width, cell.0), false, inner_x, third, inner_width, detail_color);
    }

    if model.is_empty() {
        let message = "No agents are running. Start one with Shift+Cmd+T.";
        let width = r.text_width(message);
        let x = area.x + ((area.w - width) / 2.0).round().max(0.0);
        r.text(message, false, x, area.y + (area.h * 0.4).round(), area.w, dim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(260.0, 38.0, 1000.0, 600.0);
    const CELL: (f32, f32) = (8.0, 16.0);

    fn card(pane: u64, state: AgentState) -> Card {
        Card {
            target: Target { tab: 1, pane },
            state,
            agent: "Claude Code".into(),
            title: format!("task {pane}"),
            project: "shop".into(),
            status: "Editing".into(),
            elapsed: "3m".into(),
            icon: 0,
        }
    }

    fn model() -> Model {
        let mut model = Model::default();
        for (pane, state) in [
            (1, AgentState::NeedsYou),
            (2, AgentState::Working),
            (3, AgentState::Working),
            (4, AgentState::Error),
            (5, AgentState::Idle),
        ] {
            model.columns[column_of(state)].push(card(pane, state));
        }
        model
    }

    #[test]
    fn states_sort_into_the_right_columns() {
        let model = model();
        let panes = |column: usize| model.columns[column].iter().map(|c| c.target.pane).collect::<Vec<_>>();
        assert_eq!(panes(0), [1, 4]);
        assert_eq!(panes(1), [2, 3]);
        assert!(panes(2).is_empty());
        assert_eq!(panes(3), [5]);
        assert_eq!(column_of(AgentState::Waiting), 1);
        assert_eq!(column_of(AgentState::RateLimited), 0);
    }

    #[test]
    fn cards_are_clickable_where_they_are_drawn() {
        let model = model();
        let (headers, cards) = layout(&model, AREA, CELL, 1.0);
        assert_eq!((headers.len(), cards.len()), (COLUMNS, 5));
        // Columns share the width and stay inside the area.
        assert!(headers[3].x + headers[3].w <= AREA.x + AREA.w);
        for card_box in &cards {
            let (x, y) = (card_box.rect.x + 5.0, card_box.rect.y + 5.0);
            let expected = model.columns[card_box.column][card_box.row].target;
            assert_eq!(hit(&model, AREA, CELL, 1.0, x, y), Some(expected));
        }
        assert_eq!(hit(&model, AREA, CELL, 1.0, AREA.x + 2.0, AREA.y + 2.0), None);
    }

    #[test]
    fn cards_that_do_not_fit_are_left_out() {
        let mut model = Model::default();
        for pane in 0..40 {
            model.columns[1].push(card(pane, AgentState::Working));
        }
        let (_, cards) = layout(&model, AREA, CELL, 1.0);
        assert!(!cards.is_empty() && cards.len() < 40);
        assert!(cards.iter().all(|c| c.rect.y + c.rect.h <= AREA.y + AREA.h));
    }

    #[test]
    fn keyboard_selection_moves_between_cards() {
        let counts = [2, 2, 0, 1];
        assert_eq!(step(counts, (0, 0), 0, 1), (0, 1));
        assert_eq!(step(counts, (0, 1), 0, 1), (0, 1));
        assert_eq!(step(counts, (0, 1), 1, 0), (1, 1));
        // The empty "Done" column is skipped, and the row is clamped.
        assert_eq!(step(counts, (1, 1), 1, 0), (3, 0));
        assert_eq!(step(counts, (3, 0), 1, 0), (3, 0));
        assert_eq!(step(counts, (3, 0), -1, 0), (1, 0));
        assert_eq!(step(counts, (0, 0), -1, 0), (0, 0));
        // Starting on an empty column jumps to the first card.
        assert_eq!(step(counts, (2, 0), 0, 0), (0, 0));
        assert_eq!(step([0; COLUMNS], (1, 3), 1, 1), (0, 0));
    }
}
