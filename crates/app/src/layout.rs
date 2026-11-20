//! Split-pane layout.
//!
//! A tab's panes form a binary tree: each leaf is a pane, each inner node
//! splits its rectangle in two. This module is pure geometry with no
//! knowledge of terminals, which keeps it easy to test.

pub type PaneId = u64;

/// A rectangle in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// The rectangle grown by `d` on every side.
    pub fn inflate(&self, d: f32) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Panes side by side, separated by a vertical line.
    Horizontal,
    /// Panes stacked, separated by a horizontal line.
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, PartialEq)]
pub enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        /// Share of the space given to `first`, between 0 and 1.
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// The line between two panes, which can be dragged to resize them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Divider {
    pub rect: Rect,
    pub axis: Axis,
    /// Which split this belongs to; pass to [`Node::set_ratio`].
    pub index: usize,
    /// The area being divided, needed to turn a mouse position into a ratio.
    pub parent: Rect,
}

const MIN_RATIO: f32 = 0.1;

/// Splits `rect` into two parts with a `gap`-wide divider between them.
fn divide(rect: Rect, axis: Axis, ratio: f32, gap: f32) -> (Rect, Rect, Rect) {
    match axis {
        Axis::Horizontal => {
            let first_w = ((rect.w - gap) * ratio).round().max(0.0);
            let first = Rect::new(rect.x, rect.y, first_w, rect.h);
            let divider = Rect::new(rect.x + first_w, rect.y, gap, rect.h);
            let second_x = rect.x + first_w + gap;
            let second = Rect::new(second_x, rect.y, (rect.x + rect.w - second_x).max(0.0), rect.h);
            (first, divider, second)
        }
        Axis::Vertical => {
            let first_h = ((rect.h - gap) * ratio).round().max(0.0);
            let first = Rect::new(rect.x, rect.y, rect.w, first_h);
            let divider = Rect::new(rect.x, rect.y + first_h, rect.w, gap);
            let second_y = rect.y + first_h + gap;
            let second = Rect::new(rect.x, second_y, rect.w, (rect.y + rect.h - second_y).max(0.0));
            (first, divider, second)
        }
    }
}

impl Node {
    /// Computes the rectangle of every pane and divider inside `rect`.
    pub fn layout(&self, rect: Rect, gap: f32) -> Layout {
        let mut layout = Layout::default();
        let mut split_index = 0;
        self.layout_into(rect, gap, &mut split_index, &mut layout);
        layout
    }

    fn layout_into(&self, rect: Rect, gap: f32, split_index: &mut usize, out: &mut Layout) {
        match self {
            Node::Leaf(id) => out.panes.push((*id, rect)),
            Node::Split { axis, ratio, first, second } => {
                let index = *split_index;
                *split_index += 1;
                let (a, divider, b) = divide(rect, *axis, *ratio, gap);
                out.dividers.push(Divider { rect: divider, axis: *axis, index, parent: rect });
                first.layout_into(a, gap, split_index, out);
                second.layout_into(b, gap, split_index, out);
            }
        }
    }

    /// Pane ids in reading order.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    /// Replaces pane `target` with a split holding `target` and `new`.
    /// Returns false when `target` is not in this tree.
    pub fn split(&mut self, target: PaneId, axis: Axis, new: PaneId) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                *self = Node::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(Node::Leaf(target)),
                    second: Box::new(Node::Leaf(new)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.split(target, axis, new) || second.split(target, axis, new)
            }
        }
    }

    /// Removes pane `target`; its sibling takes over the freed space.
    ///
    /// Returns `None` when the tree is now empty (it was just that pane),
    /// otherwise the tree that remains.
    pub fn remove(self, target: PaneId) -> Option<Node> {
        match self {
            Node::Leaf(id) if id == target => None,
            leaf @ Node::Leaf(_) => Some(leaf),
            Node::Split { axis, ratio, first, second } => {
                match (first.remove(target), second.remove(target)) {
                    (Some(first), Some(second)) => Some(Node::Split {
                        axis,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }

    /// Sets the ratio of the split with the given index (see [`Divider`]).
    pub fn set_ratio(&mut self, index: usize, ratio: f32) {
        let mut counter = 0;
        self.set_ratio_inner(index, ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO), &mut counter);
    }

    fn set_ratio_inner(&mut self, index: usize, new_ratio: f32, counter: &mut usize) {
        if let Node::Split { ratio, first, second, .. } = self {
            if *counter == index {
                *ratio = new_ratio;
                return;
            }
            *counter += 1;
            first.set_ratio_inner(index, new_ratio, counter);
            second.set_ratio_inner(index, new_ratio, counter);
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Layout {
    pub panes: Vec<(PaneId, Rect)>,
    pub dividers: Vec<Divider>,
}

impl Layout {
    pub fn rect_of(&self, pane: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(id, _)| *id == pane).map(|(_, rect)| *rect)
    }

    pub fn pane_at(&self, x: f32, y: f32) -> Option<PaneId> {
        self.panes.iter().find(|(_, rect)| rect.contains(x, y)).map(|(id, _)| *id)
    }

    /// The pane next to `from` in the given direction: the closest one whose
    /// centre lies that way.
    pub fn neighbor(&self, from: PaneId, direction: Direction) -> Option<PaneId> {
        let (cx, cy) = self.rect_of(from)?.center();
        self.panes
            .iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|(id, rect)| {
                let (x, y) = rect.center();
                let (dx, dy) = (x - cx, y - cy);
                // Distance along the direction, and sideways drift.
                let (along, across) = match direction {
                    Direction::Left => (-dx, dy.abs()),
                    Direction::Right => (dx, dy.abs()),
                    Direction::Up => (-dy, dx.abs()),
                    Direction::Down => (dy, dx.abs()),
                };
                (along > 0.0).then_some((*id, along + across * 2.0))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0.0, 0.0, 201.0, 100.0);

    #[test]
    fn single_pane_fills_the_area() {
        let layout = Node::Leaf(1).layout(AREA, 1.0);
        assert_eq!(layout.panes, vec![(1, AREA)]);
        assert!(layout.dividers.is_empty());
    }

    #[test]
    fn split_shares_space_and_leaves_room_for_the_divider() {
        let mut root = Node::Leaf(1);
        assert!(root.split(1, Axis::Horizontal, 2));
        let layout = root.layout(AREA, 1.0);
        assert_eq!(layout.rect_of(1), Some(Rect::new(0.0, 0.0, 100.0, 100.0)));
        assert_eq!(layout.rect_of(2), Some(Rect::new(101.0, 0.0, 100.0, 100.0)));
        assert_eq!(layout.dividers[0].rect, Rect::new(100.0, 0.0, 1.0, 100.0));
    }

    #[test]
    fn splitting_an_unknown_pane_does_nothing() {
        let mut root = Node::Leaf(1);
        assert!(!root.split(9, Axis::Vertical, 2));
        assert_eq!(root, Node::Leaf(1));
    }

    #[test]
    fn removing_a_pane_gives_its_space_to_the_sibling() {
        let mut root = Node::Leaf(1);
        root.split(1, Axis::Horizontal, 2);
        root.split(2, Axis::Vertical, 3);
        assert_eq!(root.panes(), vec![1, 2, 3]);

        let root = root.remove(2).unwrap();
        assert_eq!(root.panes(), vec![1, 3]);
        let layout = root.layout(AREA, 1.0);
        assert_eq!(layout.rect_of(3), Some(Rect::new(101.0, 0.0, 100.0, 100.0)));

        let root = root.remove(1).unwrap();
        assert_eq!(root, Node::Leaf(3));
        assert_eq!(root.remove(3), None);
    }

    #[test]
    fn set_ratio_targets_the_right_split_and_is_clamped() {
        let mut root = Node::Leaf(1);
        root.split(1, Axis::Horizontal, 2);
        root.split(2, Axis::Vertical, 3);
        let dividers = root.layout(AREA, 1.0).dividers;
        assert_eq!(dividers.iter().map(|d| d.axis).collect::<Vec<_>>(), [Axis::Horizontal, Axis::Vertical]);

        root.set_ratio(dividers[1].index, 0.99);
        let layout = root.layout(AREA, 1.0);
        // Outer split untouched, inner one clamped to 90%.
        assert_eq!(layout.rect_of(1).unwrap().w, 100.0);
        assert_eq!(layout.rect_of(2).unwrap().h, (99.0f32 * 0.9).round());
    }

    #[test]
    fn neighbor_finds_the_pane_in_each_direction() {
        // 1 | 2
        //   | 3
        let mut root = Node::Leaf(1);
        root.split(1, Axis::Horizontal, 2);
        root.split(2, Axis::Vertical, 3);
        let layout = root.layout(AREA, 1.0);
        assert_eq!(layout.neighbor(1, Direction::Right), Some(2));
        assert_eq!(layout.neighbor(2, Direction::Down), Some(3));
        assert_eq!(layout.neighbor(3, Direction::Up), Some(2));
        assert_eq!(layout.neighbor(3, Direction::Left), Some(1));
        assert_eq!(layout.neighbor(1, Direction::Left), None);
        assert_eq!(layout.pane_at(150.0, 80.0), Some(3));
    }
}
