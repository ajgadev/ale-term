//! The agent map screen in the app: which agent it shows, gathering its
//! tree, and its clicks, keys and scrolling. Drawing is in `agent_map`.

use ale_agents::AgentKind;

use super::*;
use crate::agent_map::{self, Hit, View};

#[derive(Default)]
pub struct MapState {
    /// The pane whose agent is on show, chosen when the screen opens.
    pane: Option<PaneId>,
    /// How far the tree is scrolled, in pixels.
    pub scroll: (f32, f32),
}

impl App {
    /// The agent the map shows when opened: the focused one, or else the
    /// first with subagents at work.
    fn map_target(&self) -> Option<PaneId> {
        let focused = self.focused_pane_id().filter(|id| self.panes.get(id).is_some_and(|pane| pane.agent.is_some()));
        focused.or_else(|| {
            self.tabs.iter().flat_map(|tab| tab.root.panes()).find(|id| {
                self.panes.get(id).and_then(|pane| pane.agent.as_ref()).is_some_and(|s| s.running_subagents() > 0)
            })
        })
    }

    /// Shows the map, or puts it away.
    pub(super) fn toggle_agent_map(&mut self) {
        if matches!(self.overlay, Overlay::AgentMap) {
            return self.close_overlay();
        }
        self.map = MapState { pane: self.map_target(), scroll: (0.0, 0.0) };
        self.overlay = Overlay::AgentMap;
        self.redraw();
    }

    /// Subagents at work for the focused agent: the count on the tab
    /// bar's map button.
    pub(super) fn running_subagents_here(&self) -> usize {
        let id = self.focused_pane_id();
        id.and_then(|id| self.panes.get(&id)?.agent.as_ref()).map_or(0, |session| session.running_subagents())
    }

    /// Everything the screen draws.
    pub(super) fn map_view(&mut self) -> (Vec<crate::graph::Node>, String, Option<String>) {
        let Some(pane_id) = self.map.pane.filter(|id| self.panes.get(id).is_some_and(|pane| pane.agent.is_some())) else {
            let note = "No agent to show. Focus a pane with an agent in it, or start one with Shift+Cmd+T.";
            return (Vec::new(), String::new(), Some(note.to_owned()));
        };
        let nodes = self.agent_nodes(pane_id).unwrap_or_default();
        let Some(session) = self.panes.get(&pane_id).and_then(|pane| pane.agent.as_ref()) else {
            return (nodes, String::new(), None);
        };
        let launched = nodes.len().saturating_sub(1);
        let running = session.running_subagents();
        let mut summary = match launched {
            0 => "1 agent".to_owned(),
            n => format!("{} agents", n + 1),
        };
        if running > 0 {
            summary.push_str(&format!(", {running} running"));
        }
        if launched > 0 {
            summary.push_str(" · click an agent to see its output");
        }
        let note = match (launched, session.kind, session.hooked) {
            (0, AgentKind::Claude, true) => Some("No subagents yet. They appear here as soon as the agent launches them."),
            (0, AgentKind::Claude, false) => Some(
                "Subagents show here once Claude Code's status hooks are installed: \"Set Up Claude Code Status Hooks\" in the command palette.",
            ),
            (0, _, _) => Some("This agent does not report subagents, so only the agent itself is shown."),
            _ => None,
        };
        (nodes, summary, note.map(str::to_owned))
    }

    fn map_layout(&mut self) -> Option<agent_map::Layout> {
        let metrics = self.renderer.as_ref()?.metrics();
        let (nodes, summary, note) = self.map_view();
        let view = View { nodes: &nodes, summary, note, scroll: self.map.scroll, pointer: (0.0, 0.0) };
        let top = TabBar::height(self.scale());
        Some(agent_map::Layout::new(&view, self.window_size(), (metrics.width, metrics.height), self.scale(), top))
    }

    pub(super) fn map_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        self.map_layout()?.hit(x, y)
    }

    /// A click on the map: a box opens the graph panel on it, beside its
    /// agent's terminal, where its output can be read.
    pub(super) fn map_click(&mut self, x: f32, y: f32) {
        match self.map_hit(x, y) {
            Some(Hit::Close) => self.close_overlay(),
            Some(Hit::Node(index)) => {
                let Some(pane) = self.map.pane else { return };
                let (nodes, ..) = self.map_view();
                let Some(node) = nodes.into_iter().nth(index) else { return };
                self.close_overlay();
                if let Some(tab) = self.tabs.iter().find(|tab| tab.root.panes().contains(&pane)) {
                    self.go_to(crate::sidebar::Target { tab: tab.id, pane });
                }
                self.graph.selected = node.id;
                self.graph.visible = true;
                self.relayout();
            }
            None => {}
        }
    }

    /// Scrolls the tree by a wheel or trackpad movement.
    pub(super) fn map_scroll(&mut self, dx: f32, dy: f32) {
        let Some(layout) = self.map_layout() else { return };
        let (most_x, most_y) = layout.max_scroll();
        // Moving the fingers up shows what is further down, as in a list.
        let scroll = ((self.map.scroll.0 - dx).clamp(0.0, most_x), (self.map.scroll.1 - dy).clamp(0.0, most_y));
        if scroll != self.map.scroll {
            self.map.scroll = scroll;
            self.redraw();
        }
    }

    /// Keys of the map: the arrows scroll it. Returns whether the key was
    /// one of them.
    pub(super) fn map_key(&mut self, event: &KeyEvent) -> bool {
        let Some(cell) = self.renderer.as_ref().map(|r| r.metrics().height) else { return false };
        let step = cell * 3.0;
        let (dx, dy) = match &event.logical_key {
            Key::Named(NamedKey::ArrowUp) => (0.0, step),
            Key::Named(NamedKey::ArrowDown) => (0.0, -step),
            Key::Named(NamedKey::ArrowLeft) => (step, 0.0),
            Key::Named(NamedKey::ArrowRight) => (-step, 0.0),
            _ => return false,
        };
        self.map_scroll(dx, dy);
        true
    }

    /// Whether the map is open on an agent that is still at work, so its
    /// times and tokens need redrawing now and then.
    pub(super) fn map_busy(&self) -> bool {
        matches!(self.overlay, Overlay::AgentMap)
            && self.map.pane.and_then(|id| self.panes.get(&id)?.agent.as_ref()).is_some_and(|session| {
                session.state == ale_agents::AgentState::Working || session.running_subagents() > 0
            })
    }
}
