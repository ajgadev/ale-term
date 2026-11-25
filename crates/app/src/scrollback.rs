//! Sharing one memory budget between the panes' scrollback.
//!
//! A line of history costs memory across the pane's whole width whatever is
//! written on it, so "lines per pane" says little about memory: one wide
//! pane with 5,000 lines holds some 24 MB, and every pane may. Here the
//! panes share a budget instead, and each is told how many lines it may
//! hold so that together they stay within it.
//!
//! The limits are in force before the history grows rather than applied
//! afterwards, because memory that was once used is not handed back to the
//! system when freed: it stays with the program for later use. Cutting
//! history therefore makes room for other panes, but only never having
//! held it keeps the program small.
//!
//! How the budget is shared:
//!
//! * A pane keeps what it holds, and a pane whose history is growing asks
//!   for some more.
//! * While all of that fits, what is left over is split evenly, as room to
//!   grow into.
//! * Once it does not fit, the panes asking for the most are held to a
//!   common level, losing their oldest lines; panes below that level keep
//!   everything. So a pane that sat on most of the budget makes way, a
//!   step at a time, for one that is growing, until they hold the same.
//!
//! What is not counted: a pane whose history is still growing has up to a
//! thousand lines kept ready by the terminal emulator. They are freed once
//! the pane is full or quiet (see `Terminal::limit_history`).

/// The fewest lines a pane keeps, however many panes share the budget.
pub const FEWEST_LINES: usize = 100;

/// The part of the budget a growing pane asks for beyond what it holds.
const STEP: usize = 4;

/// The scrollback of one pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    /// Lines of history it holds.
    pub lines: usize,
    /// Bytes one line takes.
    pub line_bytes: usize,
    /// Its history is growing, or would be if it were allowed more.
    pub growing: bool,
}

/// How many lines each pane may hold so that together they stay within
/// `budget` bytes, never more than `most` (the per-pane setting) and never
/// fewer than [`FEWEST_LINES`] (or `most`, if that is less).
pub fn limits(panes: &[Held], budget: usize, most: usize) -> Vec<usize> {
    let wanted = |pane: &Held| {
        let more = if pane.growing { budget / STEP } else { 0 };
        (pane.lines * pane.line_bytes + more).min(most * pane.line_bytes)
    };
    let mut smallest_first: Vec<usize> = panes.iter().map(wanted).collect();
    smallest_first.sort_unstable();
    // Pour the budget out, smallest first: a pane wanting less than an
    // equal share of what is left takes only that.
    let mut left = budget;
    let mut level = None;
    for (index, wanted) in smallest_first.iter().enumerate() {
        let share = left / (smallest_first.len() - index);
        if *wanted > share {
            level = Some(share);
            break;
        }
        left -= wanted;
    }
    panes
        .iter()
        .map(|pane| {
            let bytes = match level {
                Some(level) => wanted(pane).min(level),
                None => wanted(pane) + left / panes.len(),
            };
            (bytes / pane.line_bytes.max(1)).max(FEWEST_LINES).min(most)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: usize = 1000;

    fn quiet(lines: usize) -> Held {
        Held { lines, line_bytes: LINE, growing: false }
    }

    fn growing(lines: usize) -> Held {
        Held { growing: true, ..quiet(lines) }
    }

    /// The lines the panes may hold together.
    fn total(panes: &[Held], budget_lines: usize) -> usize {
        limits(panes, budget_lines * LINE, 5000).iter().sum()
    }

    #[test]
    fn what_is_left_over_is_room_to_grow() {
        // 600 of 1000 lines' worth held: the 400 left are split evenly.
        assert_eq!(limits(&[quiet(100), quiet(500)], 1000 * LINE, 5000), [300, 700]);
        // The per-pane setting still applies.
        assert_eq!(limits(&[quiet(100), quiet(500)], 1000 * LINE, 600), [300, 600]);
        // A growing pane asks for a quarter of the budget first.
        assert_eq!(limits(&[growing(100), quiet(500)], 1000 * LINE, 5000), [425, 575]);
        assert_eq!(limits(&[], 1000 * LINE, 5000), Vec::<usize>::new());
    }

    #[test]
    fn the_panes_never_exceed_the_budget_together() {
        assert_eq!(total(&[quiet(100), quiet(500)], 1000), 1000);
        assert_eq!(total(&[growing(100), growing(500), quiet(300)], 1000), 1000);
        assert_eq!(total(&[growing(3000), quiet(200), growing(4000)], 2000), 2000);
        assert_eq!(total(&[quiet(0), quiet(0), quiet(0), quiet(0)], 2000), 2000);
    }

    #[test]
    fn the_largest_panes_are_cut_to_a_common_level() {
        // The small pane keeps its 200; the others share the rest.
        assert_eq!(limits(&[quiet(3000), quiet(200), quiet(4000)], 2000 * LINE, 5000), [900, 200, 900]);
    }

    #[test]
    fn a_pane_holding_most_of_the_budget_makes_way_for_a_growing_one() {
        let budget = 1600 * LINE;
        // The growing pane asks for 400 more; the other gives them up.
        assert_eq!(limits(&[quiet(1500), growing(100)], budget, 5000), [1100, 500]);
        // And so on, until they hold the same.
        assert_eq!(limits(&[quiet(1100), growing(500)], budget, 5000), [800, 800]);
        assert_eq!(limits(&[quiet(800), growing(800)], budget, 5000), [800, 800]);
    }

    #[test]
    fn wide_panes_get_fewer_lines_for_the_same_memory() {
        let panes = [Held { line_bytes: 2 * LINE, ..quiet(4000) }, quiet(4000)];
        assert_eq!(limits(&panes, 2000 * LINE, 5000), [500, 1000]);
    }

    #[test]
    fn a_pane_always_keeps_a_little() {
        assert_eq!(limits(&[quiet(4000); 50], 1000 * LINE, 5000), [FEWEST_LINES; 50]);
        assert_eq!(limits(&[growing(0)], 0, 5000), [FEWEST_LINES]);
        // Unless the setting asks for less.
        assert_eq!(limits(&[quiet(40)], 0, 50), [50]);
    }
}
