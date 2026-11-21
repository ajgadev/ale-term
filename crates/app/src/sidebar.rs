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
use crate::workspace::{TabId, WorkspaceId};

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
            Filter::Working => matches!(state, AgentState::Working | AgentState::Waiting),
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
        AgentState::Waiting => palette.ansi[5],
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

impl Row {
    fn target(&self) -> Target {
        match self {
            Row::Agent { target, .. } | Row::Subagent { target, .. } | Row::Shell { target, .. } => *target,
        }
    }

    /// What the row is called, for the label that follows a dragged row.
    fn label(&self) -> &str {
        match self {
            Row::Agent { title, .. } => title,
            Row::Subagent { text, .. } | Row::Shell { text, .. } => text,
        }
    }
}

/// Where a dragged row would land: in a workspace's group, above the rows
/// of the tab `before`, or at the group's end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropSpot {
    pub workspace: WorkspaceId,
    pub before: Option<TabId>,
}

/// A row being carried by the pointer.
pub struct Dragging {
    /// The tab whose rows are being moved.
    pub tab: TabId,
    pub pointer: (f32, f32),
    /// Where it would land if let go now; `None` when that changes nothing.
    pub spot: Option<DropSpot>,
}

pub struct Group {
    pub title: String,
    /// The workspace this group lists, when grouped by project.
    pub workspace: Option<WorkspaceId>,
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
    /// A row being dragged, and where it would land.
    pub dragging: Option<Dragging>,
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
    Header(&'a Group),
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

/// How many items are fixed at the top: new session, search, toggle, three
/// chips.
const FIXED: usize = 6;

/// Places the fixed controls and every row, scrolled by `scroll` pixels,
/// including rows scrolled out of sight. Returns them with the total height
/// of the rows and the band (top, bottom) in which rows are visible.
fn place<'a>(
    model: &'a Model,
    area: Rect,
    cell: (f32, f32),
    scale: f32,
    scroll: f32,
) -> (Vec<Placed<'a>>, f32, (f32, f32)) {
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
        items.push(Placed { rect: Rect::new(area.x, row_y, area.w, header_height), item: Item::Header(group) });
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
    (items, content_height, (rows_top, area.y + area.h - notice_height))
}

/// Places everything that shows. `scroll` is how far the rows are scrolled,
/// in pixels. Returns the placed items and the total height of the rows
/// (for clamping the scroll).
fn layout<'a>(model: &'a Model, area: Rect, cell: (f32, f32), scale: f32, scroll: f32) -> (Vec<Placed<'a>>, f32) {
    let (mut items, content_height, band) = place(model, area, cell, scale, scroll);
    // Drop rows scrolled out of the visible band so they are neither drawn
    // nor clickable.
    let mut index = 0;
    items.retain(|placed| {
        index += 1;
        index <= FIXED || (placed.rect.y >= band.0 - 1.0 && placed.rect.y + placed.rect.h <= band.1 + 1.0)
    });

    let gap = (LINE_GAP * scale).round();
    let pinned_row = cell.1 + gap * 4.0;
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

/// Where a row of tab `dragged` would land if dropped at a point: in the
/// group under the pointer, between the two tabs nearest to it. Below the
/// last group counts as that group's end. `None` where there is no group,
/// or where dropping would leave the tab where it is.
pub fn drop_at(
    model: &Model,
    area: Rect,
    cell: (f32, f32),
    scale: f32,
    scroll: f32,
    (x, y): (f32, f32),
    dragged: TabId,
) -> Option<DropSpot> {
    if !area.contains(x, y) {
        return None;
    }
    let (items, _, band) = place(model, area, cell, scale, scroll);
    if y < band.0 {
        return None;
    }
    // The group under the pointer is the last heading above it; gather the
    // span of each of its tabs (a tab with splits or subagents has several
    // rows).
    let mut workspace = None;
    let mut tabs: Vec<(TabId, f32, f32)> = Vec::new();
    for placed in &items {
        match &placed.item {
            Item::Header(group) => {
                if placed.rect.y > y {
                    break;
                }
                workspace = group.workspace;
                tabs.clear();
            }
            Item::Row(row) => {
                let (tab, bottom) = (row.target().tab, placed.rect.y + placed.rect.h);
                match tabs.last_mut() {
                    Some(last) if last.0 == tab => last.2 = bottom,
                    _ => tabs.push((tab, placed.rect.y, bottom)),
                }
            }
            _ => {}
        }
    }
    let workspace = workspace?;
    let index = tabs.iter().position(|(_, top, bottom)| y < (top + bottom) / 2.0).unwrap_or(tabs.len());
    let is_dragged = |at: Option<usize>| at.and_then(|at| tabs.get(at)).is_some_and(|tab| tab.0 == dragged);
    if is_dragged(Some(index)) || is_dragged(index.checked_sub(1)) {
        return None;
    }
    Some(DropSpot { workspace, before: tabs.get(index).map(|tab| tab.0) })
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
    // Nothing lights up under the pointer while it carries a row.
    let hovered = |hit: Hit| model.dragging.is_none() && model.hovered == Some(hit);
    let spot = model.dragging.as_ref().and_then(|dragging| dragging.spot);
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
            Item::Header(group) => {
                // Lit while a dragged row is over this group.
                let target = spot.is_some_and(|spot| group.workspace == Some(spot.workspace));
                let label = fit(&group.title, rect.w - pad * 2.0, cell.0);
                let text_color = if target { color(palette.accent) } else { dim };
                r.text(&label, target, rect.x + pad, rect.y + rect.h - metrics.height - gap, rect.w, text_color);
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
                if !matches!(state, AgentState::Working | AgentState::Waiting) {
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
    if let Some(dragging) = &model.dragging {
        draw_drag(r, dragging, &items, area, scale, palette);
    }
}

/// Shows a row being dragged: its rows fade where they still sit, a line
/// marks where it would land, and its name follows the pointer.
fn draw_drag(r: &mut Renderer, dragging: &Dragging, items: &[Placed], area: Rect, scale: f32, palette: &Palette) {
    let metrics = r.metrics();
    let pad = (PAD * scale).round();
    let gap = (LINE_GAP * scale).round();
    let line = scale.round().max(1.0);
    let accent = color(palette.accent);

    let mut label = None;
    let [red, green, blue, _] = color(palette.chrome);
    for placed in items {
        if let Item::Row(row) = &placed.item
            && row.target().tab == dragging.tab
        {
            let rect = placed.rect;
            r.rect(rect.x, rect.y, rect.w - line, rect.h, [red, green, blue, 170]);
            label = label.or(Some(row.label()));
        }
    }

    // The line: above the rows of the tab it would go before, or under the
    // last row of the group.
    if let Some(spot) = dragging.spot {
        let mut inside = false;
        let mut at = None;
        for placed in items {
            match &placed.item {
                Item::Header(group) => inside = group.workspace == Some(spot.workspace),
                Item::Row(row) if inside => {
                    if Some(row.target().tab) == spot.before {
                        at = Some(placed.rect.y);
                        break;
                    }
                    if spot.before.is_none() {
                        at = Some(placed.rect.y + placed.rect.h);
                    }
                }
                _ => {}
            }
        }
        if let Some(y) = at {
            let thick = line * 2.0;
            let radius = (3.0 * scale).round();
            let y = (y - thick / 2.0).round();
            r.rect(area.x + pad, y, area.w - pad * 2.0, thick, accent);
            crate::draw::dot(r, area.x + pad, y + thick / 2.0, radius, accent);
        }
    }

    // The row's name on a small card beside the pointer.
    if let Some(label) = label {
        let label = fit(label, 28.0 * metrics.width, metrics.width);
        let width = r.text_width(&label) + pad * 2.0;
        let height = metrics.height + gap * 3.0;
        let (x, y) = (dragging.pointer.0 + (14.0 * scale).round(), dragging.pointer.1 + (6.0 * scale).round());
        let [red, green, blue, _] = color(palette.background);
        r.rect(x - line, y + line * 3.0, width + line * 4.0, height + line, [red / 2, green / 2, blue / 2, 70]);
        r.rect(x - line, y - line, width + line * 2.0, height + line * 2.0, accent);
        r.rect(x, y, width, height, color(palette.chrome_active));
        r.text(&label, true, x + pad, y + ((height - metrics.height) / 2.0).round(), width, color(palette.foreground));
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
            groups: vec![Group { title: "shop".into(), workspace: Some(7), rows: (0..rows).map(|i| agent(1, i as u64)).collect() }],
            counts: [rows, 0, rows],
            ..Model::default()
        }
    }

    #[test]
    fn a_dragged_row_lands_between_the_rows_under_the_pointer() {
        // Tabs 1, 2, 3 in "shop"; tab 4 in "blog".
        let mut model = model(0);
        model.groups[0].rows = vec![agent(1, 10), agent(2, 20), agent(3, 30)];
        model.groups.push(Group { title: "blog".into(), workspace: Some(9), rows: vec![agent(4, 40)] });
        // An empty group is not shown, so it cannot be a target.
        model.groups.push(Group { title: "empty".into(), workspace: Some(11), rows: Vec::new() });
        let area = Rect::new(0.0, 0.0, 260.0, 900.0);
        let (items, _) = layout(&model, area, CELL, 1.0, 0.0);
        let rects = |header: bool| -> Vec<Rect> {
            let wanted = |p: &&Placed| if header { matches!(p.item, Item::Header(_)) } else { matches!(p.item, Item::Row(_)) };
            items.iter().filter(wanted).map(|p| p.rect).collect()
        };
        let (headers, rows) = (rects(true), rects(false));
        assert_eq!((headers.len(), rows.len()), (2, 4));
        let at = |y: f32, dragged: TabId| drop_at(&model, area, CELL, 1.0, 0.0, (100.0, y), dragged);
        let spot = |workspace, before| Some(DropSpot { workspace, before });

        // Tab 3 carried over the top half of tab 1 goes first; over the
        // bottom half of tab 1, between 1 and 2.
        assert_eq!(at(rows[0].y + 4.0, 3), spot(7, Some(1)));
        assert_eq!(at(rows[0].y + rows[0].h - 4.0, 3), spot(7, Some(2)));
        // Tab 1 carried below the group's last row goes to its end.
        assert_eq!(at(rows[2].y + rows[2].h - 2.0, 1), spot(7, None));
        // Either side of where it already is changes nothing.
        assert_eq!(at(rows[1].y + 4.0, 2), None);
        assert_eq!(at(rows[1].y + rows[1].h - 4.0, 2), None);
        assert_eq!(at(rows[0].y + rows[0].h - 4.0, 2), None);
        // Into another group: on its heading, on its row, below it.
        assert_eq!(at(headers[1].y + 2.0, 1), spot(9, Some(4)));
        assert_eq!(at(rows[3].y + rows[3].h - 4.0, 1), spot(9, None));
        assert_eq!(at(rows[3].y + 300.0, 1), spot(9, None));
        // Over the fixed controls, and outside the sidebar: nowhere.
        assert_eq!(at(60.0, 1), None);
        assert_eq!(drop_at(&model, area, CELL, 1.0, 0.0, (400.0, rows[0].y + 4.0), 3), None);
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
        assert!(Filter::Working.matches(AgentState::Waiting));
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
