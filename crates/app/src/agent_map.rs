//! The agent map: a screen showing one agent and everything it launched
//! as a tree drawn left to right, the agent on the left and its subagents
//! (and theirs) branching off to the right. Each box says what it was asked
//! to do, how long it has run, how big its conversation is and what it is
//! doing now. Clicking one opens the graph panel on it, which shows its
//! output.
//!
//! The nodes are the graph panel's ([`crate::graph::Node`]); this module
//! lays them out, draws them and answers what is under the pointer.

use ale_config::Palette;
use ale_render::Renderer;

use crate::draw::color;
use crate::graph::{self, Node};
use crate::layout::Rect;

/// What the screen shows.
pub struct View<'a> {
    pub nodes: &'a [Node],
    /// The line under the title: the agent's task and how many it runs.
    pub summary: String,
    /// Shown under the tree, saying why there is little or nothing in it.
    pub note: Option<String>,
    /// How far the tree is scrolled, in pixels.
    pub scroll: (f32, f32),
    pub pointer: (f32, f32),
}

/// What is under a window position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Close,
    /// A box, by its position in the view's nodes.
    Node(usize),
}

pub struct Layout {
    title: (f32, f32),
    pub close: Rect,
    /// Where the tree is drawn; it scrolls within this.
    canvas: Rect,
    /// Each node's box, scrolled, in the order of the view's nodes.
    boxes: Vec<Rect>,
    /// The tree's whole size, for limiting the scroll.
    content: (f32, f32),
    column_gap: f32,
}

/// Rows each node sits at, top to bottom: a node without children takes
/// the next row, and one with children sits level with the middle of them.
/// Rows can be halves.
fn rows(parents: &[Option<usize>]) -> (Vec<f32>, usize) {
    let mut children = vec![Vec::new(); parents.len()];
    for (child, parent) in parents.iter().enumerate() {
        // Parents always come before their children, so this cannot loop.
        if let Some(parent) = parent.filter(|parent| *parent < child) {
            children[parent].push(child);
        }
    }
    fn place(node: usize, children: &[Vec<usize>], rows: &mut [f32], next: &mut usize) -> f32 {
        let row = match children[node].as_slice() {
            [] => {
                *next += 1;
                (*next - 1) as f32
            }
            kids => {
                let placed: Vec<f32> = kids.iter().map(|kid| place(*kid, children, rows, next)).collect();
                (placed[0] + placed[placed.len() - 1]) / 2.0
            }
        };
        rows[node] = row;
        row
    }
    let mut rows = vec![0.0; parents.len()];
    let mut next = 0;
    for (node, parent) in parents.iter().enumerate() {
        if parent.is_none_or(|parent| parent >= node) {
            place(node, &children, &mut rows, &mut next);
        }
    }
    (rows, next)
}

impl Layout {
    /// `top` is the height of the window's title bar, which the screen
    /// leaves clear.
    pub fn new(view: &View, window: (f32, f32), cell: (f32, f32), scale: f32, top: f32) -> Layout {
        let px = |points: f32| (points * scale).round();
        let pad = px(24.0);
        let button = (cell.1 + px(10.0)).round();
        let header = cell.1 * 2.0 + px(4.0);
        let title = (pad, top + px(10.0));
        let close_width = (cell.0 * 3.0 + px(20.0)).round();
        let close = Rect::new(window.0 - pad - close_width, title.1 + ((header - button) / 2.0).round(), close_width, button);
        let canvas_top = title.1 + header + px(20.0);
        let canvas = Rect::new(pad, canvas_top, (window.0 - pad * 2.0).max(0.0), (window.1 - pad - canvas_top).max(0.0));

        let box_height = (cell.1 * 3.0 + px(6.0) * 2.0 + px(10.0) * 2.0).round();
        let column_gap = px(56.0);
        let row_gap = px(14.0);

        let parents = graph::parents(view.nodes);
        let (rows, count) = rows(&parents);
        let deepest = view.nodes.iter().map(|node| node.depth).max().unwrap_or(0);
        // Boxes wide enough for a task of a few words, narrower so every
        // column fits across the window, down to a width that still says
        // something; deeper trees than that scroll sideways.
        let fitting = (canvas.w - deepest as f32 * column_gap) / (deepest + 1) as f32;
        let box_width = (cell.0 * 44.0).min(fitting).max(cell.0 * 24.0).floor();
        let content = (
            (deepest + 1) as f32 * box_width + deepest as f32 * column_gap,
            count as f32 * box_height + count.saturating_sub(1) as f32 * row_gap,
        );
        let scroll = clamp_scroll(view.scroll, content, canvas);
        let boxes = view
            .nodes
            .iter()
            .zip(&rows)
            .map(|(node, row)| {
                let x = canvas.x + node.depth as f32 * (box_width + column_gap) - scroll.0;
                let y = canvas.y + (row * (box_height + row_gap)).round() - scroll.1;
                Rect::new(x, y, box_width, box_height)
            })
            .collect();
        Layout { title, close, canvas, boxes, content, column_gap }
    }

    /// The furthest the tree can be scrolled each way.
    pub fn max_scroll(&self) -> (f32, f32) {
        ((self.content.0 - self.canvas.w).max(0.0), (self.content.1 - self.canvas.h).max(0.0))
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if self.close.contains(x, y) {
            return Some(Hit::Close);
        }
        if !self.canvas.contains(x, y) {
            return None;
        }
        self.boxes.iter().position(|rect| rect.contains(x, y)).map(Hit::Node)
    }
}

/// Keeps a scroll position within what there is to see.
pub fn clamp_scroll(scroll: (f32, f32), content: (f32, f32), canvas: Rect) -> (f32, f32) {
    (scroll.0.clamp(0.0, (content.0 - canvas.w).max(0.0)), scroll.1.clamp(0.0, (content.1 - canvas.h).max(0.0)))
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

/// The second line of a box: what kind of thing it is (for the agent, its
/// model once known), how long it has run and how big its conversation is.
fn meta(node: &Node) -> String {
    let kind = match (&node.model, node.depth) {
        (Some(model), 0) => model.clone(),
        _ => node.title.clone(),
    };
    let parts = [kind, node.elapsed.clone(), node.tokens_text()];
    parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ")
}

pub fn draw(r: &mut Renderer, view: &View, window: (f32, f32), scale: f32, top: f32, palette: &Palette) {
    let metrics = r.metrics();
    let cell = (metrics.width, metrics.height);
    let layout = Layout::new(view, window, cell, scale, top);
    let hover = layout.hit(view.pointer.0, view.pointer.1);
    let px = |points: f32| (points * scale).round();
    let line = scale.round().max(1.0);
    let fg = color(palette.foreground);
    let dim = color(palette.chrome_text_dim);
    let border = color(palette.border);
    let chrome = color(palette.chrome);
    let active = color(palette.chrome_active);

    r.rect(0.0, 0.0, window.0, window.1, color(palette.background));

    // Header: the title and what is on show, and the close button.
    let (x, y) = layout.title;
    let room = layout.close.x - x - cell.0 * 2.0;
    r.text("Agent map", true, x, y, room, fg);
    r.text(&fit(&view.summary, room, cell.0), false, x, y + cell.1 + px(4.0), room, dim);
    let close = layout.close;
    outline(r, close, line, if hover == Some(Hit::Close) { active } else { chrome });
    let glyph = "×";
    let glyph_width = r.text_width(glyph);
    let glyph_x = close.x + ((close.w - glyph_width) / 2.0).round();
    r.text(glyph, false, glyph_x, close.y + ((close.h - cell.1) / 2.0).round(), glyph_width, fg);

    let canvas = layout.canvas;
    r.set_clip(Some([canvas.x, canvas.y, canvas.w, canvas.h]));

    // Connectors first, so the boxes sit on top: from the middle of a
    // parent's right edge, across, down or up to the child's middle, and
    // across into it.
    let parents = graph::parents(view.nodes);
    let half_gap = (layout.column_gap / 2.0).round();
    for (child, parent) in parents.iter().enumerate() {
        let Some(parent) = parent else { continue };
        let (from, to) = (layout.boxes[*parent], layout.boxes[child]);
        let (start_x, start_y) = (from.x + from.w, from.y + (from.h / 2.0).round());
        let (end_x, end_y) = (to.x, to.y + (to.h / 2.0).round());
        let bend = (start_x + half_gap).min(end_x);
        r.rect(start_x, start_y, bend - start_x, line, border);
        r.rect(bend, start_y.min(end_y), line, (end_y - start_y).abs() + line, border);
        r.rect(bend, end_y, end_x - bend, line, border);
    }

    let inset = px(10.0);
    let spacing = px(6.0);
    for (index, (node, rect)) in view.nodes.iter().zip(&layout.boxes).enumerate() {
        let rect = *rect;
        if rect.x > canvas.x + canvas.w || rect.y > canvas.y + canvas.h || rect.x + rect.w < canvas.x || rect.y + rect.h < canvas.y {
            continue;
        }
        let hovered = hover == Some(Hit::Node(index));
        r.rect(rect.x, rect.y, rect.w, rect.h, if hovered { active } else { chrome });
        outline(r, rect, line, if hovered { color(palette.accent) } else { border });

        let radius = (3.5 * scale).round();
        let inner_x = rect.x + inset + radius * 2.0 + cell.0;
        let inner_width = (rect.x + rect.w - inset - inner_x).max(0.0);
        let first = rect.y + inset;
        let second = first + cell.1 + spacing;
        let third = second + cell.1 + spacing;
        let text_color = if node.running { fg } else { dim };
        let state = color(node.color);
        crate::draw::dot(r, rect.x + inset + radius, first + (cell.1 / 2.0).round(), radius, if node.running { state } else { dim });

        // What it was asked to do, or its kind when that is not known.
        let task = if node.detail.is_empty() { node.heading() } else { node.detail.clone() };
        r.text(&fit(&task, inner_width, cell.0), true, inner_x, first, inner_width, text_color);
        r.text(&fit(&meta(node), inner_width, cell.0), false, inner_x, second, inner_width, dim);
        let status_color = if node.running { state } else { dim };
        r.text(&fit(&node.status, inner_width, cell.0), false, inner_x, third, inner_width, status_color);
    }

    if let Some(note) = &view.note {
        let below = layout.boxes.iter().map(|rect| rect.y + rect.h).fold(canvas.y, f32::max);
        let y = (below + px(24.0)).min(canvas.y + canvas.h - cell.1);
        r.text(&fit(note, canvas.w, cell.0), false, canvas.x, y, canvas.w, dim);
    }
    r.set_clip(None);

    // When there is more than fits, say which way.
    let (more_x, more_y) = layout.max_scroll();
    if more_x > 0.0 || more_y > 0.0 {
        let hint = "Scroll to see the whole tree";
        let width = r.text_width(hint);
        r.text(hint, false, canvas.x + canvas.w - width, canvas.y - cell.1 - px(6.0), width, dim);
    }
}

/// A one-line frame just inside `rect`.
fn outline(r: &mut Renderer, rect: Rect, line: f32, ink: ale_render::Color) {
    r.rect(rect.x, rect.y, rect.w, line, ink);
    r.rect(rect.x, rect.y + rect.h - line, rect.w, line, ink);
    r.rect(rect.x, rect.y, line, rect.h, ink);
    r.rect(rect.x + rect.w - line, rect.y, line, rect.h, ink);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::NodeId;
    use ale_config::Rgb;

    const WINDOW: (f32, f32) = (1400.0, 900.0);
    const CELL: (f32, f32) = (8.0, 16.0);

    fn node(depth: usize) -> Node {
        Node {
            id: NodeId::Main,
            depth,
            title: "Explore".into(),
            detail: "look around".into(),
            status: "Reading x".into(),
            color: Rgb::new(1, 2, 3),
            elapsed: "5s".into(),
            running: true,
            model: None,
            tokens: None,
        }
    }

    fn view(nodes: &[Node]) -> View<'_> {
        View { nodes, summary: String::new(), note: None, scroll: (0.0, 0.0), pointer: (0.0, 0.0) }
    }

    #[test]
    fn leaves_take_a_row_each_and_parents_sit_between_their_children() {
        // The agent launched two; the first of them launched two more.
        let parents = [None, Some(0), Some(1), Some(1), Some(0)];
        let (rows, count) = rows(&parents);
        assert_eq!(count, 3);
        assert_eq!(rows, [1.25, 0.5, 0.0, 1.0, 2.0]);
        // Just the agent.
        assert_eq!(super::rows(&[None]), (vec![0.0], 1));
    }

    #[test]
    fn columns_go_by_depth_and_every_box_can_be_clicked() {
        let nodes = [node(0), node(1), node(2), node(1)];
        let layout = Layout::new(&view(&nodes), WINDOW, CELL, 1.0, 38.0);
        let xs: Vec<f32> = layout.boxes.iter().map(|rect| rect.x).collect();
        assert!(xs[0] < xs[1] && xs[1] < xs[2] && xs[1] == xs[3]);
        // The agent sits level with the middle of its two children.
        let middle = |rect: Rect| rect.y + rect.h / 2.0;
        assert_eq!(middle(layout.boxes[0]), (middle(layout.boxes[1]) + middle(layout.boxes[3])) / 2.0);
        for (index, rect) in layout.boxes.iter().enumerate() {
            assert_eq!(layout.hit(rect.x + 3.0, rect.y + 3.0), Some(Hit::Node(index)));
        }
        assert_eq!(layout.hit(layout.close.x + 2.0, layout.close.y + 2.0), Some(Hit::Close));
        assert_eq!(layout.max_scroll(), (0.0, 0.0));
    }

    #[test]
    fn three_columns_fit_a_laptop_window() {
        let nodes = [node(0), node(1), node(2)];
        let layout = Layout::new(&view(&nodes), (1100.0, 700.0), CELL, 1.0, 38.0);
        assert_eq!(layout.max_scroll(), (0.0, 0.0));
        assert!(layout.boxes[2].x + layout.boxes[2].w <= 1100.0 - 24.0);
    }

    #[test]
    fn the_agent_is_described_by_its_model_once_known() {
        let mut agent = node(0);
        agent.title = "Claude Code".into();
        assert_eq!(meta(&agent), "Claude Code · 5s");
        agent.model = Some("Opus 5.5".into());
        agent.tokens = Some(60_500);
        assert_eq!(meta(&agent), "Opus 5.5 · 5s · 60.5K in context");
        let sub = Node { tokens: Some(1_000), ..node(1) };
        assert_eq!(meta(&sub), "Explore · 5s · 1K tokens");
    }

    #[test]
    fn a_tree_larger_than_the_screen_scrolls_and_no_further() {
        let mut nodes = vec![node(0)];
        nodes.extend((0..30).map(|_| node(1)));
        let still = Layout::new(&view(&nodes), WINDOW, CELL, 1.0, 38.0);
        let (_, most) = still.max_scroll();
        assert!(most > 0.0);
        let scrolled = Layout::new(&View { scroll: (0.0, 1e6), ..view(&nodes) }, WINDOW, CELL, 1.0, 38.0);
        assert_eq!(scrolled.boxes[1].y, still.boxes[1].y - most);
        // A box scrolled out of sight cannot be clicked.
        let hidden = scrolled.boxes[1];
        assert_eq!(scrolled.hit(hidden.x + 3.0, hidden.y + 3.0), None);
    }
}
