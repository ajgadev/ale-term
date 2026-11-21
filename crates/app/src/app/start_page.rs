//! The start page in the app: opening one in a new tab, typing into it,
//! and turning it into a real session (an agent, or a plain terminal).

use ale_agents::AgentKind;

use super::*;
use crate::menu::{Menu, MenuKind};
use crate::start::{self, Hit, StartPage};

impl App {
    pub(super) fn start_page(&self) -> Option<&StartPage> {
        self.tabs.get(self.active)?.start.as_ref()
    }

    fn start_page_mut(&mut self) -> Option<&mut StartPage> {
        self.tabs.get_mut(self.active)?.start.as_mut()
    }

    /// Opens a new tab showing the start page. Nothing is started until
    /// the user chooses what to run.
    pub(super) fn open_start_page(&mut self) {
        self.open_start_page_in(None);
    }

    /// Opens a start page for `directory`, or for the current folder.
    pub(super) fn open_start_page_in(&mut self, directory: Option<PathBuf>) {
        let directory = directory
            .filter(|directory| directory.is_dir())
            .or_else(|| self.inherited_directory())
            .or_else(|| self.workspace(self.active_ws).and_then(|workspace| workspace.root.clone()))
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));
        let agent = AgentKind::from_command(&self.config.agents.new_session_command).unwrap_or(AgentKind::Claude);
        // The tab has no pane yet; this id stands in for one and is never
        // given to a real pane.
        let placeholder = self.next_id();
        let workspace = if self.tabs.is_empty() {
            self.workspace_for(workspace::project_root(&directory))
        } else {
            self.active_ws
        };
        let index = self.add_tab(self.active + 1, placeholder, workspace);
        self.tabs[index].start = Some(StartPage { text: String::new(), agent, directory });
        self.activate(index);
    }

    /// Replaces the start page with a shell, and if `with_agent`, starts
    /// the chosen agent in it on what was typed.
    pub(super) fn start_launch(&mut self, with_agent: bool) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Some(page) = tab.start.take() else { return };
        let directory = Some(page.directory.clone()).filter(|directory| directory.is_dir());
        let Some(id) = self.spawn_pane(directory, self.content_rect()) else {
            // Could not start a shell: leave the page as it was.
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.start = Some(page);
            }
            return;
        };
        let tab = &mut self.tabs[self.active];
        tab.root = Node::Leaf(id);
        tab.focused = id;
        if with_agent {
            let command =
                start::launch_command(page.agent, &self.config.agents.new_session_command, &page.text);
            self.type_when_ready(id, format!("{command}\r").into_bytes());
        }
        self.sync_workspace(self.active);
        self.relayout();
    }

    /// Adds typed or pasted text to the page. Returns false when the
    /// active tab is not a start page.
    pub(super) fn start_type(&mut self, text: &str) -> bool {
        let Some(page) = self.start_page_mut() else { return false };
        // Line breaks are kept (a prompt can have several lines); other
        // control characters are dropped.
        page.text.extend(text.chars().filter(|c| *c == '\n' || !c.is_control()));
        self.redraw();
        true
    }

    /// Keyboard handling while a start page is showing. Returns true when
    /// the active tab is a start page (the key is then never sent on).
    pub(super) fn start_key(&mut self, event: &KeyEvent) -> bool {
        if self.start_page().is_none() {
            return false;
        }
        let (shift, command) = (self.modifiers.shift_key(), self.modifiers.super_key());
        match &event.logical_key {
            Key::Named(NamedKey::Enter) if command => self.start_launch(false),
            Key::Named(NamedKey::Enter) if shift => {
                self.start_type("\n");
            }
            Key::Named(NamedKey::Enter) => self.start_launch(true),
            Key::Named(NamedKey::Tab) => self.open_agent_menu(),
            Key::Named(NamedKey::Backspace) => {
                if let Some(page) = self.start_page_mut() {
                    if command {
                        page.text.clear();
                    } else {
                        page.text.pop();
                    }
                }
            }
            // Esc closes the page, unless it is the only tab (closing the
            // last tab would only bring the page back).
            Key::Named(NamedKey::Escape) => {
                if self.tabs.len() > 1 {
                    self.close_tab(self.active);
                }
            }
            _ => {
                let plain = !command && !self.modifiers.control_key();
                if let (true, Some(text)) = (plain, event.text.as_deref()) {
                    self.start_type(text);
                }
            }
        }
        self.redraw();
        true
    }

    /// Handles a click while a start page is showing. Returns true when
    /// the active tab is a start page and the click was inside it.
    pub(super) fn start_click(&mut self, x: f32, y: f32) -> bool {
        let area = self.content_rect();
        if self.start_page().is_none() || !area.contains(x, y) {
            return false;
        }
        let Some(metrics) = self.renderer.as_ref().map(|r| r.metrics()) else { return true };
        let scale = self.scale();
        let cell = (metrics.width, metrics.height);
        let hit = self.start_page().and_then(|page| start::hit(page, area, cell, scale, x, y));
        match hit {
            Some(Hit::Terminal) => self.start_launch(false),
            Some(Hit::Send) => self.start_launch(true),
            Some(Hit::Agent) => self.open_agent_menu(),
            Some(Hit::Input) | None => {}
        }
        self.redraw();
        true
    }

    /// Opens the list of agents under the start page's picker.
    pub(super) fn open_agent_menu(&mut self) {
        let area = self.content_rect();
        let Some(metrics) = self.renderer.as_ref().map(|r| r.metrics()) else { return };
        let scale = self.scale();
        let Some(page) = self.start_page() else { return };
        let anchor = start::agent_button(page, area, (metrics.width, metrics.height), scale);
        self.overlay = Overlay::Menu(Box::new(Menu::agents(anchor, page.agent)));
        self.redraw();
    }

    /// Sets the agent the start page will run.
    pub(super) fn start_set_agent(&mut self, agent: AgentKind) {
        if let Some(page) = self.start_page_mut() {
            page.agent = agent;
        }
        self.redraw();
    }

    /// What the start page under the pointer looks like: the hovered
    /// control and whether its menu is open.
    pub(super) fn start_view(&self) -> start::View {
        let picking = matches!(&self.overlay, Overlay::Menu(menu) if menu.kind == MenuKind::Agent);
        let hovered = if self.overlay.is_open() { None } else { self.start_hit(self.mouse.x, self.mouse.y) };
        start::View { hovered, picking }
    }

    /// The start page control at a window position.
    pub(super) fn start_hit(&self, x: f32, y: f32) -> Option<Hit> {
        let area = self.content_rect();
        let metrics = self.renderer.as_ref()?.metrics();
        let page = self.start_page()?;
        start::hit(page, area, (metrics.width, metrics.height), self.scale(), x, y)
    }
}
