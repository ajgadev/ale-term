//! The docked board: in the board layout, every agent as a card in a strip
//! across the top of the window, with the tabs and panes below it.
//!
//! It shows the same columns as the board overlay (see [`crate::board`]),
//! but a fixed number of card rows, so its height never changes as agents
//! come and go and the terminals below are not resized. A column with more
//! cards than that scrolls on its own.
//!
//! As elsewhere, the app supplies plain data (a [`Model`]) and this module
//! lays it out, draws it and answers what is under the mouse, from one
//! layout function so the two cannot disagree.

use ale_agents::AgentState;
use ale_config::Palette;
use ale_render::Renderer;

use crate::board::{COLUMNS, Card, TITLES};
use crate::draw::color;
use crate::layout::Rect;
use crate::sidebar::{Target, state_color};

/// Sizes in points; multiplied by the display scale factor when used.
const HEADER: f32 = 38.0;
/// Room left for the macOS close/minimize/zoom buttons.
const TRAFFIC_LIGHTS: f32 = 78.0;
const PAD: f32 = 12.0;
const GAP: f32 = 4.0;
const BUTTON: f32 = 24.0;

pub const MIN_ROWS: usize = 1;
pub const MAX_ROWS: usize = 6;

pub struct Model {
    pub columns: [Vec<Card>; COLUMNS],
    /// The agent in the focused pane, drawn outlined.
    pub current: Option<Target>,
    /// Card rows shown per column.
    pub rows: usize,
    /// Whether there is room for another row (the "+" works).
    pub can_grow: bool,
    /// How many cards each column is scrolled past.
    pub scroll: [usize; COLUMNS],
    /// The line in the header, e.g. "5 agents in 2 workspaces".
    pub summary: String,
    pub hovered: Option<Hit>,
}

/// What a point on the strip is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Card(Target),
    /// The "−" and "+" beside the row count.
    FewerRows,
    MoreRows,
    /// Space in a column without a card: the wheel scrolls it.
    Column(usize),
    /// The header's empty space: drags the window.
    TitleBar,
}

/// The strip's height for a given number of card rows.
pub fn height(cell: (f32, f32), scale: f32, rows: usize) -> f32 {
    let Sizes { header, pad, gap, column_header, card, footer } = Sizes::new(cell, scale);
    let rows = rows.clamp(MIN_ROWS, MAX_ROWS) as f32;
    header + column_header + gap * 2.0 + rows * card + (rows - 1.0) * gap * 2.0 + gap * 2.0 + footer + pad
}

/// The most a column can be scrolled.
pub fn max_scroll(count: usize, rows: usize) -> usize {
    count.saturating_sub(rows)
}

struct Sizes {
    header: f32,
    pad: f32,
    gap: f32,
    column_header: f32,
    card: f32,
    footer: f32,
}

impl Sizes {
    fn new(cell: (f32, f32), scale: f32) -> Self {
        let gap = (GAP * scale).round();
        Sizes {
            header: (HEADER * scale).round(),
            pad: (PAD * scale).round(),
            gap,
            column_header: (cell.1 + gap * 3.0).round(),
            card: (cell.1 * 3.0 + gap * 5.0).round(),
            footer: (cell.1 + gap * 2.0).round(),
        }
    }
}

struct Placed {
    header: Rect,
    /// The "−", the row count and the "+".
    fewer: Rect,
    count: Rect,
    more: Rect,
    /// Each column: its whole area, its title row and its overflow bar.
    columns: [(Rect, Rect, Rect); COLUMNS],
    /// Visible cards: rectangle, column, position in the column.
    cards: Vec<(Rect, usize, usize)>,
}

/// Lays the strip out in `area`.
fn place(model: &Model, area: Rect, cell: (f32, f32), scale: f32) -> Placed {
    let sizes = Sizes::new(cell, scale);
    let Sizes { header: header_height, pad, gap, column_header, card, footer } = sizes;
    let header = Rect::new(area.x, area.y, area.w, header_height);

    // Row count control, right-aligned in the header.
    let button = (BUTTON * scale).round();
    let button_y = header.y + ((header.h - button) / 2.0).round();
    let count_width = (cell.0 * 7.0).round();
    let more = Rect::new(area.x + area.w - pad - button, button_y, button, button);
    let count = Rect::new(more.x - count_width, button_y, count_width, button);
    let fewer = Rect::new(count.x - button, button_y, button, button);

    let rows = model.rows.clamp(MIN_ROWS, MAX_ROWS);
    let width = ((area.w - pad * (COLUMNS as f32 + 1.0)) / COLUMNS as f32).floor().max(cell.0 * 8.0);
    let top = header.y + header.h;
    let cards_top = top + column_header + gap * 2.0;
    let cards_height = rows as f32 * card + (rows as f32 - 1.0) * gap * 2.0;
    let footer_y = cards_top + cards_height + gap * 2.0;

    let mut cards = Vec::new();
    let columns = std::array::from_fn(|column| {
        let x = area.x + pad + column as f32 * (width + pad);
        let whole = Rect::new(x, top, width, footer_y + footer - top);
        let title = Rect::new(x, top, width, column_header);
        let overflow = Rect::new(x, footer_y, width, footer);
        let first = model.scroll[column].min(max_scroll(model.columns[column].len(), rows));
        for (slot, index) in (first..model.columns[column].len()).take(rows).enumerate() {
            let y = cards_top + slot as f32 * (card + gap * 2.0);
            cards.push((Rect::new(x, y, width, card), column, index));
        }
        (whole, title, overflow)
    });
    Placed { header, fewer, count, more, columns, cards }
}

/// What is under a window position.
pub fn hit(model: &Model, area: Rect, cell: (f32, f32), scale: f32, x: f32, y: f32) -> Option<Hit> {
    if !area.contains(x, y) {
        return None;
    }
    let placed = place(model, area, cell, scale);
    if let Some((_, column, index)) = placed.cards.iter().find(|(rect, ..)| rect.contains(x, y)) {
        return Some(Hit::Card(model.columns[*column][*index].target));
    }
    if placed.fewer.contains(x, y) {
        return Some(Hit::FewerRows);
    }
    if placed.more.contains(x, y) {
        return Some(Hit::MoreRows);
    }
    if placed.header.contains(x, y) {
        return Some(Hit::TitleBar);
    }
    let column = placed.columns.iter().position(|(whole, ..)| x >= whole.x && x < whole.x + whole.w);
    Some(column.map_or(Hit::TitleBar, Hit::Column))
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

fn outline(r: &mut Renderer, rect: Rect, line: f32, ink: ale_render::Color) {
    r.rect(rect.x, rect.y, rect.w, line, ink);
    r.rect(rect.x, rect.y + rect.h - line, rect.w, line, ink);
    r.rect(rect.x, rect.y, line, rect.h, ink);
    r.rect(rect.x + rect.w - line, rect.y, line, rect.h, ink);
}

/// Text centered in `rect`.
fn centered(r: &mut Renderer, text: &str, bold: bool, rect: Rect, ink: ale_render::Color) {
    let metrics = r.metrics();
    let width = r.text_width(text).min(rect.w);
    let x = rect.x + ((rect.w - width) / 2.0).round();
    let y = rect.y + ((rect.h - metrics.height) / 2.0).round();
    r.text(text, bold, x, y, width, ink);
}

pub fn draw(r: &mut Renderer, model: &Model, area: Rect, scale: f32, lights: bool, palette: &Palette) {
    let metrics = r.metrics();
    let cell = (metrics.width, metrics.height);
    let line = scale.round().max(1.0);
    let gap = (GAP * scale).round();
    let pad = (PAD * scale).round();
    let dim = color(palette.chrome_text_dim);
    let fg = color(palette.foreground);
    let border = color(palette.border);
    let placed = place(model, area, cell, scale);

    r.rect(area.x, area.y, area.w, area.h, color(palette.background));

    // Header: what is on the board, and how many rows it shows.
    let header = placed.header;
    r.rect(header.x, header.y, header.w, header.h, color(palette.chrome));
    r.rect(header.x, header.y + header.h - line, header.w, line, border);
    let text_y = header.y + ((header.h - metrics.height) / 2.0).round();
    let left = header.x + if lights { (TRAFFIC_LIGHTS * scale).round() } else { pad };
    let used = r.text("Agents", true, left, text_y, header.w, color(palette.accent));
    let summary_room = placed.fewer.x - pad - (left + used + cell.0 * 2.0);
    if summary_room > cell.0 * 4.0 {
        let summary = fit(&model.summary, summary_room, cell.0);
        r.text(&summary, false, left + used + cell.0 * 2.0, text_y, summary_room, dim);
    }
    let rows = model.rows.clamp(MIN_ROWS, MAX_ROWS);
    for (rect, glyph, hit, enabled) in [
        (placed.fewer, "−", Hit::FewerRows, rows > MIN_ROWS),
        (placed.more, "+", Hit::MoreRows, model.can_grow && rows < MAX_ROWS),
    ] {
        if enabled && model.hovered == Some(hit) {
            r.rect(rect.x, rect.y, rect.w, rect.h, border);
        }
        centered(r, glyph, false, rect, if enabled { fg } else { border });
    }
    let row_label = if rows == 1 { "1 row".to_owned() } else { format!("{rows} rows") };
    centered(r, &row_label, false, placed.count, dim);
    outline(r, Rect::new(placed.fewer.x, placed.fewer.y, placed.more.x + placed.more.w - placed.fewer.x, placed.fewer.h), line, border);

    // Column titles, each with its count.
    let header_states = [AgentState::NeedsYou, AgentState::Working, AgentState::Done, AgentState::Idle];
    for (column, (_, title, overflow)) in placed.columns.iter().enumerate() {
        let title_y = title.y + ((title.h - metrics.height) / 2.0).round();
        let radius = (3.5 * scale).round();
        let dot = color(state_color(header_states[column], palette));
        crate::draw::dot(r, title.x + radius + line, title.y + (title.h / 2.0).round(), radius, dot);
        let text_x = title.x + radius * 2.0 + cell.0;
        let used = r.text(TITLES[column], true, text_x, title_y, title.w, fg);
        let count = model.columns[column].len();
        let count_text = count.to_string();
        let badge_width = r.text_width(&count_text) + cell.0;
        let badge = Rect::new(text_x + used + cell.0, title_y - gap / 2.0, badge_width, metrics.height + gap);
        r.rect(badge.x, badge.y, badge.w, badge.h, color(palette.chrome));
        centered(r, &count_text, false, badge, if count > 0 { dot } else { dim });

        // Below a column with more cards than rows: how many, and which
        // way there is more to see.
        if count > rows {
            let first = model.scroll[column].min(max_scroll(count, rows));
            let arrows = match (first > 0, first + rows < count) {
                (true, true) => "↕",
                (true, false) => "↑",
                _ => "↓",
            };
            let [ar, ag, ab, _] = color(palette.accent);
            r.rect(overflow.x, overflow.y, overflow.w, overflow.h, [ar, ag, ab, 40]);
            let text = format!("{count} cards · scroll to see all {arrows}");
            centered(r, &fit(&text, overflow.w, cell.0), false, *overflow, color(palette.accent));
        }
    }

    for (rect, column, index) in &placed.cards {
        let card = &model.columns[*column][*index];
        let rect = *rect;
        let current = model.current == Some(card.target);
        let hovered = model.hovered == Some(Hit::Card(card.target));
        r.rect(rect.x, rect.y, rect.w, rect.h, color(if hovered { palette.chrome_active } else { palette.chrome }));
        outline(r, rect, line, color(if current { palette.accent } else { palette.border }));
        if current {
            outline(r, Rect::new(rect.x + line, rect.y + line, rect.w - line * 2.0, rect.h - line * 2.0), line, color(palette.accent));
        }
        // The left edge carries the state's color.
        let state = color(state_color(card.state, palette));
        r.rect(rect.x, rect.y, line * 3.0, rect.h, state);

        let inner_x = rect.x + line * 3.0 + gap * 3.0;
        let inner_width = rect.x + rect.w - gap * 3.0 - inner_x;
        let first = rect.y + gap * 1.5;
        let second = first + metrics.height + gap;
        let third = second + metrics.height + gap;

        // Agent icon and name, with the time in this state at the right.
        let size = metrics.height.round();
        r.image(card.icon, inner_x, first, size, size, [0.0, 0.0, 1.0, 1.0]);
        let name_x = inner_x + size + (cell.0 * 0.75).round();
        let elapsed_width = r.text_width(&card.elapsed);
        let name_room = inner_x + inner_width - elapsed_width - cell.0 - name_x;
        r.text(&fit(&card.agent, name_room, cell.0), false, name_x, first, name_room.max(0.0), dim);
        r.text(&card.elapsed, false, inner_x + inner_width - elapsed_width, first, elapsed_width, dim);
        r.text(&fit(&card.title, inner_width, cell.0), true, inner_x, second, inner_width, fg);

        // The workspace, then what it is doing or waiting for.
        let project = fit(&card.project, (inner_width * 0.4).max(cell.0 * 4.0), cell.0);
        let used = r.text(&project, false, inner_x, third, inner_width, dim);
        let status_x = inner_x + used + cell.0 * 2.0;
        let status_room = inner_x + inner_width - status_x;
        if status_room > cell.0 * 3.0 {
            let status_color = match card.state {
                AgentState::Working | AgentState::Idle => dim,
                _ => state,
            };
            r.text(&fit(&card.status, status_room, cell.0), false, status_x, third, status_room, status_color);
        }
    }

    if model.columns.iter().all(Vec::is_empty) {
        let (first, _, _) = placed.columns[0];
        let (last, _, _) = placed.columns[COLUMNS - 1];
        let cards_area = Rect::new(first.x, first.y + metrics.height * 2.0, last.x + last.w - first.x, first.h - metrics.height * 2.0);
        centered(r, "No agents are running. Start one with Shift+Cmd+T.", false, cards_area, dim);
    }

    r.rect(area.x, area.y + area.h - line, area.w, line, border);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::column_of;

    const AREA_WIDTH: f32 = 1200.0;
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

    fn model(rows: usize) -> Model {
        let mut columns: [Vec<Card>; COLUMNS] = Default::default();
        for (pane, state) in [
            (1, AgentState::NeedsYou),
            (2, AgentState::Working),
            (3, AgentState::Working),
            (4, AgentState::Working),
            (5, AgentState::Idle),
        ] {
            columns[column_of(state)].push(card(pane, state));
        }
        Model { columns, current: None, rows, can_grow: true, scroll: [0; COLUMNS], summary: String::new(), hovered: None }
    }

    fn area(rows: usize) -> Rect {
        Rect::new(0.0, 0.0, AREA_WIDTH, height(CELL, 1.0, rows))
    }

    #[test]
    fn only_the_set_number_of_rows_is_shown_and_all_of_it_fits() {
        for rows in MIN_ROWS..=MAX_ROWS {
            let model = model(rows);
            let placed = place(&model, area(rows), CELL, 1.0);
            let in_working = placed.cards.iter().filter(|(_, column, _)| *column == 1).count();
            assert_eq!(in_working, rows.min(3));
            let bottom = area(rows).y + area(rows).h;
            for (rect, ..) in &placed.cards {
                assert!(rect.y + rect.h <= bottom && rect.x + rect.w <= AREA_WIDTH);
            }
            for (whole, _, overflow) in &placed.columns {
                assert!(overflow.y + overflow.h <= bottom && whole.y + whole.h <= bottom);
            }
        }
        // More rows make a taller strip; the height does not depend on the
        // cards, so the terminals below keep their size.
        assert!(height(CELL, 1.0, 3) > height(CELL, 1.0, 2));
        assert_eq!(height(CELL, 1.0, 0), height(CELL, 1.0, MIN_ROWS));
    }

    #[test]
    fn cards_are_clickable_where_they_are_drawn() {
        let model = model(2);
        let placed = place(&model, area(2), CELL, 1.0);
        for (rect, column, index) in &placed.cards {
            let expected = model.columns[*column][*index].target;
            assert_eq!(hit(&model, area(2), CELL, 1.0, rect.x + 5.0, rect.y + 5.0), Some(Hit::Card(expected)));
        }
        let (fewer, more) = (placed.fewer, placed.more);
        assert_eq!(hit(&model, area(2), CELL, 1.0, fewer.x + 2.0, fewer.y + 2.0), Some(Hit::FewerRows));
        assert_eq!(hit(&model, area(2), CELL, 1.0, more.x + 2.0, more.y + 2.0), Some(Hit::MoreRows));
        assert_eq!(hit(&model, area(2), CELL, 1.0, 100.0, 10.0), Some(Hit::TitleBar));
        // Below the last card of the "Done" column, which is empty.
        let (done, ..) = placed.columns[2];
        assert_eq!(hit(&model, area(2), CELL, 1.0, done.x + 5.0, done.y + done.h - 5.0), Some(Hit::Column(2)));
        assert_eq!(hit(&model, area(2), CELL, 1.0, 5.0, area(2).h + 5.0), None);
    }

    #[test]
    fn a_scrolled_column_shows_later_cards() {
        let mut model = model(1);
        model.scroll[1] = 2;
        let placed = place(&model, area(1), CELL, 1.0);
        let working: Vec<usize> = placed.cards.iter().filter(|c| c.1 == 1).map(|c| c.2).collect();
        assert_eq!(working, [2]);
        // Scrolling past the end is clamped.
        model.scroll[1] = 10;
        let placed = place(&model, area(1), CELL, 1.0);
        let working: Vec<usize> = placed.cards.iter().filter(|c| c.1 == 1).map(|c| c.2).collect();
        assert_eq!(working, [2]);
        assert_eq!(max_scroll(3, 1), 2);
        assert_eq!(max_scroll(1, 2), 0);
    }
}
