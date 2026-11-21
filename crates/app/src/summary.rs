//! The summary card: what an agent has been doing lately, shown beside the
//! pointer when it rests on the agent's tab, sidebar row or board card.
//!
//! The app supplies plain data (a [`Summary`]); this module lays it out and
//! draws it.

use ale_agents::AgentState;
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
}

/// Width of the card, in text columns.
const COLUMNS: usize = 54;
/// At most this many lines of the last message.
const MESSAGE_LINES: usize = 4;

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
}

fn lines(summary: &Summary) -> Vec<Line> {
    let mut lines = vec![Line::Header, Line::Title, Line::Status];
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
        lines.extend(summary.actions.iter().map(|(action, age)| Line::Action(action.clone(), age.clone())));
    }
    if summary.message.is_none() && summary.actions.is_empty() {
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
                r.text(&fit(&summary.agent, name_room, cell.0), false, name_x, text_y, name_room.max(0.0), dim);
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
            title: "pricing page".into(),
            state: AgentState::Done,
            status: "Done".into(),
            elapsed: "2m".into(),
            message: Some("Updated the pricing cards so they share one height. ".repeat(6)),
            actions: vec![("Running npm test".into(), "2m".into()), ("Editing pricing.css".into(), "3m".into())],
            hooked: true,
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
