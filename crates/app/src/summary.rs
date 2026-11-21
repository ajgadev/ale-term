//! The summary card: what an agent has been doing lately, shown beside the
//! pointer when it rests on the agent's tab, sidebar row or board card.
//!
//! The app supplies plain data (a [`Summary`]); this module lays it out and
//! draws it.

use ale_agents::AgentState;
use ale_agents::plan::{ItemStatus, Plan};
use ale_config::Palette;
use ale_render::Renderer;

use crate::draw::color;
use crate::graph::wrap;
use crate::layout::Rect;
use crate::sidebar::state_color;

/// What the card says about one agent.
pub struct Summary {
    /// Image key of the agent's icon.
    pub icon: u64,
    /// The agent's name, e.g. "Claude Code".
    pub agent: String,
    /// The name the user gave its tab, if any.
    pub tab: Option<String>,
    /// Its task.
    pub title: String,
    pub state: AgentState,
    /// What it is doing or waiting for, or its state.
    pub status: String,
    /// Time in its current state.
    pub elapsed: String,
    /// The last thing it said (Claude Code's own record of the session).
    pub message: Option<String>,
    /// Its latest actions, newest first, each with how long ago it began.
    pub actions: Vec<(String, String)>,
    /// Its status comes from hooks; without them nothing is recorded.
    pub hooked: bool,
    /// The to-do list it is working through, if any.
    pub plan: Plan,
}

/// Width of the card, in text columns.
const COLUMNS: usize = 54;
/// At most this many lines of the last message.
const MESSAGE_LINES: usize = 4;
/// At most this many items of a plan, around the one in progress.
const PLAN_ITEMS: usize = 8;
/// With a plan to show, fewer of the latest actions.
const ACTIONS_WITH_PLAN: usize = 3;

/// The card's lines, top to bottom.
#[derive(Debug, PartialEq)]
enum Line {
    Header,
    Title,
    Status,
    Divider,
    Label(&'static str),
    Message(String),
    Action(String, String),
    Note(&'static str),
    /// "Plan", with how many items are done.
    PlanHeading(usize, usize),
    PlanItem(ItemStatus, String),
    /// Items of the plan not shown, before or after those listed.
    PlanMore(usize),
}

/// The plan's lines: a window of items around the one in progress (or the
/// first not done), with a note for those left out on either side.
fn plan_lines(plan: &Plan) -> Vec<Line> {
    let Some((done, total)) = plan.progress() else { return Vec::new() };
    let focus = plan
        .items
        .iter()
        .position(|item| item.status == ItemStatus::InProgress)
        .or_else(|| plan.items.iter().position(|item| item.status != ItemStatus::Done))
        .unwrap_or(total.saturating_sub(1));
    let start = focus.saturating_sub(2).min(total.saturating_sub(PLAN_ITEMS));
    let end = (start + PLAN_ITEMS).min(total);
    let mut lines = vec![Line::Divider, Line::PlanHeading(done, total)];
    if start > 0 {
        lines.push(Line::PlanMore(start));
    }
    for item in &plan.items[start..end] {
        let text = if item.status == ItemStatus::InProgress { item.label() } else { item.text.as_str() };
        lines.push(Line::PlanItem(item.status, text.to_owned()));
    }
    if end < total {
        lines.push(Line::PlanMore(total - end));
    }
    lines
}

fn lines(summary: &Summary) -> Vec<Line> {
    let mut lines = vec![Line::Header, Line::Title, Line::Status];
    lines.extend(plan_lines(&summary.plan));
    if let Some(message) = summary.message.as_deref().filter(|m| !m.is_empty()) {
        lines.push(Line::Divider);
        lines.push(Line::Label("Last message"));
        let mut rows = wrap(message, COLUMNS - 2);
        if rows.len() > MESSAGE_LINES {
            rows.truncate(MESSAGE_LINES);
            if let Some(last) = rows.last_mut() {
                // Room for the ellipsis.
                while last.chars().count() > COLUMNS - 3 {
                    last.pop();
                }
                last.push('…');
            }
        }
        lines.extend(rows.into_iter().map(Line::Message));
    }
    if !summary.actions.is_empty() {
        lines.push(Line::Divider);
        lines.push(Line::Label("Latest actions"));
        let shown = if summary.plan.is_empty() { summary.actions.len() } else { ACTIONS_WITH_PLAN };
        lines.extend(summary.actions.iter().take(shown).map(|(action, age)| Line::Action(action.clone(), age.clone())));
    }
    if summary.message.is_none() && summary.actions.is_empty() && summary.plan.is_empty() {
        lines.push(Line::Divider);
        lines.push(Line::Note(if summary.hooked {
            "Nothing done yet in this session."
        } else {
            "Set up its status hooks to see what it did."
        }));
    }
    lines
}

/// Sizes in pixels.
struct Sizes {
    pad: f32,
    row: f32,
    divider: f32,
}

impl Sizes {
    fn new(cell: (f32, f32), scale: f32) -> Self {
        Sizes { pad: (10.0 * scale).round(), row: (cell.1 + 4.0 * scale).round(), divider: (9.0 * scale).round() }
    }

    fn height_of(&self, line: &Line) -> f32 {
        if *line == Line::Divider { self.divider } else { self.row }
    }
}

/// Where the card goes: below and to the right of the pointer, or flipped
/// to the other side when it would leave the window.
fn frame(summary: &Summary, pointer: (f32, f32), window: (f32, f32), cell: (f32, f32), scale: f32) -> Rect {
    let sizes = Sizes::new(cell, scale);
    let width = (COLUMNS as f32 * cell.0 + sizes.pad * 2.0).min(window.0 - sizes.pad * 2.0).round();
    let height = lines(summary).iter().map(|line| sizes.height_of(line)).sum::<f32>() + sizes.pad * 2.0;
    let offset = (16.0 * scale).round();
    let mut x = pointer.0 + offset;
    if x + width > window.0 - sizes.pad {
        x = pointer.0 - offset - width;
    }
    let mut y = pointer.1 + offset;
    if y + height > window.1 - sizes.pad {
        y = pointer.1 - offset - height;
    }
    let x = x.clamp(sizes.pad.min(window.0 - width), (window.0 - width - sizes.pad).max(0.0));
    let y = y.clamp(0.0, (window.1 - height).max(0.0));
    Rect::new(x.round(), y.round(), width, height.round())
}

/// Shortens `text` with an ellipsis to fit `width` pixels.
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

pub fn draw(r: &mut Renderer, summary: &Summary, pointer: (f32, f32), window: (f32, f32), scale: f32, palette: &Palette) {
    let metrics = r.metrics();
    let cell = (metrics.width, metrics.height);
    let sizes = Sizes::new(cell, scale);
    let frame = frame(summary, pointer, window, cell, scale);
    let line = scale.round().max(1.0);
    let (fg, dim, border) = (color(palette.foreground), color(palette.chrome_text_dim), color(palette.border));
    let state = color(state_color(summary.state, palette));

    // A soft shadow, then the card with a state-colored edge.
    let [br, bg, bb, _] = color(palette.background);
    let shadow = (4.0 * scale).round();
    r.rect(frame.x + shadow, frame.y + shadow, frame.w, frame.h, [br / 2, bg / 2, bb / 2, 120]);
    r.rect(frame.x, frame.y, frame.w, frame.h, color(palette.chrome));
    for (x, y, w, h) in [
        (frame.x, frame.y, frame.w, line),
        (frame.x, frame.y + frame.h - line, frame.w, line),
        (frame.x + frame.w - line, frame.y, line, frame.h),
    ] {
        r.rect(x, y, w, h, border);
    }
    r.rect(frame.x, frame.y, line * 3.0, frame.h, state);

    let left = frame.x + sizes.pad + line * 2.0;
    let width = frame.x + frame.w - sizes.pad - left;
    let mut y = frame.y + sizes.pad;
    for item in lines(summary) {
        let height = sizes.height_of(&item);
        let text_y = y + ((height - cell.1) / 2.0).round();
        match &item {
            Line::Header => {
                let size = cell.1.round();
                r.image(summary.icon, left, text_y, size, size, [0.0, 0.0, 1.0, 1.0]);
                let elapsed_width = r.text_width(&summary.elapsed);
                let name_x = left + size + (cell.0 * 0.75).round();
                let name_room = left + width - elapsed_width - cell.0 - name_x;
                let (agent, tab) = (&summary.agent, summary.tab.as_deref());
                crate::board::draw_agent_name(r, agent, tab, (name_x, text_y), name_room, cell.0, dim, fg);
                r.text(&summary.elapsed, false, left + width - elapsed_width, text_y, elapsed_width, dim);
            }
            Line::Title => {
                r.text(&fit(&summary.title, width, cell.0), true, left, text_y, width, fg);
            }
            Line::Status => {
                let ink = if summary.state == AgentState::Working { dim } else { state };
                r.text(&fit(&summary.status, width, cell.0), false, left, text_y, width, ink);
            }
            Line::Divider => {
                r.rect(left, y + (height / 2.0).round(), width, line, border);
            }
            Line::Label(label) => {
                r.text(label, true, left, text_y, width, color(palette.accent));
            }
            Line::Message(text) => {
                r.text(text, false, left, text_y, width, fg);
            }
            Line::Action(action, age) => {
                let age_width = r.text_width(age);
                let room = width - age_width - cell.0 * 3.0;
                r.text("•", false, left, text_y, cell.0, dim);
                r.text(&fit(action, room, cell.0), false, left + cell.0 * 2.0, text_y, room, fg);
                r.text(age, false, left + width - age_width, text_y, age_width, dim);
            }
            Line::Note(note) => {
                r.text(note, false, left, text_y, width, dim);
            }
            Line::PlanHeading(done, total) => {
                let used = r.text("Plan", true, left, text_y, width, color(palette.accent));
                let count = format!("  {done} of {total} done");
                r.text(&count, false, left + used, text_y, width - used, dim);
            }
            Line::PlanItem(status, text) => {
                let (mark, mark_ink, text_ink, bold) = match status {
                    ItemStatus::Done => ("✓", color(palette.ansi[2]), dim, false),
                    ItemStatus::InProgress => ("▸", color(palette.accent), fg, true),
                    ItemStatus::Pending => ("○", dim, fg, false),
                };
                r.text(mark, false, left, text_y, cell.0 * 2.0, mark_ink);
                let room = width - cell.0 * 2.0;
                r.text(&fit(text, room, cell.0), bold, left + cell.0 * 2.0, text_y, room, text_ink);
            }
            Line::PlanMore(count) => {
                let text = if *count == 1 { "  … 1 more".to_owned() } else { format!("  … {count} more") };
                r.text(&text, false, left, text_y, width, dim);
            }
        }
        y += height;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (f32, f32) = (8.0, 16.0);
    const WINDOW: (f32, f32) = (1200.0, 800.0);

    fn summary() -> Summary {
        Summary {
            icon: 0,
            agent: "Claude Code".into(),
            tab: Some("Sheesh".into()),
            title: "pricing page".into(),
            state: AgentState::Done,
            status: "Done".into(),
            elapsed: "2m".into(),
            message: Some("Updated the pricing cards so they share one height. ".repeat(6)),
            actions: vec![("Running npm test".into(), "2m".into()), ("Editing pricing.css".into(), "3m".into())],
            hooked: true,
            plan: Plan::default(),
        }
    }

    #[test]
    fn long_messages_are_cut_to_a_few_lines() {
        let lines = lines(&summary());
        let message: Vec<&Line> = lines.iter().filter(|line| matches!(line, Line::Message(_))).collect();
        assert_eq!(message.len(), MESSAGE_LINES);
        assert!(matches!(message.last(), Some(Line::Message(text)) if text.ends_with('…')));
        assert_eq!(lines.iter().filter(|line| matches!(line, Line::Action(..))).count(), 2);
    }

    #[test]
    fn without_anything_recorded_it_says_why() {
        let mut quiet = summary();
        quiet.message = None;
        quiet.actions.clear();
        assert!(lines(&quiet).contains(&Line::Note("Nothing done yet in this session.")));
        quiet.hooked = false;
        assert!(lines(&quiet).contains(&Line::Note("Set up its status hooks to see what it did.")));
    }

    #[test]
    fn a_long_plan_shows_the_items_around_the_one_in_progress() {
        use ale_agents::plan::PlanItem;
        let item = |n: usize, status| PlanItem { id: n.to_string(), text: format!("step {n}"), active: None, status };
        let mut with_plan = summary();
        with_plan.plan.items = (1..=12)
            .map(|n| item(n, if n < 7 { ItemStatus::Done } else if n == 7 { ItemStatus::InProgress } else { ItemStatus::Pending }))
            .collect();
        let lines = lines(&with_plan);
        assert!(lines.contains(&Line::PlanHeading(6, 12)));
        let items: Vec<&String> = lines.iter().filter_map(|l| if let Line::PlanItem(_, t) = l { Some(t) } else { None }).collect();
        assert_eq!(items.len(), PLAN_ITEMS);
        assert_eq!(items[0], "step 5");
        assert!(lines.contains(&Line::PlanItem(ItemStatus::InProgress, "step 7".into())));
        // The four done before them are summed up; nothing is left after.
        let more: Vec<&Line> = lines.iter().filter(|l| matches!(l, Line::PlanMore(_))).collect();
        assert_eq!(more, [&Line::PlanMore(4)]);
        // With a plan, fewer actions.
        assert_eq!(lines.iter().filter(|l| matches!(l, Line::Action(..))).count(), 2.min(ACTIONS_WITH_PLAN));
    }

    #[test]
    fn the_card_stays_inside_the_window() {
        let summary = summary();
        for pointer in [(10.0, 10.0), (1190.0, 10.0), (10.0, 790.0), (1190.0, 790.0), (600.0, 400.0)] {
            let frame = frame(&summary, pointer, WINDOW, CELL, 1.0);
            assert!(frame.x >= 0.0 && frame.y >= 0.0, "{pointer:?}: {frame:?}");
            assert!(frame.x + frame.w <= WINDOW.0 && frame.y + frame.h <= WINDOW.1, "{pointer:?}: {frame:?}");
            // It never covers the point it describes.
            assert!(!frame.contains(pointer.0, pointer.1), "{pointer:?}: {frame:?}");
        }
    }
}
