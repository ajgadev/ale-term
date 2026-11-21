//! The agent graph: a panel beside an agent's terminal showing the agent,
//! the subagents it has launched (and theirs), and its background tasks as
//! a tree of boxes, with the selected node's recent output underneath.
//!
//! As with the sidebar and board, the app supplies plain data and this
//! module does layout, drawing and hit-testing.

use ale_config::{Palette, Rgb};
use ale_render::Renderer;

use crate::draw::color;
use crate::layout::Rect;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum NodeId {
    /// The agent itself.
    #[default]
    Main,
    Subagent(String),
    Task(String),
}

pub struct Node {
    pub id: NodeId,
    /// 0 for the agent, 1 for what it launched, 2 for what those launched.
    pub depth: usize,
    /// Kind of thing: "Claude Code", "Explore", "Background task".
    pub title: String,
    /// What it was asked to do.
    pub detail: String,
    /// What it is doing, or how it ended.
    pub status: String,
    pub color: Rgb,
    pub elapsed: String,
    /// Finished nodes are drawn subdued.
    pub running: bool,
}

/// How a line of output is styled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    Prompt,
    Text,
    Tool,
}

#[derive(Default)]
pub struct Model {
    pub nodes: Vec<Node>,
    pub selected: NodeId,
    pub output: Vec<(OutputKind, String)>,
    /// Shown in place of output when there is none, saying why.
    pub note: String,
}

/// Orders subagents so each follows its parent, and gives each its depth
/// in the tree (1 for those launched by the agent itself).
///
/// Input and output refer to subagents by position; `parents[i]` is the
/// position of `i`'s parent, if it has one.
pub fn tree_order(parents: &[Option<usize>]) -> Vec<(usize, usize)> {
    fn visit(node: usize, depth: usize, parents: &[Option<usize>], out: &mut Vec<(usize, usize)>) {
        // Depth is capped so a malformed cycle cannot recurse forever.
        if depth > 8 || out.iter().any(|(seen, _)| *seen == node) {
            return;
        }
        out.push((node, depth));
        for child in 0..parents.len() {
            if parents[child] == Some(node) {
                visit(child, depth + 1, parents, out);
            }
        }
    }
    let mut out = Vec::new();
    for node in 0..parents.len() {
        // Roots: no parent, or a parent that does not exist.
        if parents[node].is_none_or(|parent| parent >= parents.len() || parent == node) {
            visit(node, 1, parents, &mut out);
        }
    }
    // Anything unreachable (a cycle) is still listed, at the top level.
    for node in 0..parents.len() {
        if !out.iter().any(|(seen, _)| *seen == node) {
            out.push((node, 1));
        }
    }
    out
}

/// Sizes in points.
const PAD: f32 = 12.0;
const GAP: f32 = 4.0;
/// The tree takes the height its nodes need, between these shares of the
/// panel; the rest is output.
const TREE_MIN_SHARE: f32 = 0.3;
const TREE_MAX_SHARE: f32 = 0.68;

struct Placed {
    rect: Rect,
    index: usize,
}

struct Layout {
    nodes: Vec<Placed>,
    /// Where the output section starts.
    output: Rect,
    indent: f32,
}

fn layout(model: &Model, area: Rect, cell: (f32, f32), scale: f32) -> Layout {
    let pad = (PAD * scale).round();
    let gap = (GAP * scale).round();
    let indent = cell.0 * 2.0;
    let box_height = cell.1 * 3.0 + gap * 4.0;
    let needed = pad * 2.0 + model.nodes.len() as f32 * (box_height + gap * 2.0);
    let tree_height = needed.clamp(area.h * TREE_MIN_SHARE, area.h * TREE_MAX_SHARE).round();
    let tree_bottom = area.y + tree_height;

    let mut nodes = Vec::new();
    let mut y = area.y + pad;
    for (index, node) in model.nodes.iter().enumerate() {
        if y + box_height > tree_bottom {
            break;
        }
        let x = area.x + pad + node.depth as f32 * indent;
        let width = (area.x + area.w - pad - x).max(cell.0 * 6.0);
        nodes.push(Placed { rect: Rect::new(x, y, width, box_height), index });
        y += box_height + gap * 2.0;
    }
    let output = Rect::new(area.x, tree_bottom, area.w, (area.y + area.h - tree_bottom).max(0.0));
    Layout { nodes, output, indent }
}

/// The node under a window position.
pub fn hit(model: &Model, area: Rect, cell: (f32, f32), scale: f32, x: f32, y: f32) -> Option<NodeId> {
    let layout = layout(model, area, cell, scale);
    let found = layout.nodes.iter().find(|placed| placed.rect.contains(x, y))?;
    Some(model.nodes[found.index].id.clone())
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

/// Breaks `text` into rows of at most `columns` characters, preferring to
/// break at spaces.
pub fn wrap(text: &str, columns: usize) -> Vec<String> {
    let columns = columns.max(4);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut length = 0;
    for word in text.split(' ') {
        let mut word: Vec<char> = word.chars().collect();
        // A word longer than a row is cut into pieces.
        while word.len() > columns {
            if length > 0 {
                rows.push(std::mem::take(&mut row));
                length = 0;
            }
            rows.push(word.drain(..columns).collect());
        }
        let needed = word.len() + (length > 0) as usize;
        if length + needed > columns {
            rows.push(std::mem::take(&mut row));
            length = 0;
        }
        if length > 0 {
            row.push(' ');
            length += 1;
        }
        row.extend(word.iter());
        length += word.len();
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

pub fn draw(r: &mut Renderer, model: &Model, area: Rect, scale: f32, palette: &Palette) {
    let metrics = r.metrics();
    let cell = (metrics.width, metrics.height);
    let pad = (PAD * scale).round();
    let gap = (GAP * scale).round();
    let line = scale.round().max(1.0);
    let dim = color(palette.chrome_text_dim);
    let fg = color(palette.foreground);
    let border = color(palette.border);

    r.rect(area.x, area.y, area.w, area.h, color(palette.chrome));
    r.rect(area.x, area.y, line, area.h, border);

    let layout = layout(model, area, cell, scale);

    // Connectors first, so boxes sit on top of them: from each node an
    // elbow up to the nearest earlier node one level shallower.
    for (position, placed) in layout.nodes.iter().enumerate() {
        let node = &model.nodes[placed.index];
        if node.depth == 0 {
            continue;
        }
        let parent = layout.nodes[..position]
            .iter()
            .rev()
            .find(|earlier| model.nodes[earlier.index].depth + 1 == node.depth);
        let Some(parent) = parent else { continue };
        let x = parent.rect.x + (layout.indent / 2.0).round();
        let mid = placed.rect.y + (placed.rect.h / 2.0).round();
        let top = parent.rect.y + parent.rect.h;
        r.rect(x, top, line, mid - top, border);
        r.rect(x, mid, placed.rect.x - x, line, border);
    }

    for placed in &layout.nodes {
        let node = &model.nodes[placed.index];
        let rect = placed.rect;
        let selected = node.id == model.selected;
        r.rect(rect.x, rect.y, rect.w, rect.h, color(palette.background));
        let edge = if selected { color(palette.accent) } else { border };
        r.rect(rect.x, rect.y, rect.w, line, edge);
        r.rect(rect.x, rect.y + rect.h - line, rect.w, line, edge);
        r.rect(rect.x + rect.w - line, rect.y, line, rect.h, edge);
        r.rect(rect.x, rect.y, line * 3.0, rect.h, color(node.color));

        let inner_x = rect.x + gap * 2.0 + line * 3.0;
        let inner_width = rect.x + rect.w - gap * 2.0 - inner_x;
        let first = rect.y + gap;
        let second = first + metrics.height + gap;
        let third = second + metrics.height + gap;
        let title_color = if node.running { fg } else { dim };

        let elapsed_width = r.text_width(&node.elapsed);
        let title = fit(&node.title, inner_width - elapsed_width - cell.0, cell.0);
        r.text(&title, true, inner_x, first, inner_width, title_color);
        r.text(&node.elapsed, false, inner_x + inner_width - elapsed_width, first, elapsed_width, dim);
        r.text(&fit(&node.detail, inner_width, cell.0), false, inner_x, second, inner_width, title_color);
        let status_color = if node.running { color(node.color) } else { dim };
        r.text(&fit(&node.status, inner_width, cell.0), false, inner_x, third, inner_width, status_color);
    }
    let hidden = model.nodes.len() - layout.nodes.len();
    if hidden > 0 {
        let note = format!("+ {hidden} more");
        r.text(&note, false, area.x + pad, layout.output.y - metrics.height - gap, area.w, dim);
    }

    // Output of the selected node: the most recent rows that fit.
    let output = layout.output;
    r.rect(output.x, output.y, output.w, line, border);
    let heading = match model.nodes.iter().find(|node| node.id == model.selected) {
        Some(node) => format!("Output · {}", node.title),
        None => "Output".to_owned(),
    };
    let text_x = output.x + pad;
    let text_width = output.w - pad * 2.0;
    r.text(&fit(&heading, text_width, cell.0), true, text_x, output.y + gap * 2.0, text_width, dim);

    let rows_top = output.y + gap * 3.0 + metrics.height + gap;
    let row_height = metrics.height + (gap / 2.0).round();
    let capacity = ((output.y + output.h - gap - rows_top) / row_height).floor().max(0.0) as usize;
    let columns = (text_width / cell.0).floor() as usize;
    let mut rows: Vec<(OutputKind, String)> = Vec::new();
    for (kind, text) in &model.output {
        let marked = match kind {
            OutputKind::Prompt => format!("› {text}"),
            OutputKind::Tool => format!("▸ {text}"),
            OutputKind::Text => text.clone(),
        };
        rows.extend(wrap(&marked, columns).into_iter().map(|row| (*kind, row)));
    }
    if rows.is_empty() {
        r.text(&fit(&model.note, text_width, cell.0), false, text_x, rows_top, text_width, dim);
        return;
    }
    let skip = rows.len().saturating_sub(capacity);
    for (index, (kind, row)) in rows.iter().skip(skip).enumerate() {
        let row_color = match kind {
            OutputKind::Text => fg,
            OutputKind::Prompt | OutputKind::Tool => dim,
        };
        r.text(row, false, text_x, rows_top + index as f32 * row_height, text_width, row_color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(900.0, 38.0, 400.0, 700.0);
    const CELL: (f32, f32) = (8.0, 16.0);

    fn node(id: NodeId, depth: usize) -> Node {
        Node {
            id,
            depth,
            title: "Explore".into(),
            detail: "look around".into(),
            status: "Reading x".into(),
            color: Rgb::new(1, 2, 3),
            elapsed: "5s".into(),
            running: true,
        }
    }

    #[test]
    fn children_follow_their_parents_in_tree_order() {
        // 0 and 3 are launched by the agent; 1 and 2 by 0; 4 by 2.
        let parents = [None, Some(0), Some(0), None, Some(2)];
        assert_eq!(tree_order(&parents), [(0, 1), (1, 2), (2, 2), (4, 3), (3, 1)]);
        assert_eq!(tree_order(&[]), []);
        // A parent that does not exist makes a root; a cycle does not hang
        // and loses nobody.
        assert_eq!(tree_order(&[Some(7), None]), [(0, 1), (1, 1)]);
        let mut cyclic: Vec<usize> = tree_order(&[Some(1), Some(0)]).iter().map(|(node, _)| *node).collect();
        cyclic.sort();
        assert_eq!(cyclic, [0, 1]);
    }

    #[test]
    fn nodes_are_indented_by_depth_and_clickable() {
        let model = Model {
            nodes: vec![
                node(NodeId::Main, 0),
                node(NodeId::Subagent("a".into()), 1),
                node(NodeId::Subagent("b".into()), 2),
                node(NodeId::Task("t".into()), 1),
            ],
            ..Model::default()
        };
        let placed = layout(&model, AREA, CELL, 1.0);
        let xs: Vec<f32> = placed.nodes.iter().map(|p| p.rect.x).collect();
        assert_eq!(xs, [912.0, 928.0, 944.0, 928.0]);
        // Every box ends at the same right edge, inside the panel.
        assert!(placed.nodes.iter().all(|p| p.rect.x + p.rect.w == AREA.x + AREA.w - 12.0));
        for p in &placed.nodes {
            let id = hit(&model, AREA, CELL, 1.0, p.rect.x + 4.0, p.rect.y + 4.0);
            assert_eq!(id.as_ref(), Some(&model.nodes[p.index].id));
        }
        // The output section is not part of any node.
        assert_eq!(hit(&model, AREA, CELL, 1.0, 950.0, 700.0), None);
    }

    #[test]
    fn nodes_that_do_not_fit_are_left_out() {
        let model = Model { nodes: (0..40).map(|i| node(NodeId::Subagent(i.to_string()), 1)).collect(), ..Model::default() };
        let placed = layout(&model, AREA, CELL, 1.0);
        assert!(!placed.nodes.is_empty() && placed.nodes.len() < 40);
        assert!(placed.nodes.iter().all(|p| p.rect.y + p.rect.h <= placed.output.y));
    }

    #[test]
    fn text_wraps_at_spaces_and_cuts_long_words() {
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("short", 20), ["short"]);
        assert_eq!(wrap("abcdefghijkl", 5), ["abcde", "fghij", "kl"]);
        assert_eq!(wrap("a abcdefghij", 5), ["a", "abcde", "fghij"]);
        assert_eq!(wrap("", 10), [""]);
        // Rows never exceed the width.
        assert!(wrap("the quick brown fox jumps over the lazy dog", 7).iter().all(|row| row.chars().count() <= 7));
    }
}
