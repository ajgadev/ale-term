//! The start page in the app: opening one in a new tab, typing into it,
//! and turning it into a real session (an agent, or a plain terminal).

use std::path::Path;

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
        let agent = self.available_agent(agent);
        self.list_files(directory.clone());
        self.tabs[index].start = Some(StartPage::new(agent, directory));
        self.activate(index);
        self.check_installed_agents();
    }

    // ---- Files: @ references, the paperclip and the folder ---------------

    /// Lists a folder's files in the background, for `@` references; they
    /// arrive as `UserEvent::FilesListed`.
    fn list_files(&self, directory: PathBuf) {
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new().name("list-files".into()).spawn(move || {
            let files = crate::mention::list_files(&directory);
            let _ = proxy.send_event(UserEvent::FilesListed(directory, Arc::new(files)));
        });
        if let Err(e) = spawned {
            log::warn!("cannot list files: {e}");
        }
    }

    /// A folder's files arrived: start pages for that folder can offer them.
    pub(super) fn files_listed(&mut self, directory: &Path, files: Arc<Vec<String>>) {
        for tab in &mut self.tabs {
            if let Some(page) = tab.start.as_mut().filter(|page| page.directory == directory) {
                page.files = Some(files.clone());
            }
        }
        self.redraw();
    }

    /// Puts the highlighted (or clicked) file into the prompt in place of
    /// the reference being typed.
    fn start_accept_suggestion(&mut self, index: Option<usize>) {
        let Some(page) = self.start_page_mut() else { return };
        let files = page.suggestions();
        let Some(file) = files.get(index.unwrap_or(page.suggestion)).or(files.first()) else { return };
        page.text = crate::mention::accept(&page.text, file);
        page.text_changed();
        self.redraw();
    }

    /// The paperclip: asks macOS for files (the standard open panel) and
    /// adds them to the prompt as references. The panel runs in a separate
    /// helper so the window stays responsive while it is open.
    fn choose_files(&mut self) {
        let Some((tab, directory)) = self.start_target() else { return };
        let script = [
            "on run argv",
            "set chosen to choose file with prompt \"Attach files to the prompt\" default location (POSIX file (item 1 of argv)) with multiple selections allowed",
            "set out to \"\"",
            "repeat with f in chosen",
            "set out to out & POSIX path of f & linefeed",
            "end repeat",
            "return out",
            "end run",
        ];
        self.ask_macos(&script, directory, move |output| {
            let files: Vec<PathBuf> = output.lines().filter(|line| !line.is_empty()).map(PathBuf::from).collect();
            (!files.is_empty()).then(|| UserEvent::FilesChosen(tab, files))
        });
    }

    /// "Choose folder…": asks macOS for a folder for the session.
    pub(super) fn choose_folder(&mut self) {
        let Some((tab, directory)) = self.start_target() else { return };
        let script = [
            "on run argv",
            "return POSIX path of (choose folder with prompt \"Run the session in\" default location (POSIX file (item 1 of argv)))",
            "end run",
        ];
        self.ask_macos(&script, directory, move |output| {
            let folder = output.trim_end_matches('\n');
            let folder = if folder.len() > 1 { folder.trim_end_matches('/') } else { folder };
            (!folder.is_empty()).then(|| UserEvent::FolderChosen(tab, PathBuf::from(folder)))
        });
    }

    /// The active start page's tab and folder.
    fn start_target(&self) -> Option<(TabId, PathBuf)> {
        let tab = self.tabs.get(self.active)?;
        Some((tab.id, tab.start.as_ref()?.directory.clone()))
    }

    /// Runs an AppleScript (`lines`) with `argument` as its one argument,
    /// on a helper thread, and sends what `answer` makes of its output.
    /// Cancelling the dialog makes the script fail, which sends nothing.
    /// The argument is passed separately, never spliced into the script.
    fn ask_macos(&self, lines: &[&str], argument: PathBuf, answer: impl FnOnce(&str) -> Option<UserEvent> + Send + 'static) {
        let mut command = std::process::Command::new("osascript");
        for line in lines {
            command.arg("-e").arg(line);
        }
        command.arg("--").arg(argument);
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new().name("ask-macos".into()).spawn(move || {
            let Ok(output) = command.stdin(std::process::Stdio::null()).output() else { return };
            if !output.status.success() {
                return;
            }
            if let Some(event) = answer(&String::from_utf8_lossy(&output.stdout)) {
                let _ = proxy.send_event(event);
            }
        });
        if let Err(e) = spawned {
            log::warn!("cannot show the macOS dialog: {e}");
        }
    }

    /// Files picked with the paperclip: added to that tab's prompt.
    pub(super) fn files_chosen(&mut self, tab: TabId, files: Vec<PathBuf>) {
        let Some(page) = self.tabs.iter_mut().find(|t| t.id == tab).and_then(|t| t.start.as_mut()) else { return };
        let references: Vec<String> = files.iter().map(|file| crate::mention::reference_to(file, &page.directory)).collect();
        if !page.text.is_empty() && !page.text.ends_with(char::is_whitespace) {
            page.text.push(' ');
        }
        page.text.push_str(&references.join(" "));
        page.text.push(' ');
        page.text_changed();
        self.redraw();
    }

    /// Moves a start page's session to another folder.
    pub(super) fn start_set_folder(&mut self, tab: TabId, folder: PathBuf) {
        if !folder.is_dir() {
            return;
        }
        let Some(page) = self.tabs.iter_mut().find(|t| t.id == tab).and_then(|t| t.start.as_mut()) else { return };
        if page.directory != folder {
            page.directory = folder.clone();
            page.files = None;
            page.text_changed();
            self.list_files(folder);
        }
        self.redraw();
    }

    /// Opens the list of folders under the start page's folder button: the
    /// projects of the open workspaces, then "Choose folder…".
    pub(super) fn open_folder_menu(&mut self) {
        let area = self.content_rect();
        let Some(metrics) = self.renderer.as_ref().map(|r| r.metrics()) else { return };
        let scale = self.scale();
        let Some(page) = self.start_page() else { return };
        let anchor = start::folder_button(page, area, (metrics.width, metrics.height), scale);
        let mut choices: Vec<(String, PathBuf)> = Vec::new();
        let roots = std::iter::once(page.directory.clone())
            .chain(self.workspaces.iter().filter_map(|workspace| workspace.root.clone()));
        for root in roots {
            if !choices.iter().any(|(_, path)| *path == root) && root.is_dir() {
                let name = root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned());
                choices.push((name, root));
            }
        }
        let menu = Menu::folders(anchor, &page.directory, &choices);
        self.overlay = Overlay::Menu(Box::new(menu));
        self.redraw();
    }

    /// Looks up which agents are installed, in the background, unless that
    /// was done in the last half minute. The answer arrives as
    /// `UserEvent::AgentsFound`.
    pub(super) fn check_installed_agents(&mut self) {
        if self.agents_checked.is_some_and(|at| at.elapsed() < Duration::from_secs(30)) {
            return;
        }
        self.agents_checked = Some(Instant::now());
        let proxy = self.proxy.clone();
        let shell = self.config.terminal.shell.clone();
        let spawned = std::thread::Builder::new().name("find-agents".into()).spawn(move || {
            let found = ale_agents::available::detect(shell.as_deref());
            let _ = proxy.send_event(UserEvent::AgentsFound(found));
        });
        if let Err(e) = spawned {
            log::warn!("cannot look for installed agents: {e}");
        }
    }

    /// The installed agents arrived: start pages that picked an agent
    /// that is not installed switch to one that is, and an open agent menu
    /// is refreshed.
    pub(super) fn agents_found(&mut self, found: Vec<ale_agents::available::Installed>) {
        log::info!("installed agents: {:?}", found.iter().map(|f| (f.kind.name(), &f.location)).collect::<Vec<_>>());
        self.installed_agents = Some(found);
        let replacements: Vec<(usize, AgentKind)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| {
                let current = tab.start.as_ref()?.agent;
                let available = self.available_agent(current);
                (available != current).then_some((index, available))
            })
            .collect();
        for (index, agent) in replacements {
            if let Some(page) = self.tabs[index].start.as_mut() {
                page.agent = agent;
            }
        }
        if matches!(&self.overlay, Overlay::Menu(menu) if menu.kind == MenuKind::Agent) {
            self.open_agent_menu();
        }
        self.redraw();
    }

    /// The agents to offer, each with where it is installed: those found
    /// installed, or every agent while that is not known yet (or nothing
    /// was found, in which case it is better to offer all than none).
    fn agent_choices(&self) -> Vec<(AgentKind, String)> {
        match &self.installed_agents {
            Some(found) if !found.is_empty() => found.iter().map(|f| (f.kind, f.location.clone())).collect(),
            _ => AgentKind::ALL.iter().map(|kind| (*kind, String::new())).collect(),
        }
    }

    /// `wanted`, if it can be offered, or else the first agent that can.
    fn available_agent(&self, wanted: AgentKind) -> AgentKind {
        let choices = self.agent_choices();
        if choices.iter().any(|(kind, _)| *kind == wanted) {
            wanted
        } else {
            choices.first().map_or(wanted, |(kind, _)| *kind)
        }
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
        page.text_changed();
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
        // While an `@` reference is being typed, the file list has the
        // arrows, Enter, Tab and Esc.
        let offered = self.start_page().map_or(0, |page| page.suggestions().len());
        let referencing = self.start_page().is_some_and(StartPage::referencing);
        if referencing && !command {
            let mut handled = true;
            match &event.logical_key {
                Key::Named(NamedKey::ArrowDown) if offered > 0 => {
                    if let Some(page) = self.start_page_mut() {
                        page.suggestion = (page.suggestion + 1) % offered;
                    }
                }
                Key::Named(NamedKey::ArrowUp) if offered > 0 => {
                    if let Some(page) = self.start_page_mut() {
                        page.suggestion = (page.suggestion + offered - 1) % offered;
                    }
                }
                Key::Named(NamedKey::Enter | NamedKey::Tab) if offered > 0 && !shift => self.start_accept_suggestion(None),
                Key::Named(NamedKey::Escape) => {
                    if let Some(page) = self.start_page_mut() {
                        page.suggestions_closed = true;
                    }
                }
                _ => handled = false,
            }
            if handled {
                self.redraw();
                return true;
            }
        }
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
                    page.text_changed();
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
            Some(Hit::Attach) => self.choose_files(),
            Some(Hit::Folder) => self.open_folder_menu(),
            Some(Hit::Suggestion(index)) => self.start_accept_suggestion(Some(index)),
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
        let menu = Menu::agents(anchor, page.agent, &self.agent_choices());
        self.overlay = Overlay::Menu(Box::new(menu));
        self.check_installed_agents();
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
        let picking_folder = matches!(&self.overlay, Overlay::Menu(menu) if menu.kind == MenuKind::Folder);
        let hovered = if self.overlay.is_open() { None } else { self.start_hit(self.mouse.x, self.mouse.y) };
        start::View { hovered, picking, picking_folder }
    }

    /// The start page control at a window position.
    pub(super) fn start_hit(&self, x: f32, y: f32) -> Option<Hit> {
        let area = self.content_rect();
        let metrics = self.renderer.as_ref()?.metrics();
        let page = self.start_page()?;
        start::hit(page, area, (metrics.width, metrics.height), self.scale(), x, y)
    }
}
