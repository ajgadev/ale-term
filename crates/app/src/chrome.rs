//! The window chrome: for now, the tab bar across the top.
//!
//! Layout (where each tab is) is computed by one function that both drawing
//! and mouse hit-testing use, so the two can never disagree.

use ale_config::Palette;
use ale_render::Renderer;

use crate::draw::color;
use crate::layout::Rect;

/// Sizes in points; multiplied by the display scale factor when used.
const BAR_HEIGHT: f32 = 38.0;
/// Room left for the macOS close/minimize/zoom buttons.
const TRAFFIC_LIGHTS: f32 = 78.0;
const TAB_MAX_WIDTH: f32 = 240.0;
const TAB_PADDING: f32 = 14.0;
const WORKSPACE_MAX_WIDTH: f32 = 220.0;
const CLOSE_SIZE: f32 = 20.0;

pub struct TabBar {
    pub bar: Rect,
    /// The workspace name, left of the tabs; clicking it opens the
    /// workspace switcher.
    pub workspace: Rect,
    pub tabs: Vec<Rect>,
    /// The close button of each tab, at its right end.
    pub close: Vec<Rect>,
    /// The "+" button.
    pub new_tab: Rect,
}

/// What the tab bar shows at the moment.
pub struct TabBarView<'a> {
    /// Name of the workspace whose tabs these are.
    pub workspace: &'a str,
    pub titles: &'a [TabLabel],
    /// Position of the tab on screen.
    pub active: usize,
    /// What the pointer is over, for showing a tab's close button.
    pub hovered: Option<TabBarHit>,
}

/// What a tab shows.
pub struct TabLabel {
    pub text: String,
    /// Secondary text after the title: the agent's current activity.
    pub detail: Option<String>,
    /// Color of the status dot, for tabs with an agent.
    pub dot: Option<ale_config::Rgb>,
    /// Image key of the agent's icon (see `icons`), for tabs with one.
    pub icon: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabBarHit {
    Tab(usize),
    /// The close button of a tab.
    Close(usize),
    NewTab,
    Workspace,
    /// Empty space, used for dragging the window.
    Empty,
}

impl TabBar {
    /// Computes where everything in the tab bar goes.
    ///
    /// The bar spans from `bar_x` (the sidebar's right edge, or 0) to the
    /// window's right edge. `traffic_lights` says whether the macOS window
    /// buttons sit at the bar's left end and need room. `workspace_width`
    /// is the width of the workspace label's text.
    pub fn layout(
        tab_count: usize,
        window_width: f32,
        scale: f32,
        bar_x: f32,
        traffic_lights: bool,
        workspace_width: f32,
    ) -> TabBar {
        let height = (BAR_HEIGHT * scale).round();
        let lights = bar_x + if traffic_lights { (TRAFFIC_LIGHTS * scale).round() } else { 0.0 };
        let padding = (TAB_PADDING * scale).round();
        let bar_width = (window_width - bar_x).max(0.0);
        let label = (workspace_width + padding * 2.0).min(WORKSPACE_MAX_WIDTH * scale).min(bar_width * 0.3);
        let workspace = Rect::new(lights, 0.0, label.round(), height);
        let left = lights + workspace.w;
        let button = height;
        let available = (window_width - left - button).max(0.0);
        let width = (available / tab_count.max(1) as f32).min(TAB_MAX_WIDTH * scale).floor();

        let tabs: Vec<Rect> = (0..tab_count)
            .map(|i| Rect::new(left + i as f32 * width, 0.0, width, height))
            .collect();
        // A square at the right end of each tab, unless tabs are too
        // narrow to spare the room.
        let close_size = (CLOSE_SIZE * scale).round();
        let close = tabs
            .iter()
            .map(|tab| {
                if tab.w < close_size * 3.0 {
                    return Rect::default();
                }
                let x = tab.x + tab.w - close_size - (6.0 * scale).round();
                Rect::new(x, ((height - close_size) / 2.0).round(), close_size, close_size)
            })
            .collect();
        TabBar {
            bar: Rect::new(bar_x, 0.0, bar_width, height),
            workspace,
            tabs,
            close,
            new_tab: Rect::new(left + tab_count as f32 * width, 0.0, button, height),
        }
    }

    pub fn height(scale: f32) -> f32 {
        (BAR_HEIGHT * scale).round()
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<TabBarHit> {
        if !self.bar.contains(x, y) {
            return None;
        }
        if let Some(index) = self.close.iter().position(|close| close.contains(x, y)) {
            return Some(TabBarHit::Close(index));
        }
        if let Some(index) = self.tabs.iter().position(|tab| tab.contains(x, y)) {
            return Some(TabBarHit::Tab(index));
        }
        if self.new_tab.contains(x, y) {
            return Some(TabBarHit::NewTab);
        }
        if self.workspace.contains(x, y) {
            return Some(TabBarHit::Workspace);
        }
        Some(TabBarHit::Empty)
    }

    /// The text shown for a workspace, with a marker hinting it is a menu.
    pub fn workspace_label(name: &str) -> String {
        format!("{name} ▾")
    }

    pub fn draw(&self, r: &mut Renderer, view: &TabBarView, scale: f32, palette: &Palette) {
        let TabBarView { workspace, titles, active, hovered } = *view;
        let metrics = r.metrics();
        let line = scale.round().max(1.0);
        let bar = self.bar;
        let text_y = ((bar.h - metrics.height) / 2.0).round();
        let padding = (TAB_PADDING * scale).round();

        r.rect(bar.x, bar.y, bar.w, bar.h, color(palette.chrome));
        r.rect(bar.x, bar.h - line, bar.w, line, color(palette.border));

        // Workspace label.
        let label = fit(&Self::workspace_label(workspace), self.workspace.w - padding * 2.0, metrics.width);
        r.text(&label, true, self.workspace.x + padding, text_y, self.workspace.w - padding, color(palette.accent));

        for (index, (tab, title)) in self.tabs.iter().zip(titles).enumerate() {
            let is_active = index == active;
            if is_active {
                // The active tab takes the terminal's background and covers
                // the bar's bottom border, so it reads as part of the pane.
                r.rect(tab.x, tab.y, tab.w, tab.h, color(palette.chrome_active));
                r.rect(tab.x, tab.y, tab.w, line * 2.0, color(palette.accent));
                r.rect(tab.x - line, tab.y, line, tab.h, color(palette.border));
                r.rect(tab.x + tab.w - line, tab.y, line, tab.h, color(palette.border));
            } else if index + 1 != active && index + 1 != self.tabs.len() {
                // A short separator between two inactive tabs.
                let inset = (tab.h * 0.3).round();
                r.rect(tab.x + tab.w - line, tab.y + inset, line, tab.h - inset * 2.0, color(palette.border));
            }

            let text_color = if is_active { palette.foreground } else { palette.chrome_text_dim };
            let mut x = tab.x + padding;
            let close = self.close[index];
            // The title stops short of the close button, shown or not, so
            // it does not shift when the mouse passes over.
            let right = if close.w > 0.0 { close.x - (4.0 * scale).round() } else { tab.x + tab.w - padding };
            let mut max_width = (right - x).max(0.0);
            // The agent's icon with a dot in its state's color, or just
            // the dot when there is no icon.
            match (title.icon, title.dot) {
                (Some(icon), dot) => {
                    let size = metrics.height.round();
                    // The icon plus a little air before the title.
                    let slot = size + (metrics.width * 0.75).round();
                    let icon_y = ((bar.h - size) / 2.0).round();
                    r.image(icon, x, icon_y, size, size, [0.0, 0.0, 1.0, 1.0]);
                    if let Some(dot) = dot {
                        let radius = (3.0 * scale).round();
                        // Ringed in the tab's color so it reads over the icon.
                        let behind = if is_active { palette.chrome_active } else { palette.chrome };
                        crate::draw::dot(r, x + size, icon_y + radius, radius + line, color(behind));
                        crate::draw::dot(r, x + size, icon_y + radius, radius, color(dot));
                    }
                    x += slot;
                    max_width -= slot;
                }
                (None, Some(dot)) => {
                    r.text("●", false, x, text_y, metrics.width * 2.0, color(dot));
                    x += metrics.width * 2.0;
                    max_width -= metrics.width * 2.0;
                }
                (None, None) => {}
            }
            let label = fit(&title.text, max_width, metrics.width);
            let used = r.text(&label, is_active, x, text_y, max_width, color(text_color));
            // What the agent is doing, in the space that is left.
            if let Some(detail) = &title.detail {
                let room = max_width - used - metrics.width * 2.0;
                if room > metrics.width * 8.0 {
                    let detail = fit(detail, room, metrics.width);
                    r.text(&detail, false, x + used + metrics.width * 2.0, text_y, room, color(palette.chrome_text_dim));
                }
            }
        }

        // Close buttons: on the active tab, and on the one under the mouse.
        for (index, close) in self.close.iter().enumerate() {
            let over_tab = matches!(hovered, Some(TabBarHit::Tab(i) | TabBarHit::Close(i)) if i == index);
            if close.w <= 0.0 || !(index == active || over_tab) {
                continue;
            }
            let over_button = hovered == Some(TabBarHit::Close(index));
            if over_button {
                r.rect(close.x, close.y, close.w, close.h, color(palette.border));
            }
            let glyph_x = close.x + ((close.w - metrics.width) / 2.0).round();
            let glyph_y = close.y + ((close.h - metrics.height) / 2.0).round();
            let glyph_color = if over_button { palette.foreground } else { palette.chrome_text_dim };
            r.text("×", false, glyph_x, glyph_y, close.w, color(glyph_color));
        }

        let plus_x = self.new_tab.x + ((self.new_tab.w - metrics.width) / 2.0).round();
        r.text("+", false, plus_x, text_y, self.new_tab.w, color(palette.chrome_text_dim));
    }
}

/// Shortens `text` with an ellipsis so it fits in `max_width` pixels.
fn fit(text: &str, max_width: f32, cell_width: f32) -> String {
    let max_cells = (max_width / cell_width).floor() as usize;
    let count = text.chars().count();
    if count <= max_cells {
        return text.to_owned();
    }
    if max_cells == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(max_cells - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_share_the_width_up_to_a_maximum() {
        // An 72px label plus 14px padding each side: tabs start at 78+100.
        let bar = TabBar::layout(2, 1000.0, 1.0, 0.0, true, 72.0);
        assert_eq!(bar.workspace, Rect::new(78.0, 0.0, 100.0, 38.0));
        assert_eq!(bar.tabs[0], Rect::new(178.0, 0.0, 240.0, 38.0));
        assert_eq!(bar.tabs[1].x, 418.0);
        assert_eq!(bar.new_tab.x, 658.0);

        // Many tabs shrink to fit and never run past the window.
        let bar = TabBar::layout(20, 1000.0, 1.0, 0.0, true, 72.0);
        assert!(bar.new_tab.x + bar.new_tab.w <= 1000.0);

        // Beside a 260px sidebar the bar starts at its edge, with no room
        // reserved for the window buttons (they sit over the sidebar).
        let bar = TabBar::layout(1, 1000.0, 1.0, 260.0, false, 72.0);
        assert_eq!(bar.bar, Rect::new(260.0, 0.0, 740.0, 38.0));
        assert_eq!(bar.workspace.x, 260.0);
        assert_eq!(bar.hit(100.0, 10.0), None);
        assert!(bar.new_tab.x + bar.new_tab.w <= 1000.0);
    }

    #[test]
    fn hit_testing_matches_layout() {
        let bar = TabBar::layout(2, 1000.0, 1.0, 0.0, true, 72.0);
        assert_eq!(bar.hit(100.0, 10.0), Some(TabBarHit::Workspace));
        assert_eq!(bar.hit(200.0, 10.0), Some(TabBarHit::Tab(0)));
        // Each tab has a close button at its right end.
        let close = bar.close[0];
        assert!(close.x > bar.tabs[0].x + 150.0 && close.x + close.w <= bar.tabs[0].x + bar.tabs[0].w);
        assert_eq!(bar.hit(close.x + 2.0, close.y + 2.0), Some(TabBarHit::Close(0)));
        // Very narrow tabs have none, and the whole tab selects.
        let crowded = TabBar::layout(40, 1000.0, 1.0, 0.0, true, 72.0);
        assert_eq!(crowded.close[0].w, 0.0);
        let first = crowded.tabs[0];
        assert_eq!(crowded.hit(first.x + first.w - 2.0, 10.0), Some(TabBarHit::Tab(0)));
        assert_eq!(bar.hit(500.0, 10.0), Some(TabBarHit::Tab(1)));
        assert_eq!(bar.hit(670.0, 10.0), Some(TabBarHit::NewTab));
        assert_eq!(bar.hit(900.0, 10.0), Some(TabBarHit::Empty));
        assert_eq!(bar.hit(20.0, 10.0), Some(TabBarHit::Empty));
        assert_eq!(bar.hit(100.0, 60.0), None);
    }

    #[test]
    fn long_titles_are_shortened() {
        assert_eq!(fit("hello", 80.0, 8.0), "hello");
        assert_eq!(fit("hello world", 64.0, 8.0), "hello w…");
        assert_eq!(fit("hello", 0.0, 8.0), "");
    }
}
