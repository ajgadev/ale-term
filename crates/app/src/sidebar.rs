//! The sidebar: every agent and shell, grouped by project or by status.
//!
//! The app builds a [`Model`] (plain data: what to show); this module lays
//! it out, draws it and answers "what is under the mouse". Keeping layout
//! and hit-testing in one function means they cannot disagree.

use ale_agents::AgentState;
use ale_config::{Palette, Rgb};
use ale_render::Renderer;

use crate::draw::color;
use crate::layout::{PaneId, Rect};
use crate::workspace::TabId;

/// Which rows the sidebar shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    /// Agents that want the user or have finished.
    Waiting,
    Working,
}

impl Filter {
    pub fn matches(self, state: AgentState) -> bool {
        match self {
            Filter::All => true,
            Filter::Waiting => {
                matches!(state, AgentState::NeedsYou | AgentState::Done | AgentState::Error | AgentState::RateLimited)
            }
            Filter::Working => matches!(state, AgentState::Working | AgentState::WaitingOnSubagents),
        }
    }
}

/// The color that stands for a state, on tabs and in the sidebar.
pub fn state_color(state: AgentState, palette: &Palette) -> Rgb {
    match state {
        AgentState::NeedsYou => palette.accent,
        AgentState::Working => palette.ansi[4],
        AgentState::Done => palette.ansi[2],
        AgentState::Idle => palette.chrome_text_dim,
        AgentState::Error => palette.ansi[1],
        AgentState::RateLimited => palette.ansi[3],
        AgentState::WaitingOnSubagents => palette.ansi[5],
    }
}

/// Where a click on a row leads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub tab: TabId,
    pub pane: PaneId,
}

pub enum Row {
    Agent {
        target: Target,
        state: AgentState,
        title: String,
        /// Second line: what it is doing, or its state.
        status: String,
        /// The agent's name, shown at the end of the second line.
        agent: String,
        elapsed: String,
        /// This is the pane the user is looking at.
        current: bool,
        /// Image key of the agent's icon.
        icon: u64,
    },
    /// A subagent, listed under its agent.
    Subagent {
        target: Target,
        /// Position among the agent's subagents.
        index: usize,
        text: String,
        running: bool,
    },
    /// A plain shell or a running command.
    Shell { target: Target, text: String, current: bool },
}

pub struct Group {
    pub title: String,
    pub rows: Vec<Row>,
}

#[derive(Default)]
pub struct Model {
    pub groups: Vec<Group>,
    /// Agent counts for the filter chips: all, waiting, working.
    pub counts: [usize; 3],
    pub filter: Filter,
    pub by_status: bool,
    pub query: String,
    pub searching: bool,
    /// A hint shown at the bottom, e.g. that status hooks can be installed.
    pub notice: Option<String>,
    /// How many ended sessions are in the archive.
    pub archived: usize,
    /// What the pointer is over, drawn highlighted.
    pub hovered: Option<Hit>,
    /// The "New session" menu is open.
    pub menu_open: bool,
}

/// What a point in the sidebar is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The strip beside the traffic lights: drags the window.
    TitleBar,
    NewSession,
    Search,
    Filter(Filter),
    GroupToggle,
    Row(Target),
    /// A subagent row: its agent's pane and the subagent's position.
    Subagent(Target, usize),
    Archive,
    Notice,
    Nothing,
}

/// One thing to draw, with where it goes.
enum Item<'a> {
    NewSession,
    Archive(usize),
    Search,
    Chip(Filter, String),
    GroupToggle,
    Header(&'a str),
    Row(&'a Row),
    Notice(&'a str),
}

struct Placed<'a> {
    rect: Rect,
    item: Item<'a>,
}

/// Sizes in points.
const TOP: f32 = 38.0;
const PAD: f32 = 12.0;
const LINE_GAP: f32 = 4.0;

/// Places everything. `scroll` is how far the rows are scrolled, in pixels.
/// Returns the placed items and the total height of the rows (for clamping
/// the scroll).
fn layout<'a>(model: &'a Model, area: Rect, cell: (f32, f32), scale: f32, scroll: f32) -> (Vec<Placed<'a>>, f32) {
    let pad = (PAD * scale).round();
    let gap = (LINE_GAP * scale).round();
    let line = cell.1;
    let mut items = Vec::new();
    let mut y = area.y + (TOP * scale).round();

    // "New session" button.
    let button = new_session_button(area, cell, scale);
    items.push(Placed { rect: button, item: Item::NewSession });
    y += button.h + gap * 2.0;

    // Search box, with the grouping toggle to its right.
    let search_height = line + gap * 3.0;
    let toggle_width = cell.0 * 3.0;
    let search_width = area.w - pad * 2.0 - toggle_width - gap;
    items.push(Placed { rect: Rect::new(area.x + pad, y, search_width, search_height), item: Item::Search });
    items.push(Placed {
        rect: Rect::new(area.x + area.w - pad - toggle_width, y, toggle_width, search_height),
        item: Item::GroupToggle,
    });
    y += search_height + gap * 2.0;

    // Filter chips.
    let chip_height = line + gap * 2.0;
    let mut x = area.x + pad;
    let labels = ["All", "Waiting", "Working"];
    for (index, filter) in [Filter::All, Filter::Waiting, Filter::Working].into_iter().enumerate() {
        let label = format!("{} {}", labels[index], model.counts[index]);
        let width = label.chars().count() as f32 * cell.0 + gap * 2.0;
        items.push(Placed { rect: Rect::new(x, y, width, chip_height), item: Item::Chip(filter, label) });
        x += width + gap;
    }
    y += chip_height + gap * 3.0;

    // Rows scroll under the fixed header above; the archive row and any
    // notice are pinned below.
    let pinned_row = line + gap * 4.0;
    let notice_height = pinned_row + if model.notice.is_some() { pinned_row } else { 0.0 };
    let rows_top = y;
    let mut row_y = rows_top - scroll;
    for group in &model.groups {
        if group.rows.is_empty() {
            continue;
        }
        let header_height = line + gap * 3.0;
        items.push(Placed { rect: Rect::new(area.x, row_y, area.w, header_height), item: Item::Header(&group.title) });
        row_y += header_height;
        for row in &group.rows {
            let height = match row {
                Row::Agent { .. } => line * 2.0 + gap * 4.0,
                Row::Subagent { .. } | Row::Shell { .. } => line + gap * 2.0,
            };
            items.push(Placed { rect: Rect::new(area.x, row_y, area.w, height), item: Item::Row(row) });
            row_y += height;
        }
        row_y += gap * 2.0;
    }
    let content_height = row_y + scroll - rows_top;

    // Drop rows scrolled out of the visible band so they are neither drawn
    // nor clickable.
    let band = (rows_top, area.y + area.h - notice_height);
    let fixed = 6; // new session, search, toggle, three chips
    let mut index = 0;
    items.retain(|placed| {
        index += 1;
        index <= fixed || (placed.rect.y >= band.0 - 1.0 && placed.rect.y + placed.rect.h <= band.1 + 1.0)
    });

    let bottom = area.y + area.h;
    items.push(Placed {
        rect: Rect::new(area.x, bottom - pinned_row, area.w, pinned_row),
        item: Item::Archive(model.archived),
    });
    if let Some(notice) = &model.notice {
        items.push(Placed {
            rect: Rect::new(area.x, bottom - pinned_row * 2.0, area.w, pinned_row),
            item: Item::Notice(notice),
        });
    }
    (items, content_height)
}

/// Where the "New session" button is; its menu opens below it.
pub fn new_session_button(area: Rect, cell: (f32, f32), scale: f32) -> Rect {
    let pad = (PAD * scale).round();
    let gap = (LINE_GAP * scale).round();
    Rect::new(area.x + pad, area.y + (TOP * scale).round(), area.w - pad * 2.0, cell.1 + gap * 3.0)
}

/// The largest useful scroll offset for this model.
pub fn max_scroll(model: &Model, area: Rect, cell: (f32, f32), scale: f32) -> f32 {
    let (items, content) = layout(model, area, cell, scale, 0.0);
    let rows_top = items.get(5).map_or(area.y, |chip| chip.rect.y + chip.rect.h + (LINE_GAP * scale).round() * 3.0);
    let pinned_row = cell.1 + (LINE_GAP * scale).round() * 4.0;
    let notice = pinned_row + if model.notice.is_some() { pinned_row } else { 0.0 };
    (content - (area.y + area.h - notice - rows_top)).max(0.0)
}

pub fn hit(model: &Model, area: Rect, cell: (f32, f32), scale: f32, scroll: f32, x: f32, y: f32) -> Option<Hit> {
    if !area.contains(x, y) {
        return None;
    }
    if y < area.y + (TOP * scale).round() {
        return Some(Hit::TitleBar);
    }
    let (items, _) = layout(model, area, cell, scale, scroll);
    let found = items.iter().find(|placed| placed.rect.contains(x, y)).map(|placed| match &placed.item {
        Item::NewSession => Hit::NewSession,
        Item::Archive(_) => Hit::Archive,
        Item::Search => Hit::Search,
        Item::Chip(filter, _) => Hit::Filter(*filter),
        Item::GroupToggle => Hit::GroupToggle,
        Item::Row(Row::Agent { target, .. } | Row::Shell { target, .. }) => Hit::Row(*target),
        Item::Row(Row::Subagent { target, index, .. }) => Hit::Subagent(*target, *index),
        Item::Notice(_) => Hit::Notice,
        Item::Header(_) => Hit::Nothing,
    });
    Some(found.unwrap_or(Hit::Nothing))
}

/// Shortens `text` to `width` pixels with an ellipsis.
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

pub fn draw(r: &mut Renderer, model: &Model, area: Rect, scale: f32, scroll: f32, palette: &Palette) {
    let metrics = r.metrics();
    let cell = (metrics.width, metrics.height);
    let pad = (PAD * scale).round();
    let gap = (LINE_GAP * scale).round();
    let line = scale.round().max(1.0);
    let dim = color(palette.chrome_text_dim);
    let fg = color(palette.foreground);

    r.rect(area.x, area.y, area.w, area.h, color(palette.chrome));
    r.rect(area.x + area.w - line, area.y, line, area.h, color(palette.border));

    let (items, _) = layout(model, area, cell, scale, scroll);
    let hovered = |hit: Hit| model.hovered == Some(hit);
    // A faint wash behind a row under the pointer.
    let hover_wash = {
        let [red, green, blue, _] = color(palette.background);
        [red, green, blue, 140]
    };
    for placed in &items {
        let rect = placed.rect;
        let text_y = rect.y + ((rect.h - metrics.height) / 2.0).round();
        match &placed.item {
            Item::NewSession => {
                let lit = model.menu_open || hovered(Hit::NewSession);
                r.rect(rect.x, rect.y, rect.w, rect.h, color(if lit { palette.selection } else { palette.border }));
                let arrow = "▾";
                let arrow_width = r.text_width(arrow);
                r.text("+ New session", false, rect.x + gap * 2.0, text_y, rect.w - arrow_width - gap * 5.0, fg);
                r.text(arrow, false, rect.x + rect.w - arrow_width - gap * 2.0, text_y, arrow_width, if lit { fg } else { dim });
            }
            Item::Archive(count) => {
                if hovered(Hit::Archive) {
                    r.rect(rect.x, rect.y, rect.w - line, rect.h, hover_wash);
                }
                let text = if hovered(Hit::Archive) { fg } else { dim };
                r.rect(rect.x, rect.y, rect.w - line, line, color(palette.border));
                let count = count.to_string();
                let count_width = r.text_width(&count);
                r.text("Archived", false, rect.x + pad, text_y, rect.w, text);
                r.text(&count, false, rect.x + rect.w - pad - count_width, text_y, count_width, text);
            }
            Item::Search => {
                let border = if model.searching { palette.accent } else { palette.border };
                r.rect(rect.x, rect.y, rect.w, rect.h, color(palette.background));
                r.rect(rect.x, rect.y + rect.h - line, rect.w, line, color(border));
                let inner = rect.w - gap * 4.0;
                if model.query.is_empty() && !model.searching {
                    r.text("Search agents", false, rect.x + gap * 2.0, text_y, inner, dim);
                } else {
                    let shown = r.text(&model.query, false, rect.x + gap * 2.0, text_y, inner, fg);
                    if model.searching {
                        r.rect(rect.x + gap * 2.0 + shown, text_y, line * 2.0, metrics.height, color(palette.accent));
                    }
                }
            }
            Item::Chip(filter, label) => {
                let selected = *filter == model.filter;
                if selected {
                    r.rect(rect.x, rect.y, rect.w, rect.h, color(palette.border));
                }
                let lit = selected || hovered(Hit::Filter(*filter));
                r.text(label, selected, rect.x + gap, text_y, rect.w, if lit { fg } else { dim });
            }
            Item::GroupToggle => {
                // "≡" for by-project, "◐" for by-status.
                let glyph = if model.by_status { "◐" } else { "≡" };
                r.text(glyph, false, rect.x + cell.0, text_y, rect.w, if hovered(Hit::GroupToggle) { fg } else { dim });
            }
            Item::Header(title) => {
                let label = fit(title, rect.w - pad * 2.0, cell.0);
                r.text(&label, false, rect.x + pad, rect.y + rect.h - metrics.height - gap, rect.w, dim);
            }
            Item::Row(Row::Agent { target, state, title, status, agent, elapsed, current, icon }) => {
                if !*current && hovered(Hit::Row(*target)) {
                    r.rect(rect.x, rect.y, rect.w - line, rect.h, hover_wash);
                }
                if *current {
                    r.rect(rect.x, rect.y, rect.w - line, rect.h, color(palette.background));
                    r.rect(rect.x, rect.y, line * 2.0, rect.h, color(palette.accent));
                }
                let state_rgb = color(state_color(*state, palette));
                let first = rect.y + gap * 1.5;
                let second = first + metrics.height + gap;
                // The agent's icon, with a dot in its state's color.
                let size = metrics.height.round();
                let slot = size + (cell.0 * 0.75).round();
                let text_x = rect.x + pad + slot;
                let right = rect.x + rect.w - pad;

                r.image(*icon, rect.x + pad, first, size, size, [0.0, 0.0, 1.0, 1.0]);
                // A busy agent's icon spins instead of carrying a dot.
                if !matches!(state, AgentState::Working | AgentState::WaitingOnSubagents) {
                    let radius = (3.0 * scale).round();
                    let behind = if *current { palette.background } else { palette.chrome };
                    crate::draw::dot(r, rect.x + pad + size, first + radius, radius + line, color(behind));
                    crate::draw::dot(r, rect.x + pad + size, first + radius, radius, state_rgb);
                }
                let elapsed_width = r.text_width(elapsed);
                r.text(elapsed, false, right - elapsed_width, first, elapsed_width, dim);
                let title = fit(title, right - elapsed_width - cell.0 - text_x, cell.0);
                r.text(&title, true, text_x, first, rect.w, fg);

                // Working is the normal case and stays quiet; other states
                // take their color.
                let status_color = if *state == AgentState::Working { dim } else { state_rgb };
                let status = fit(status, right - text_x, cell.0);
                let used = r.text(&status, false, text_x, second, rect.w, status_color);
                // The agent's name goes at the end if the status leaves room.
                let agent_width = r.text_width(agent);
                if text_x + used + cell.0 * 2.0 + agent_width <= right {
                    r.text(agent, false, right - agent_width, second, agent_width, dim);
                }
            }
            Item::Row(Row::Subagent { target, index, text, running }) => {
                if hovered(Hit::Subagent(*target, *index)) {
                    r.rect(rect.x, rect.y, rect.w - line, rect.h, hover_wash);
                }
                let text_x = rect.x + pad + metrics.height.round() + (cell.0 * 0.75).round();
                let label = fit(&format!("↳ {text}"), rect.x + rect.w - pad - text_x, cell.0);
                r.text(&label, false, text_x, text_y, rect.w, if *running { fg } else { dim });
            }
            Item::Row(Row::Shell { target, text, current }) => {
                let lit = hovered(Hit::Row(*target));
                if !*current && lit {
                    r.rect(rect.x, rect.y, rect.w - line, rect.h, hover_wash);
                }
                if *current {
                    r.rect(rect.x, rect.y, rect.w - line, rect.h, color(palette.background));
                    r.rect(rect.x, rect.y, line * 2.0, rect.h, color(palette.accent));
                }
                let label = fit(text, rect.w - pad * 2.0 - cell.0 * 2.0, cell.0);
                r.text("›", false, rect.x + pad, text_y, cell.0 * 2.0, dim);
                r.text(&label, false, rect.x + pad + cell.0 * 2.0, text_y, rect.w, if *current || lit { fg } else { dim });
            }
            Item::Notice(text) => {
                r.rect(rect.x, rect.y, rect.w - line, line, color(palette.border));
                let label = fit(text, rect.w - pad * 2.0, cell.0);
                r.text(&label, false, rect.x + pad, text_y, rect.w, color(palette.accent));
            }
        }
    }
}

/// How long something has been going on, in the shortest readable form.
pub fn format_elapsed(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0.0, 0.0, 260.0, 600.0);
    const CELL: (f32, f32) = (8.0, 16.0);

    fn agent(tab: TabId, pane: PaneId) -> Row {
        Row::Agent {
            target: Target { tab, pane },
            state: AgentState::Working,
            title: "Fix login".into(),
            status: "Editing auth.rs".into(),
            agent: "Claude Code".into(),
            elapsed: "3m".into(),
            current: false,
            icon: 0,
        }
    }

    fn model(rows: usize) -> Model {
        Model {
            groups: vec![Group { title: "shop".into(), rows: (0..rows).map(|i| agent(1, i as u64)).collect() }],
            counts: [rows, 0, rows],
            ..Model::default()
        }
    }

    #[test]
    fn clicks_resolve_to_the_right_thing() {
        let model = model(3);
        let at = |x, y| hit(&model, AREA, CELL, 1.0, 0.0, x, y);
        assert_eq!(at(300.0, 100.0), None);
        assert_eq!(at(100.0, 10.0), Some(Hit::TitleBar));
        // The fixed controls are 36px lower than the title strip: button
        // (28px) plus a gap.
        assert_eq!(at(100.0, 45.0), Some(Hit::NewSession));
        assert_eq!(at(100.0, 81.0), Some(Hit::Search));
        assert_eq!(at(240.0, 81.0), Some(Hit::GroupToggle));
        assert_eq!(at(20.0, 118.0), Some(Hit::Filter(Filter::All)));
        assert_eq!(at(100.0, 118.0), Some(Hit::Filter(Filter::Waiting)));
        assert_eq!(at(180.0, 118.0), Some(Hit::Filter(Filter::Working)));
        // The archive row is pinned to the bottom.
        assert_eq!(at(50.0, 590.0), Some(Hit::Archive));
        // The chips fit inside the sidebar at its default width.
        let (placed, _) = layout(&model, AREA, CELL, 1.0, 0.0);
        assert!(placed.iter().take(6).all(|p| p.rect.x + p.rect.w <= AREA.w - 12.0 + 0.5));
        // Rows: header first, then one row per agent, 48px each.
        let (items, _) = layout(&model, AREA, CELL, 1.0, 0.0);
        let rows: Vec<&Placed> = items.iter().filter(|p| matches!(p.item, Item::Row(_))).collect();
        assert_eq!(rows.len(), 3);
        let second = rows[1].rect;
        assert_eq!(at(50.0, second.y + 5.0), Some(Hit::Row(Target { tab: 1, pane: 1 })));
        assert_eq!(at(50.0, 500.0), Some(Hit::Nothing));
    }

    #[test]
    fn long_lists_scroll_and_hidden_rows_are_not_clickable() {
        let model = model(40);
        let limit = max_scroll(&model, AREA, CELL, 1.0);
        assert!(limit > 0.0);
        assert_eq!(max_scroll(&self::model(2), AREA, CELL, 1.0), 0.0);

        // Scrolled to the end, the last row is visible and the first is not.
        let (items, _) = layout(&model, AREA, CELL, 1.0, limit);
        let panes: Vec<PaneId> = items
            .iter()
            .filter_map(|p| match p.item {
                Item::Row(Row::Agent { target, .. }) => Some(target.pane),
                _ => None,
            })
            .collect();
        assert_eq!(panes.last(), Some(&39));
        assert!(!panes.contains(&0));
        // No row is placed over the fixed header.
        let header_bottom = items[5].rect.y + items[5].rect.h;
        assert!(items.iter().filter(|p| matches!(p.item, Item::Row(_))).all(|p| p.rect.y >= header_bottom));
    }

    #[test]
    fn notice_is_pinned_to_the_bottom() {
        let mut model = model(1);
        model.notice = Some("Set up status".into());
        // Above the archive row.
        assert_eq!(hit(&model, AREA, CELL, 1.0, 0.0, 50.0, 560.0), Some(Hit::Notice));
        assert_eq!(hit(&model, AREA, CELL, 1.0, 0.0, 50.0, 595.0), Some(Hit::Archive));
    }

    #[test]
    fn filters_select_states() {
        assert!(Filter::All.matches(AgentState::Idle));
        assert!(Filter::Waiting.matches(AgentState::NeedsYou));
        assert!(Filter::Waiting.matches(AgentState::Done));
        assert!(!Filter::Waiting.matches(AgentState::Working));
        assert!(Filter::Working.matches(AgentState::WaitingOnSubagents));
        assert!(!Filter::Working.matches(AgentState::Idle));
    }

    #[test]
    fn elapsed_time_is_short() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(59), "59s");
        assert_eq!(format_elapsed(60), "1m");
        assert_eq!(format_elapsed(3599), "59m");
        assert_eq!(format_elapsed(7200), "2h");
        assert_eq!(format_elapsed(200_000), "2d");
    }
}
