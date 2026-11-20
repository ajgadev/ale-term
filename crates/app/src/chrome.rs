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

pub struct TabBar {
    pub bar: Rect,
    /// The workspace name, left of the tabs; clicking it opens the
    /// workspace switcher.
    pub workspace: Rect,
    pub tabs: Vec<Rect>,
    /// The "+" button.
    pub new_tab: Rect,
}

/// What a tab shows.
pub struct TabLabel {
    pub text: String,
    /// Secondary text after the title: the agent's current activity.
    pub detail: Option<String>,
    /// Color of the status dot, for tabs with an agent.
    pub dot: Option<ale_config::Rgb>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabBarHit {
    Tab(usize),
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

        let tabs = (0..tab_count)
            .map(|i| Rect::new(left + i as f32 * width, 0.0, width, height))
            .collect();
        TabBar {
            bar: Rect::new(bar_x, 0.0, bar_width, height),
            workspace,
            tabs,
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

    pub fn draw(
        &self,
        r: &mut Renderer,
        workspace: &str,
        titles: &[TabLabel],
        active: usize,
        scale: f32,
        palette: &Palette,
    ) {
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
            let mut max_width = tab.w - padding * 2.0;
            // A dot in the agent's state color, when the tab holds one.
            if let Some(dot) = title.dot {
                r.text("●", false, x, text_y, metrics.width * 2.0, color(dot));
                x += metrics.width * 2.0;
                max_width -= metrics.width * 2.0;
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
