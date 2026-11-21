//! Workspace behaviour of the app: which workspace each tab belongs to,
//! switching between them, templates, and saving/restoring the session.
//!
//! All tabs live in one list (`App::tabs`); each carries the id of its
//! workspace, and the tab bar shows only those of the active workspace.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ale_config::{REPO_TEMPLATE_FILE, TabTemplate};

use super::{App, Tab};
use crate::actions::{self, Action};
use crate::layout::{Node, PaneId};
use crate::overlay::{Confirm, InputKind, ListKind, Overlay};
use crate::workspace::{
    self, GENERAL, SESSION_VERSION, SavedNode, SavedPane, SavedTab, SavedWorkspace, Session,
    Workspace, WorkspaceId,
};

/// One row of a list overlay.
pub struct ListItem {
    pub title: String,
    pub detail: String,
    pub choice: Choice,
}

/// What picking a row does.
#[derive(Clone, Copy)]
pub enum Choice {
    Run(Action),
    Workspace(WorkspaceId),
    /// Index into the config's workspace templates.
    Template(usize),
    MoveTabTo(WorkspaceId),
    /// Index into the session archive.
    Archived(usize),
}

/// A path with the home folder shortened to `~`.
fn display_path(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
    }
    path.display().to_string()
}

impl App {
    pub(super) fn next_id(&mut self) -> u64 {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        id
    }

    /// Indices (into `tabs`) of the tabs in the active workspace, in order.
    pub(super) fn visible_tabs(&self) -> Vec<usize> {
        (0..self.tabs.len()).filter(|i| self.tabs[*i].workspace == self.active_ws).collect()
    }

    pub(super) fn workspace(&self, id: WorkspaceId) -> Option<&Workspace> {
        self.workspaces.iter().find(|workspace| workspace.id == id)
    }

    pub(super) fn workspace_name(&self) -> &str {
        self.workspace(self.active_ws).map_or(GENERAL, |workspace| workspace.name.as_str())
    }

    fn tab_count(&self, id: WorkspaceId) -> usize {
        self.tabs.iter().filter(|tab| tab.workspace == id).count()
    }

    /// The workspace that gathers tabs of the project at `root` (or of no
    /// project, for `None`), created if it does not exist yet.
    pub(super) fn workspace_for(&mut self, root: Option<PathBuf>) -> WorkspaceId {
        if let Some(existing) = self.workspaces.iter().find(|w| !w.manual && w.root == root) {
            return existing.id;
        }
        let id = self.next_id();
        let name = root.as_deref().map_or_else(|| GENERAL.to_owned(), workspace::root_name);
        self.workspaces.push(Workspace { id, name, root, manual: false, last_tab: None });
        id
    }

    /// Adds a tab holding `pane` at position `index` in the tab list.
    pub(super) fn add_tab(&mut self, index: usize, pane: PaneId, workspace: WorkspaceId) -> usize {
        let id = self.next_id();
        let index = index.min(self.tabs.len());
        let tab = Tab {
            id,
            workspace,
            root: Node::Leaf(pane),
            focused: pane,
            custom_title: None,
            pinned: false,
            start: None,
        };
        self.tabs.insert(index, tab);
        // Keep pointing at the same tab if one was inserted before it.
        if index <= self.active && self.tabs.len() > 1 {
            self.active += 1;
        }
        index
    }

    /// Shows the tab at `index`, switching workspace if it is in another.
    pub(super) fn activate(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else { return };
        let (tab_id, workspace_id) = (tab.id, tab.workspace);
        self.active = index;
        self.active_ws = workspace_id;
        if let Some(workspace) = self.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            workspace.last_tab = Some(tab_id);
        }
        self.mark_seen();
        self.mark_dirty();
        self.redraw();
    }

    pub(super) fn switch_workspace(&mut self, id: WorkspaceId) {
        let Some(workspace) = self.workspace(id) else { return };
        let last = workspace.last_tab;
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.workspace == id && Some(tab.id) == last)
            .or_else(|| self.tabs.iter().position(|tab| tab.workspace == id));
        if let Some(index) = index {
            self.activate(index);
        }
    }

    pub(super) fn cycle_workspace(&mut self, offset: isize) {
        let count = self.workspaces.len() as isize;
        let Some(current) = self.workspaces.iter().position(|w| w.id == self.active_ws) else { return };
        if count > 1 {
            let next = (current as isize + offset).rem_euclid(count) as usize;
            self.switch_workspace(self.workspaces[next].id);
        }
    }

    /// Moves a tab to the workspace of the project its folder is in.
    ///
    /// Called when a tab's folder may have changed. Pinned tabs and tabs in
    /// hand-made workspaces stay where they are. When the tab being moved
    /// is the one on screen, the view follows it, so `cd`-ing into a
    /// project brings up that project's tabs.
    pub(super) fn sync_workspace(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else { return };
        let current = self.workspace(tab.workspace);
        if tab.pinned || current.is_some_and(|workspace| workspace.manual) {
            return;
        }
        let Some(directory) = self.panes.get(&tab.focused).and_then(|p| p.terminal.working_directory())
        else {
            return;
        };
        let root = workspace::project_root(&directory);
        if current.is_some_and(|workspace| workspace.root == root) {
            return;
        }
        let target = self.workspace_for(root);
        self.move_tab(index, target);
    }

    /// Moves the tab `id` to `position` among the tabs on screen.
    pub(super) fn reorder_tab(&mut self, id: crate::workspace::TabId, position: usize) {
        let visible = self.visible_tabs();
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else { return };
        let Some(current) = visible.iter().position(|&index| index == from) else { return };
        let position = position.min(visible.len() - 1);
        if current == position {
            return;
        }
        // Taking the tab out shifts later tabs down by one, so inserting
        // at the target's old place puts it after the target when moving
        // right and before it when moving left: both what is wanted.
        let target = visible[position];
        let active = self.tabs.get(self.active).map(|tab| tab.id);
        let tab = self.tabs.remove(from);
        self.tabs.insert(target, tab);
        if let Some(index) = self.tabs.iter().position(|tab| Some(tab.id) == active) {
            self.active = index;
        }
        self.mark_dirty();
        self.redraw();
    }

    pub(super) fn move_tab(&mut self, index: usize, target: WorkspaceId) {
        let Some(tab) = self.tabs.get_mut(index) else { return };
        if tab.workspace == target {
            return;
        }
        tab.workspace = target;
        if index == self.active {
            self.activate(index);
        }
        self.prune_workspaces();
        self.mark_dirty();
        self.redraw();
    }

    /// Fixes up the active tab after the tab that was at `index` (in
    /// `workspace`) has been removed from the list.
    pub(super) fn after_tab_removed(&mut self, index: usize, workspace: WorkspaceId, was_active: bool) {
        if self.tabs.is_empty() {
            self.workspaces.clear();
            return;
        }
        if !was_active {
            if index < self.active {
                self.active -= 1;
            }
        } else {
            // Prefer the neighbour in the same workspace; if that was the
            // workspace's last tab, fall back to the workspace before it.
            let same = |i: &usize| self.tabs[*i].workspace == workspace;
            let next = (index..self.tabs.len()).find(same).or_else(|| (0..index).rev().find(same));
            let next = next.unwrap_or_else(|| {
                let position = self.workspaces.iter().position(|w| w.id == workspace).unwrap_or(0);
                let fallback = self
                    .workspaces
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| w.id != workspace)
                    .min_by_key(|(i, _)| position.abs_diff(*i))
                    .map(|(_, w)| (w.id, w.last_tab));
                fallback
                    .and_then(|(id, last)| {
                        let in_workspace = |tab: &&Tab| tab.workspace == id;
                        self.tabs
                            .iter()
                            .position(|tab| tab.workspace == id && Some(tab.id) == last)
                            .or_else(|| self.tabs.iter().position(|tab| in_workspace(&tab)))
                    })
                    .unwrap_or(0)
            });
            self.activate(next);
        }
        self.prune_workspaces();
        self.mark_dirty();
    }

    /// Drops workspaces that have no tabs left.
    fn prune_workspaces(&mut self) {
        let used: Vec<WorkspaceId> = self.tabs.iter().map(|tab| tab.workspace).collect();
        self.workspaces.retain(|workspace| used.contains(&workspace.id));
    }

    // ---- List overlays ------------------------------------------------------

    /// The rows of a list overlay matching `query`, best match first.
    pub(super) fn list_items(&self, kind: ListKind, query: &str) -> Vec<ListItem> {
        let mut items = Vec::new();
        match kind {
            ListKind::Commands => {
                return actions::search(query)
                    .into_iter()
                    .map(|(action, name, keys)| ListItem {
                        title: (*name).to_owned(),
                        detail: (*keys).to_owned(),
                        choice: Choice::Run(*action),
                    })
                    .collect();
            }
            ListKind::Workspaces => {
                for workspace in &self.workspaces {
                    let count = self.tab_count(workspace.id);
                    let tabs = if count == 1 { "1 tab".to_owned() } else { format!("{count} tabs") };
                    let place = workspace.root.as_deref().map(display_path);
                    items.push(ListItem {
                        title: workspace.name.clone(),
                        detail: place.map_or(tabs.clone(), |place| format!("{tabs} · {place}")),
                        choice: Choice::Workspace(workspace.id),
                    });
                }
                // Templates for projects that are not open yet.
                for (index, template) in self.config.workspaces.iter().enumerate() {
                    let root = template.root_path();
                    if self.workspaces.iter().any(|w| !w.manual && w.root.as_deref() == Some(&root)) {
                        continue;
                    }
                    items.push(ListItem {
                        title: template.display_name(),
                        detail: format!("open · {}", display_path(&root)),
                        choice: Choice::Template(index),
                    });
                }
            }
            ListKind::Archive => {
                let now = ale_agents::archive::now();
                for (index, session) in self.archive.sessions().iter().enumerate() {
                    let agent = session.kind().map_or(session.agent.as_str(), |kind| kind.name());
                    let when = ale_agents::archive::time_ago(now, session.ended_at);
                    items.push(ListItem {
                        title: session.title.clone(),
                        detail: format!("{agent} · {} · {when}", session.project),
                        choice: Choice::Archived(index),
                    });
                }
            }
            ListKind::MoveTab => {
                for workspace in self.workspaces.iter().filter(|w| w.id != self.active_ws) {
                    items.push(ListItem {
                        title: workspace.name.clone(),
                        detail: workspace.root.as_deref().map(display_path).unwrap_or_default(),
                        choice: Choice::MoveTabTo(workspace.id),
                    });
                }
            }
        }
        let mut scored: Vec<(i32, ListItem)> = items
            .into_iter()
            .filter_map(|item| actions::fuzzy_score(query, &item.title).map(|score| (score, item)))
            .collect();
        scored.sort_by_key(|(score, _)| -score);
        scored.into_iter().map(|(_, item)| item).collect()
    }

    /// Carries out the chosen row, except commands (the caller runs those,
    /// since some need the event loop).
    pub(super) fn choose(&mut self, choice: Choice) {
        match choice {
            Choice::Run(_) => {}
            Choice::Workspace(id) => self.switch_workspace(id),
            Choice::Template(index) => self.open_template(index),
            Choice::Archived(index) => self.resume_archived(index),
            Choice::MoveTabTo(id) => {
                // A deliberate move sticks: the tab no longer follows its
                // folder.
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.pinned = true;
                }
                self.move_tab(self.active, id);
            }
        }
    }

    pub(super) fn submit_input(&mut self, kind: InputKind, text: &str) {
        let text = text.trim();
        match kind {
            InputKind::RenameTab => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.custom_title = (!text.is_empty()).then(|| text.to_owned());
                }
            }
            InputKind::RenameWorkspace => {
                if !text.is_empty()
                    && let Some(workspace) = self.workspaces.iter_mut().find(|w| w.id == self.active_ws)
                {
                    workspace.name = text.to_owned();
                }
            }
            InputKind::NewWorkspace => {
                if text.is_empty() {
                    return;
                }
                let directory = self.inherited_directory();
                let Some(pane) = self.spawn_pane(directory, self.content_rect()) else { return };
                let id = self.next_id();
                self.workspaces.push(Workspace {
                    id,
                    name: text.to_owned(),
                    root: None,
                    manual: true,
                    last_tab: None,
                });
                let index = self.add_tab(self.tabs.len(), pane, id);
                self.activate(index);
            }
        }
        self.mark_dirty();
        self.redraw();
    }

    // ---- Templates ----------------------------------------------------------

    pub(super) fn show_message(&mut self, title: &str, lines: Vec<String>) {
        self.overlay = Overlay::Confirm(Box::new(Confirm {
            title: title.to_owned(),
            lines,
            hint: "Press Enter or Esc to close".into(),
            template: None,
            install_hooks: false,
        }));
        self.redraw();
    }

    /// Opens the tabs of a template in `workspace`, running their commands.
    /// With no tabs listed, opens one plain tab.
    fn apply_tabs(&mut self, workspace: WorkspaceId, root: &Path, tabs: &[TabTemplate]) {
        let plain = [TabTemplate::default()];
        let tabs = if tabs.is_empty() { &plain[..] } else { tabs };
        let mut first = None;
        for template in tabs {
            let directory = match &template.dir {
                Some(dir) => root.join(dir),
                None => root.to_path_buf(),
            };
            let Some(pane) = self.spawn_pane(Some(directory), self.content_rect()) else { continue };
            let index = self.add_tab(self.tabs.len(), pane, workspace);
            let tab = &mut self.tabs[index];
            tab.custom_title = template.title.clone();
            // Template tabs belong to their workspace wherever they `cd`.
            tab.pinned = true;
            if let Some(command) = &template.command {
                self.type_when_ready(pane, format!("{command}\r").into_bytes());
            }
            first.get_or_insert(index);
        }
        if let Some(first) = first {
            self.activate(first);
        }
    }

    /// Opens a workspace from a template in the config file.
    fn open_template(&mut self, index: usize) {
        let Some(template) = self.config.workspaces.get(index).cloned() else { return };
        let root = template.root_path();
        if !root.is_dir() {
            return self.show_message("Folder not found", vec![display_path(&root)]);
        }
        let workspace = self.workspace_for(Some(root.clone()));
        if let Some(entry) = self.workspaces.iter_mut().find(|w| w.id == workspace) {
            entry.name = template.display_name();
        }
        self.apply_tabs(workspace, &root, &template.tabs);
    }

    /// Adds the template's tabs to the current workspace. The template
    /// comes from the config file or, failing that, from the project's own
    /// `.ale-term.toml`, which must be approved before it may run commands.
    pub(super) fn set_up_workspace(&mut self) {
        let Some(root) = self.workspace(self.active_ws).and_then(|w| w.root.clone()) else {
            return self.show_message(
                "No project here",
                vec!["Templates apply to workspaces that belong to a project folder.".into()],
            );
        };
        let workspace = self.active_ws;
        if let Some(template) = self.config.workspaces.iter().find(|t| t.root_path() == root).cloned() {
            return self.apply_tabs(workspace, &root, &template.tabs);
        }

        let file = root.join(REPO_TEMPLATE_FILE);
        let Ok(contents) = std::fs::read_to_string(&file) else {
            return self.show_message(
                "No template for this workspace",
                vec![
                    "Add a [[workspace]] block to the settings file (Cmd+,),".into(),
                    format!("or a {REPO_TEMPLATE_FILE} file to the project."),
                ],
            );
        };
        let tabs = match ale_config::parse_repo_template(&contents) {
            Ok(tabs) => tabs,
            Err(error) => {
                let lines = error.lines().take(6).map(str::to_owned).collect();
                return self.show_message(&format!("Cannot read {REPO_TEMPLATE_FILE}"), lines);
            }
        };
        if self.approvals.is_approved(&root, &contents) {
            return self.apply_tabs(workspace, &root, &tabs);
        }

        // Show exactly what would run and let the user decide.
        let project = workspace::root_name(&root);
        let mut lines = vec![format!("{REPO_TEMPLATE_FILE} in \"{project}\" wants to open these tabs:"), String::new()];
        for tab in tabs.iter().take(12) {
            let title = tab.title.as_deref().unwrap_or("tab");
            lines.push(match &tab.command {
                Some(command) => format!("  {title}: {command}"),
                None => format!("  {title}"),
            });
        }
        if tabs.len() > 12 {
            lines.push(format!("  ... and {} more", tabs.len() - 12));
        }
        self.overlay = Overlay::Confirm(Box::new(Confirm {
            title: "Run this project's commands?".into(),
            lines,
            hint: "Enter: allow and run  ·  Esc: cancel".into(),
            template: Some((root, contents, tabs)),
            install_hooks: false,
        }));
        self.redraw();
    }

    /// The user pressed Enter on a confirmation.
    pub(super) fn confirm_accepted(&mut self) {
        let Overlay::Confirm(confirm) = std::mem::replace(&mut self.overlay, Overlay::None) else { return };
        if confirm.install_hooks {
            self.install_hooks();
        }
        if let Some((root, contents, tabs)) = confirm.template {
            if let Some(dir) = &self.state_dir
                && let Err(e) = self.approvals.approve(&root, &contents, dir)
            {
                log::warn!("approval not saved: {e}");
            }
            let workspace = self.workspace_for(Some(root.clone()));
            self.apply_tabs(workspace, &root, &tabs);
        }
        self.redraw();
    }

    // ---- Session ------------------------------------------------------------

    /// Notes that the layout changed; it is written to disk a moment later,
    /// so a burst of changes costs one write.
    pub(super) fn mark_dirty(&mut self) {
        if self.save_at.is_none() {
            self.save_at = Some(Instant::now() + Duration::from_secs(2));
        }
    }

    /// Writes the current workspaces, tabs and splits to disk. Only folders
    /// and command lines are recorded, never terminal contents.
    pub(super) fn save_session(&mut self) {
        self.save_at = None;
        if !self.config.window.restore {
            return;
        }
        let Some(dir) = self.state_dir.clone() else { return };
        if self.tabs.iter().all(|tab| tab.start.is_some()) {
            // Everything was closed: next launch starts fresh.
            let _ = std::fs::remove_file(dir.join("session.json"));
            return;
        }
        let describe = |id: PaneId| -> SavedPane {
            let Some(pane) = self.panes.get(&id) else { return SavedPane::default() };
            // For an agent whose conversation can be resumed, offer that
            // rather than the command that originally started it.
            let resume = pane
                .agent
                .as_ref()
                .and_then(|agent| ale_agents::archive::resume_command(agent.kind, agent.session_id.as_deref()));
            SavedPane {
                cwd: pane.terminal.working_directory(),
                command: resume
                    .or_else(|| pane.terminal.shell().command.filter(|command| !command.trim().is_empty())),
            }
        };
        let mut session = Session { version: SESSION_VERSION, active_workspace: 0, workspaces: Vec::new() };
        for workspace in &self.workspaces {
            // A start page has nothing to restore.
            let tabs: Vec<&Tab> =
                self.tabs.iter().filter(|tab| tab.workspace == workspace.id && tab.start.is_none()).collect();
            if tabs.is_empty() {
                continue;
            }
            if workspace.id == self.active_ws {
                session.active_workspace = session.workspaces.len();
            }
            session.workspaces.push(SavedWorkspace {
                name: workspace.name.clone(),
                root: workspace.root.clone(),
                manual: workspace.manual,
                active_tab: tabs.iter().position(|tab| Some(tab.id) == workspace.last_tab).unwrap_or(0),
                tabs: tabs
                    .iter()
                    .map(|tab| SavedTab {
                        title: tab.custom_title.clone(),
                        pinned: tab.pinned,
                        focused: tab.root.panes().iter().position(|id| *id == tab.focused).unwrap_or(0),
                        layout: SavedNode::from_node(&tab.root, &describe),
                    })
                    .collect(),
            });
        }
        if let Err(e) = session.save(&dir) {
            log::warn!("session not saved: {e}");
        }
    }

    /// Reopens the saved session. Returns false when there is none.
    ///
    /// Each pane starts a fresh shell in its old folder. A command that was
    /// running is typed at the prompt but not run: press Enter to start it
    /// again, or clear the line.
    pub(super) fn restore_session(&mut self) -> bool {
        let Some(session) = self.state_dir.as_deref().and_then(Session::load) else { return false };
        let rect = self.content_rect();
        let mut active = None;
        for (position, saved) in session.workspaces.iter().enumerate() {
            let id = self.next_id();
            self.workspaces.push(Workspace {
                id,
                name: saved.name.clone(),
                root: saved.root.clone(),
                manual: saved.manual,
                last_tab: None,
            });
            for (tab_position, saved_tab) in saved.tabs.iter().enumerate() {
                let mut spawn = |pane: &SavedPane| -> Option<PaneId> {
                    let directory = pane.cwd.clone().filter(|cwd| cwd.is_dir());
                    let id = self.spawn_pane(directory, rect)?;
                    if let Some(command) = &pane.command {
                        self.type_when_ready(id, command.clone().into_bytes());
                    }
                    Some(id)
                };
                let Some(root) = saved_tab.layout.to_node(&mut spawn) else { continue };
                let panes = root.panes();
                let index = self.add_tab(self.tabs.len(), panes[0], id);
                let tab = &mut self.tabs[index];
                tab.root = root;
                tab.focused = panes[saved_tab.focused.min(panes.len() - 1)];
                tab.custom_title = saved_tab.title.clone();
                tab.pinned = saved_tab.pinned;
                let tab_id = tab.id;
                if tab_position == saved.active_tab || self.workspaces.last().is_some_and(|w| w.last_tab.is_none()) {
                    if let Some(workspace) = self.workspaces.last_mut() {
                        workspace.last_tab = Some(tab_id);
                    }
                    if position == session.active_workspace {
                        active = Some(id);
                    }
                }
            }
        }
        self.prune_workspaces();
        if self.tabs.is_empty() {
            return false;
        }
        let target = active.filter(|id| self.workspace(*id).is_some()).unwrap_or(self.workspaces[0].id);
        self.active = 0;
        self.active_ws = self.tabs[0].workspace;
        self.switch_workspace(target);
        self.relayout();
        true
    }
}
