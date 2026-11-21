//! The board layout: the agents as a board docked above the tabs, in place
//! of the sidebar, and the switch between the two layouts.

use ale_agents::AgentState;
use ale_config::{AgentLayout, SettingValue};

use super::*;
use crate::board;
use crate::dock::{self, Hit};

/// What the user has done with the docked board.
#[derive(Default)]
pub struct DockState {
    /// How many cards each column is scrolled past.
    pub scroll: [usize; board::COLUMNS],
    /// Trackpad movement not yet amounting to a card.
    pub scroll_remainder: f32,
    /// What the pointer is over, for hover highlights.
    pub hovered: Option<Hit>,
}

impl App {
    pub(super) fn board_layout(&self) -> bool {
        self.config.agents.layout == AgentLayout::Board
    }

    /// The rows set in the settings.
    fn dock_rows(&self) -> usize {
        (self.config.agents.board_rows as usize).clamp(dock::MIN_ROWS, dock::MAX_ROWS)
    }

    /// The rows shown: those set, or fewer in a short window, so the
    /// board never takes more than 60% of the height.
    fn dock_rows_shown(&self) -> usize {
        let Some(metrics) = self.renderer.as_ref().map(|r| r.metrics()) else { return dock::MIN_ROWS };
        let (cell, scale, limit) = ((metrics.width, metrics.height), self.scale(), self.window_size().1 * 0.6);
        (dock::MIN_ROWS..=self.dock_rows())
            .rev()
            .find(|rows| dock::height(cell, scale, *rows) <= limit)
            .unwrap_or(dock::MIN_ROWS)
    }

    /// Height of the docked board: none in the sidebar layout or while the
    /// board is hidden (Cmd+B).
    pub(super) fn dock_height(&self) -> f32 {
        if !self.board_layout() || !self.sidebar.visible {
            return 0.0;
        }
        let Some(metrics) = self.renderer.as_ref().map(|r| r.metrics()) else { return 0.0 };
        dock::height((metrics.width, metrics.height), self.scale(), self.dock_rows_shown()).round()
    }

    pub(super) fn dock_rect(&self) -> Option<Rect> {
        let height = self.dock_height();
        (height > 0.0).then(|| Rect::new(0.0, 0.0, self.window_size().0, height))
    }

    /// Whether the macOS window buttons sit on the docked board's header.
    pub(super) fn dock_lights(&self) -> bool {
        self.window.as_ref().is_none_or(|w| w.fullscreen().is_none())
    }

    /// Every agent as a card, in the board's columns.
    pub(super) fn board_cards(&self) -> [Vec<board::Card>; board::COLUMNS] {
        let now = Instant::now();
        let mut columns: [Vec<board::Card>; board::COLUMNS] = Default::default();
        for tab in &self.tabs {
            let project = self.workspace(tab.workspace).map_or("", |w| w.name.as_str());
            for pane_id in tab.root.panes() {
                let Some(pane) = self.panes.get(&pane_id) else { continue };
                let Some(session) = &pane.agent else { continue };
                columns[board::column_of(session.state)].push(board::Card {
                    target: crate::sidebar::Target { tab: tab.id, pane: pane_id },
                    state: session.state,
                    agent: session.kind.name().to_owned(),
                    title: self.agent_title(pane, session),
                    project: project.to_owned(),
                    status: Self::agent_status(session),
                    elapsed: crate::sidebar::format_elapsed(now.duration_since(session.since).as_secs()),
                    // Spins while the agent is busy.
                    icon: self.agent_icon(session),
                    tab: tab.custom_title.clone(),
                });
            }
        }
        columns
    }

    pub(super) fn dock_model(&self) -> dock::Model {
        let columns = self.board_cards();
        let agents: usize = columns.iter().map(Vec::len).sum();
        let needing = columns[board::column_of(AgentState::NeedsYou)].len();
        let mut workspaces: Vec<WorkspaceId> = Vec::new();
        for tab in &self.tabs {
            let has_agent = tab.root.panes().iter().any(|id| self.panes.get(id).is_some_and(|p| p.agent.is_some()));
            if has_agent && !workspaces.contains(&tab.workspace) {
                workspaces.push(tab.workspace);
            }
        }
        let plural = |n: usize, word: &str| if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") };
        let mut summary = match agents {
            0 => "no agents running".to_owned(),
            _ => format!("{} in {}", plural(agents, "agent"), plural(workspaces.len(), "workspace")),
        };
        if needing > 0 {
            summary.push_str(&format!(" · {needing} need{} you", if needing == 1 { "s" } else { "" }));
        }
        let current = self.tabs.get(self.active).and_then(|tab| {
            let pane = self.panes.get(&tab.focused)?;
            pane.agent.as_ref()?;
            Some(crate::sidebar::Target { tab: tab.id, pane: tab.focused })
        });
        dock::Model {
            columns,
            current,
            rows: self.dock_rows_shown(),
            // A short window already shows fewer rows than set.
            can_grow: self.dock_rows_shown() == self.dock_rows(),
            scroll: self.dock.scroll,
            summary,
            hovered: self.dock.hovered,
        }
    }

    pub(super) fn dock_hit(&self, x: f32, y: f32) -> Option<Hit> {
        let rect = self.dock_rect()?;
        let metrics = self.renderer.as_ref()?.metrics();
        dock::hit(&self.dock_model(), rect, (metrics.width, metrics.height), self.scale(), x, y)
    }

    pub(super) fn dock_click(&mut self, hit: Hit) {
        match hit {
            Hit::Card(target) => {
                self.go_to(target);
                self.reveal_graph(target.pane, None);
            }
            Hit::FewerRows => self.set_board_rows(self.dock_rows_shown().saturating_sub(1)),
            Hit::MoreRows if self.dock_rows_shown() == self.dock_rows() => {
                self.set_board_rows(self.dock_rows() + 1);
            }
            Hit::MoreRows => {}
            Hit::TitleBar => {
                let clicks = self.register_click();
                if let Some(window) = &self.window {
                    if clicks == 2 {
                        window.set_maximized(!window.is_maximized());
                    } else {
                        let _ = window.drag_window();
                    }
                }
            }
            Hit::Column(_) => {}
        }
        self.redraw();
    }

    /// Scrolls the column under the pointer by whole cards.
    pub(super) fn dock_scroll(&mut self, column: usize, pixels: f32) {
        let Some(metrics) = self.renderer.as_ref().map(|r| r.metrics()) else { return };
        // A card is about three lines tall; scroll one per that much movement.
        let step = metrics.height * 3.0;
        self.dock.scroll_remainder += pixels;
        let cards = (self.dock.scroll_remainder / step).trunc();
        self.dock.scroll_remainder -= cards * step;
        if cards == 0.0 {
            return;
        }
        let count = self.board_cards().get(column).map_or(0, Vec::len);
        let limit = dock::max_scroll(count, self.dock_rows_shown());
        let Some(scroll) = self.dock.scroll.get_mut(column) else { return };
        // Moving the fingers up shows later cards, as in a list.
        *scroll = (*scroll as isize - cards as isize).clamp(0, limit as isize) as usize;
        self.redraw();
    }

    /// Switches between the sidebar and board layouts, saving the choice
    /// in the settings file like the settings screen does.
    pub(super) fn set_layout(&mut self, layout: AgentLayout) {
        // Either way, show the agents: that is what the switch is for.
        let hidden = !self.sidebar.visible;
        self.sidebar.visible = true;
        self.sidebar.searching = false;
        if layout == self.config.agents.layout {
            if hidden {
                self.relayout();
            }
            return;
        }
        let word = match layout {
            AgentLayout::Sidebar => "sidebar",
            AgentLayout::Board => "board",
        };
        if !self.write_setting("agents", "layout", SettingValue::Text(word.into())) {
            // Not saved (the error is on screen); still switch for now.
            self.config.agents.layout = layout;
        }
        self.relayout();
    }

    pub(super) fn toggle_layout(&mut self) {
        let other = match self.config.agents.layout {
            AgentLayout::Sidebar => AgentLayout::Board,
            AgentLayout::Board => AgentLayout::Sidebar,
        };
        self.set_layout(other);
    }

    fn set_board_rows(&mut self, rows: usize) {
        let rows = rows.clamp(dock::MIN_ROWS, dock::MAX_ROWS);
        if rows == self.dock_rows() {
            return;
        }
        if !self.write_setting("agents", "board_rows", SettingValue::Integer(rows as i64)) {
            self.config.agents.board_rows = rows as u32;
        }
        self.relayout();
    }

    /// Test hook for visual checks: four more tabs, for `demo_agents`.
    /// Run it a moment before, so their shells have started: a shell that
    /// shows its first prompt ends a pretend session in its pane.
    pub(super) fn demo_tabs(&mut self) {
        for _ in 0..4 {
            self.new_tab(None);
        }
        self.select_tab(0);
    }

    /// Test hook for visual checks: pretends an agent runs in every pane
    /// (see `demo_tabs` for more panes), in a spread of states.
    pub(super) fn demo_agents(&mut self) {
        use ale_agents::{AgentKind, Session};
        let examples = [
            (AgentKind::Claude, AgentState::NeedsYou, "docs nav", Some("Allow edit to nav.tsx?")),
            (AgentKind::Claude, AgentState::Working, "pricing page", Some("Editing pricing.css")),
            (AgentKind::Codex, AgentState::Done, "blog pass", None),
            (AgentKind::OpenCode, AgentState::Idle, "tests", None),
            (AgentKind::Claude, AgentState::Working, "doc comments", Some("Reading lib.rs")),
            (AgentKind::Codex, AgentState::Done, "og tags", None),
            (AgentKind::Gemini, AgentState::Working, "refactor parser", Some("Running cargo test")),
        ];
        let now = Instant::now();
        let mut panes = Vec::new();
        // Newest first: the first tab's shells may still report their
        // prompt, which ends a pretend session.
        for tab in self.tabs.iter().rev() {
            panes.extend(tab.root.panes());
        }
        for (pane_id, (kind, state, title, activity)) in panes.into_iter().zip(examples) {
            let Some(pane) = self.panes.get_mut(&pane_id) else { continue };
            let mut session = Session::new(kind, now);
            session.state = state;
            session.hooked = true;
            session.title = Some(title.to_owned());
            session.activity = activity.map(str::to_owned);
            pane.agent = Some(session);
        }
        self.redraw();
    }
}
