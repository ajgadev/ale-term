//! Coding agents in the app: noticing them, following their status,
//! alerting the user, and the sidebar that lists them.

use std::time::{Duration, Instant};

use ale_agents::channel::{self, ENV_BIN, ENV_PANE, ENV_SOCKET};
use ale_agents::{AgentKind, AgentState, Change, HookMessage, Server, Session, hooks, install};

use super::*;
use ale_agents::ArchivedSession;
use ale_agents::archive;

use crate::board;
use crate::overlay::Confirm;
use crate::sidebar::{self, Filter, Group, Hit, Model, Row, Target};

/// What the user has chosen in the sidebar.
#[derive(Default)]
pub struct SidebarState {
    pub visible: bool,
    pub filter: Filter,
    pub by_status: bool,
    pub query: String,
    /// The search box has the keyboard.
    pub searching: bool,
    pub scroll: f32,
}

/// Order of the groups when the sidebar is grouped by status: what needs
/// the user first.
const STATUS_ORDER: [AgentState; 7] = [
    AgentState::NeedsYou,
    AgentState::Error,
    AgentState::RateLimited,
    AgentState::Working,
    AgentState::WaitingOnSubagents,
    AgentState::Done,
    AgentState::Idle,
];

impl App {
    // ---- Receiving status ---------------------------------------------------

    /// Starts listening for status reports from agent hooks.
    pub(super) fn start_agent_server(&mut self) {
        let Some(state_dir) = self.state_dir.clone() else { return };
        channel::remove_stale_sockets(&state_dir);
        let path = channel::socket_path(&state_dir);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Socket paths are limited to about 100 characters. If the state
        // folder is too deep for that, use the temporary folder instead;
        // hooks then find us through the environment only.
        let fallback = std::env::temp_dir().join(format!("ale-term-{}.sock", std::process::id()));
        for candidate in [path, fallback] {
            let proxy = self.proxy.clone();
            match Server::start(candidate, move |message| {
                let _ = proxy.send_event(UserEvent::Agent(message));
            }) {
                Ok(server) => {
                    self.agent_server = Some(server);
                    break;
                }
                Err(e) => log::info!("cannot listen for agent status here: {e}"),
            }
        }
        if self.agent_server.is_none() {
            log::warn!("agent status is unavailable: no usable socket location");
        }
        if let Some(path) = install::claude_settings_path() {
            self.claude_hooks = install::claude_installed_at(&path);
        }
        self.hooks_declined = state_dir.join("hooks-declined").exists();
    }

    /// Environment variables that let a hook find this window and say which
    /// pane it is in.
    pub(super) fn agent_env(&self, pane: PaneId) -> Vec<(String, String)> {
        let Some(server) = &self.agent_server else { return Vec::new() };
        let mut env = vec![
            (ENV_SOCKET.to_owned(), server.path().to_string_lossy().into_owned()),
            (ENV_PANE.to_owned(), pane.to_string()),
        ];
        if let Ok(binary) = std::env::current_exe() {
            env.push((ENV_BIN.to_owned(), binary.to_string_lossy().into_owned()));
        }
        env
    }

    /// A hook reported something.
    pub(super) fn hook_message(&mut self, message: HookMessage) {
        let Some(kind) = AgentKind::from_slug(&message.agent) else { return };
        // The pane id comes from the environment; if that was stripped,
        // fall back to the terminal the hook is attached to.
        let pane_id = if self.panes.contains_key(&message.pane) {
            Some(message.pane)
        } else {
            message.tty.and_then(|tty| {
                self.panes.iter().find(|(_, pane)| pane.terminal.tty_device() == Some(tty)).map(|(id, _)| *id)
            })
        };
        let Some(pane_id) = pane_id else { return };
        let Some(pane) = self.panes.get_mut(&pane_id) else { return };

        let now = Instant::now();
        if pane.agent.as_ref().is_none_or(|session| session.kind != kind) {
            pane.agent = Some(Session::new(kind, now));
        }
        let Some(session) = pane.agent.as_mut() else { return };
        let change = hooks::apply(session, &message.event, &message.payload, now);
        log::debug!(
            "hook {} {:?} -> {:?} (state {:?}, activity {:?}, subagents {:?})",
            message.agent,
            message.payload["hook_event_name"].as_str().unwrap_or(&message.event),
            change,
            session.state,
            session.activity,
            session
                .subagents
                .iter()
                .map(|s| (s.kind.as_str(), s.description.as_str(), s.activity.as_deref(), s.finished.is_some()))
                .collect::<Vec<_>>(),
        );
        if change == Change::Ended {
            if pane.terminal.shell().integrated {
                // The program may still be on screen (e.g. after /clear);
                // the session goes away when the command itself ends.
                session.activity = None;
                session.title = None;
                let change = session.set_idle(now);
                return self.agent_changed(pane_id, change);
            }
            self.archive_agent(pane_id);
            return self.redraw();
        }
        self.agent_changed(pane_id, change);
    }

    /// The shell reported a command starting or ending: an agent session
    /// begins when its command starts and ends when it exits.
    pub(super) fn sync_agent(&mut self, pane_id: PaneId) {
        let Some(pane) = self.panes.get(&pane_id) else { return };
        let shell = pane.terminal.shell();
        if !shell.integrated {
            return;
        }
        let running = shell.command.as_deref().and_then(AgentKind::from_command);
        let current = pane.agent.as_ref().map(|session| (session.kind, session.hooked));
        let start = match (shell.command.is_some(), current, running) {
            // Back at the prompt: whatever was running has ended.
            (false, Some(_), _) => None,
            (false, None, _) => return,
            // A different agent than the one we were following.
            (true, Some((kind, _)), Some(new)) if kind != new => Some(new),
            (true, None, Some(new)) => Some(new),
            // An agent started under another name (an alias, a wrapper
            // script) still reports through its hooks; keep following it.
            (true, Some((_, true)), None) => return,
            (true, Some((_, false)), None) => None,
            _ => return,
        };
        self.archive_agent(pane_id);
        if let (Some(kind), Some(pane)) = (start, self.panes.get_mut(&pane_id)) {
            pane.agent = Some(Session::new(kind, Instant::now()));
        }
    }

    // ---- Archive ------------------------------------------------------------

    /// Ends the agent session in a pane, if any, and files it in the
    /// archive. Sessions that never did anything (say, `claude --version`)
    /// are not worth keeping.
    pub(super) fn archive_agent(&mut self, pane_id: PaneId) {
        let Some(pane) = self.panes.get_mut(&pane_id) else { return };
        let Some(session) = pane.agent.take() else { return };
        if !session.hooked && session.title.is_none() {
            return;
        }
        let pane = &self.panes[&pane_id];
        let project = self
            .tabs
            .iter()
            .find(|tab| tab.root.panes().contains(&pane_id))
            .and_then(|tab| self.workspace(tab.workspace))
            .map_or_else(String::new, |workspace| workspace.name.clone());
        let entry = ArchivedSession {
            agent: session.kind.slug().to_owned(),
            title: self.agent_title(pane, &session),
            cwd: session.cwd.clone().or_else(|| pane.terminal.working_directory()),
            project,
            session_id: session.session_id.clone(),
            ended_at: archive::now(),
            duration: session.started.elapsed().as_secs(),
        };
        self.archive.add(entry);
        self.redraw();
    }

    /// Opens a new tab with the start page, where the user says what to
    /// run: an agent on some task, or a plain terminal.
    pub(super) fn new_agent_session(&mut self) {
        self.open_start_page();
    }

    /// Takes a session out of the archive and continues it in a new tab,
    /// in the folder it ran in.
    ///
    /// Where the agent can resume a conversation by id, that command is
    /// run. Otherwise the agent's start command is typed but not run,
    /// since starting a fresh conversation may not be what was meant.
    pub(super) fn resume_archived(&mut self, index: usize) {
        let Some(session) = self.archive.take(index) else { return };
        let directory = session.cwd.clone().filter(|cwd| cwd.is_dir());
        let before = self.panes.len();
        self.new_tab(directory);
        if self.panes.len() == before {
            // The tab could not be opened; keep the session archived.
            self.archive.add(session);
            return;
        }
        let Some(id) = self.focused_pane_id() else { return };
        let input = match (session.resume_command(), session.kind()) {
            (Some(command), _) => format!("{command}\r"),
            (None, Some(kind)) => archive::start_command(kind).to_owned(),
            (None, None) => return,
        };
        self.type_when_ready(id, input.into_bytes());
    }

    // ---- Board --------------------------------------------------------------

    pub(super) fn board_model(&self) -> board::Model {
        let now = Instant::now();
        let mut model = board::Model::default();
        for tab in &self.tabs {
            let project = self.workspace(tab.workspace).map_or("", |w| w.name.as_str());
            for pane_id in tab.root.panes() {
                let Some(pane) = self.panes.get(&pane_id) else { continue };
                let Some(session) = &pane.agent else { continue };
                model.columns[board::column_of(session.state)].push(board::Card {
                    target: Target { tab: tab.id, pane: pane_id },
                    state: session.state,
                    agent: session.kind.name().to_owned(),
                    title: self.agent_title(pane, session),
                    project: project.to_owned(),
                    status: Self::agent_status(session),
                    elapsed: sidebar::format_elapsed(now.duration_since(session.since).as_secs()),
                });
            }
        }
        if let Overlay::Board { selected } = self.overlay {
            model.selected = selected;
        }
        model
    }

    /// Moves the board's selection, keeping it on a real card.
    pub(super) fn board_move(&mut self, dx: isize, dy: isize) {
        let counts = self.board_model().columns.each_ref().map(Vec::len);
        if let Overlay::Board { selected } = &mut self.overlay {
            *selected = board::step(counts, *selected, dx, dy);
        }
        self.redraw();
    }

    /// The pane printed something (used when the agent has no hooks).
    pub(super) fn agent_output(&mut self, pane_id: PaneId) {
        let change = match self.panes.get_mut(&pane_id).and_then(|pane| pane.agent.as_mut()) {
            Some(session) => session.on_output(Instant::now()),
            None => return,
        };
        self.agent_changed(pane_id, change);
    }

    /// The pane rang the bell or asked for a notification.
    pub(super) fn agent_attention(&mut self, pane_id: PaneId, message: Option<String>) {
        let Some(session) = self.panes.get_mut(&pane_id).and_then(|pane| pane.agent.as_mut()) else { return };
        let change = session.on_attention(Instant::now());
        if let (Some(message), false) = (message, session.hooked) {
            session.activity = Some(ale_agents::session::clean(&message, 120));
        }
        self.agent_changed(pane_id, change);
    }

    /// Reacts to a change in an agent's session: redraw, and alert the user
    /// when it wants them and they are not already looking at it.
    pub(super) fn agent_changed(&mut self, pane_id: PaneId, change: Change) {
        match change {
            Change::None => return,
            Change::Updated | Change::Ended => {}
            Change::State(state) => {
                let looking = self.focused && self.focused_pane_id() == Some(pane_id) && !self.overlay.is_modal();
                if looking {
                    self.mark_seen();
                } else if state.wants_attention() {
                    // Only when the agent is stuck without the user. A
                    // finished turn shows in the sidebar and on the tab,
                    // quietly.
                    self.alert(pane_id, state);
                }
            }
        }
        self.redraw();
    }

    /// The user is looking at the focused pane: a finished turn there is no
    /// longer news.
    pub(super) fn mark_seen(&mut self) {
        if !self.focused {
            return;
        }
        let Some(id) = self.focused_pane_id() else { return };
        if let Some(session) = self.panes.get_mut(&id).and_then(|pane| pane.agent.as_mut())
            && session.mark_seen(Instant::now()) != Change::None
        {
            self.redraw();
        }
    }

    /// Advances time-based state (silence means done, finished subagents
    /// expire). Returns when it needs to run next.
    pub(super) fn tick_agents(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        let mut changes = Vec::new();
        for (id, pane) in &mut self.panes {
            let Some(session) = pane.agent.as_mut() else { continue };
            let (change, wake) = session.tick(now);
            if change != Change::None {
                changes.push((*id, change));
            }
            if let Some(wake) = wake {
                next = Some(next.map_or(wake, |n| n.min(wake)));
            }
        }
        for (id, change) in changes {
            self.agent_changed(id, change);
        }
        next
    }

    /// Keeps the elapsed times in the sidebar current. Returns when to
    /// redraw next: while something is under a minute old, every second on
    /// the CPU renderer and every ten on the GPU renderer; after that,
    /// once a minute.
    pub(super) fn refresh_elapsed(&mut self) -> Option<Instant> {
        if !self.sidebar.visible {
            self.elapsed_refresh = None;
            return None;
        }
        let now = Instant::now();
        let youngest = self
            .panes
            .values()
            .filter_map(|pane| pane.agent.as_ref())
            .map(|session| now.duration_since(session.since))
            .min()?;
        if self.elapsed_refresh.is_some_and(|at| now >= at) {
            self.elapsed_refresh = None;
            self.redraw();
        }
        // On the GPU renderer every redraw keeps the driver's working
        // memory in use, so the clock ticks coarsely there.
        let cpu = self.renderer.as_ref().is_some_and(|r| r.backend() == Backend::Cpu);
        let interval = match (youngest < Duration::from_secs(60), cpu) {
            (true, true) => 1,
            (true, false) => 10,
            (false, _) => 60,
        };
        Some(*self.elapsed_refresh.get_or_insert(now + Duration::from_secs(interval)))
    }

    // ---- Alerts -------------------------------------------------------------

    fn alert(&mut self, pane_id: PaneId, state: AgentState) {
        // Automated runs must stay silent.
        if self.options.snapshot.is_some() {
            return;
        }
        let now = Instant::now();
        if self.config.agents.sound && now.duration_since(self.last_sound) > Duration::from_millis(1500) {
            self.last_sound = now;
            run_detached("afplay", &["/System/Library/Sounds/Funk.aiff"]);
        }
        if !self.focused {
            if let Some(window) = &self.window {
                window.request_user_attention(Some(winit::window::UserAttentionType::Informational));
            }
            if self.config.agents.notifications {
                let (title, body) = self.alert_text(pane_id, state);
                // The texts are passed as arguments, never spliced into the
                // script, so nothing an agent says can run as AppleScript.
                run_detached(
                    "osascript",
                    &[
                        "-e",
                        "on run argv",
                        "-e",
                        "display notification (item 1 of argv) with title (item 2 of argv)",
                        "-e",
                        "end run",
                        "--",
                        &body,
                        &title,
                    ],
                );
            }
        }
    }

    fn alert_text(&self, pane_id: PaneId, state: AgentState) -> (String, String) {
        let session = self.panes.get(&pane_id).and_then(|pane| pane.agent.as_ref());
        let name = session.map_or("Agent", |s| s.kind.name());
        let title = session.and_then(|s| s.title.clone()).unwrap_or_else(|| name.to_owned());
        let detail = session.and_then(|s| s.activity.clone());
        let body = match (state, detail) {
            (_, Some(detail)) => format!("{name}: {detail}"),
            (state, None) => format!("{name}: {}", state.label().to_lowercase()),
        };
        (title, body)
    }

    // ---- Sidebar ------------------------------------------------------------

    pub(super) fn sidebar_width(&self) -> f32 {
        if !self.sidebar.visible {
            return 0.0;
        }
        let width = (self.config.agents.sidebar_width * self.scale()).round();
        // Never take more than 45% of the window.
        width.min((self.window_size().0 * 0.45).round())
    }

    pub(super) fn sidebar_rect(&self) -> Option<Rect> {
        self.sidebar.visible.then(|| Rect::new(0.0, 0.0, self.sidebar_width(), self.window_size().1))
    }

    /// A short label for a pane that is not an agent: its running command,
    /// or its folder.
    fn pane_label(&self, pane: &Pane) -> String {
        let shell = pane.terminal.shell();
        let folder = shell.cwd.as_deref().map(folder_name).unwrap_or_else(|| pane.fallback_title.clone());
        match shell.command.filter(|command| !command.is_empty()) {
            Some(command) => format!("{command} — {folder}"),
            None if shell.integrated => folder,
            None => pane.title.clone().unwrap_or(folder),
        }
    }

    /// What to call an agent's session: the task it reported, else the
    /// title it gave its terminal, else just its name.
    pub(super) fn agent_title(&self, pane: &Pane, session: &Session) -> String {
        let reported = session.title.clone();
        // Without shell integration there is no telling who set the title.
        let from_terminal = || match pane.terminal.shell().integrated {
            true => Self::program_title(pane),
            false => pane.title.clone(),
        };
        reported.or_else(from_terminal).unwrap_or_else(|| session.kind.name().to_owned())
    }

    pub(super) fn agent_status(session: &Session) -> String {
        let label = session.state.label();
        match (session.state, &session.activity) {
            (AgentState::Working | AgentState::NeedsYou | AgentState::WaitingOnSubagents, Some(activity)) => {
                activity.clone()
            }
            (AgentState::Error | AgentState::RateLimited, Some(activity)) => format!("{label}: {activity}"),
            _ => label.to_owned(),
        }
    }

    /// The most pressing agent state among a tab's panes, for its dot.
    pub(super) fn tab_state(&self, tab: &Tab) -> Option<AgentState> {
        let states: Vec<AgentState> = tab
            .root
            .panes()
            .iter()
            .filter_map(|id| self.panes.get(id)?.agent.as_ref().map(|session| session.state))
            .collect();
        STATUS_ORDER.into_iter().find(|state| states.contains(state))
    }

    pub(super) fn sidebar_model(&self) -> Model {
        let now = Instant::now();
        let filter = self.sidebar.filter;
        let query = self.sidebar.query.to_lowercase();
        let matches_query = |texts: &[&str]| query.is_empty() || texts.iter().any(|t| t.to_lowercase().contains(&query));
        let active_tab = self.tabs.get(self.active).map(|tab| tab.id);

        let mut counts = [0usize; 3];
        // Every row with the workspace it belongs to and, for agents, its
        // state; grouped afterwards.
        let mut rows: Vec<(WorkspaceId, Option<AgentState>, Vec<Row>)> = Vec::new();
        for tab in &self.tabs {
            let workspace_name = self.workspace(tab.workspace).map_or("", |w| w.name.as_str());
            for pane_id in tab.root.panes() {
                let Some(pane) = self.panes.get(&pane_id) else { continue };
                let target = Target { tab: tab.id, pane: pane_id };
                let current = Some(tab.id) == active_tab && tab.focused == pane_id;
                let Some(session) = &pane.agent else {
                    let text = match &tab.custom_title {
                        Some(title) if tab.root.panes().len() == 1 => title.clone(),
                        _ => self.pane_label(pane),
                    };
                    if filter == Filter::All && !self.sidebar.by_status && matches_query(&[&text, workspace_name]) {
                        rows.push((tab.workspace, None, vec![Row::Shell { target, text, current }]));
                    }
                    continue;
                };
                counts[0] += 1;
                counts[1] += Filter::Waiting.matches(session.state) as usize;
                counts[2] += Filter::Working.matches(session.state) as usize;

                let title = self.agent_title(pane, session);
                let status = Self::agent_status(session);
                if !filter.matches(session.state) || !matches_query(&[&title, &status, workspace_name]) {
                    continue;
                }
                let mut group = vec![Row::Agent {
                    target,
                    state: session.state,
                    title,
                    status,
                    // Grouped by status, the project is the useful extra.
                    agent: if self.sidebar.by_status { workspace_name.to_owned() } else { session.kind.name().to_owned() },
                    elapsed: sidebar::format_elapsed(now.duration_since(session.since).as_secs()),
                    current,
                    icon: crate::icons::key(session.kind),
                }];
                for (index, subagent) in session.subagents.iter().enumerate() {
                    // Running ones, and those that finished a moment ago.
                    let recent = subagent.finished.is_none_or(|at| now.duration_since(at) < Duration::from_secs(20));
                    if !recent {
                        continue;
                    }
                    let text = if subagent.description.is_empty() {
                        subagent.kind.clone()
                    } else {
                        format!("{}: {}", subagent.kind, subagent.description)
                    };
                    group.push(Row::Subagent { target, index, text, running: subagent.finished.is_none() });
                }
                rows.push((tab.workspace, Some(session.state), group));
            }
        }

        let mut groups = Vec::new();
        if self.sidebar.by_status {
            for state in STATUS_ORDER {
                let mut group = Group { title: state.label().to_owned(), rows: Vec::new() };
                for (_, row_state, row) in &mut rows {
                    if *row_state == Some(state) {
                        group.rows.append(row);
                    }
                }
                groups.push(group);
            }
        } else {
            for workspace in &self.workspaces {
                let mut group = Group { title: workspace.name.clone(), rows: Vec::new() };
                for (id, _, row) in &mut rows {
                    if *id == workspace.id {
                        group.rows.append(row);
                    }
                }
                groups.push(group);
            }
        }
        Model {
            groups,
            counts,
            filter,
            by_status: self.sidebar.by_status,
            query: self.sidebar.query.clone(),
            searching: self.sidebar.searching,
            notice: self.hooks_notice(),
            archived: self.archive.len(),
        }
    }

    fn sidebar_geometry(&self) -> Option<(Rect, (f32, f32), f32)> {
        let rect = self.sidebar_rect()?;
        let metrics = self.renderer.as_ref()?.metrics();
        Some((rect, (metrics.width, metrics.height), self.scale()))
    }

    pub(super) fn sidebar_hit(&self, x: f32, y: f32) -> Option<Hit> {
        let (rect, cell, scale) = self.sidebar_geometry()?;
        sidebar::hit(&self.sidebar_model(), rect, cell, scale, self.sidebar.scroll, x, y)
    }

    pub(super) fn sidebar_scroll(&mut self, pixels: f32) {
        let Some((rect, cell, scale)) = self.sidebar_geometry() else { return };
        let limit = sidebar::max_scroll(&self.sidebar_model(), rect, cell, scale);
        self.sidebar.scroll = (self.sidebar.scroll - pixels).clamp(0.0, limit);
        self.redraw();
    }

    pub(super) fn sidebar_click(&mut self, hit: Hit) {
        self.sidebar.searching = false;
        match hit {
            Hit::Row(target) => {
                self.go_to(target);
                // Show the agent's graph when it has subagents or tasks.
                self.reveal_graph(target.pane, None);
            }
            Hit::Subagent(target, index) => {
                self.go_to(target);
                self.reveal_graph(target.pane, Some(index));
            }
            Hit::Filter(filter) => {
                self.sidebar.filter = filter;
                self.sidebar.scroll = 0.0;
            }
            Hit::GroupToggle => {
                self.sidebar.by_status = !self.sidebar.by_status;
                self.sidebar.scroll = 0.0;
            }
            Hit::Search => self.sidebar.searching = true,
            Hit::Notice => self.offer_hooks(),
            Hit::NewSession => self.new_agent_session(),
            Hit::Archive => self.open_list(ListKind::Archive),
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
            Hit::Nothing => {}
        }
        self.redraw();
    }

    /// Keyboard input while the sidebar's search box is active. Returns
    /// true when the key was used.
    pub(super) fn sidebar_key(&mut self, event: &KeyEvent) -> bool {
        if !self.sidebar.searching {
            return false;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.sidebar.query.clear();
                self.sidebar.searching = false;
            }
            Key::Named(NamedKey::Enter) => self.sidebar.searching = false,
            Key::Named(NamedKey::Backspace) => {
                self.sidebar.query.pop();
            }
            _ => {
                if self.modifiers.super_key() || self.modifiers.control_key() {
                    return false;
                }
                if let Some(text) = event.text.as_deref() {
                    self.sidebar.query.extend(text.chars().filter(|c| !c.is_control()));
                }
            }
        }
        self.sidebar.scroll = 0.0;
        self.redraw();
        true
    }

    /// Shows a pane: switches to its workspace and tab and focuses it.
    pub(super) fn go_to(&mut self, target: Target) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == target.tab) else { return };
        if self.tabs[index].root.panes().contains(&target.pane) {
            self.tabs[index].focused = target.pane;
        }
        self.activate(index);
        self.mark_seen();
    }

    /// Jumps to the next agent that wants the user, most urgent first.
    pub(super) fn next_attention(&mut self) {
        let current = self.focused_pane_id();
        let mut candidates: Vec<(usize, Target)> = Vec::new();
        for tab in &self.tabs {
            for pane_id in tab.root.panes() {
                let Some(session) = self.panes.get(&pane_id).and_then(|pane| pane.agent.as_ref()) else { continue };
                if Some(pane_id) != current && (session.state == AgentState::Done || session.state.wants_attention()) {
                    let urgency = STATUS_ORDER.iter().position(|s| *s == session.state).unwrap_or(9);
                    candidates.push((urgency, Target { tab: tab.id, pane: pane_id }));
                }
            }
        }
        candidates.sort_by_key(|(urgency, _)| *urgency);
        if let Some((_, target)) = candidates.first() {
            self.go_to(*target);
        }
    }

    // ---- Hook setup ---------------------------------------------------------

    /// The hint shown in the sidebar while Claude Code is running without
    /// its status hooks. Nothing is installed without the user asking.
    fn hooks_notice(&self) -> Option<String> {
        if self.claude_hooks || self.hooks_declined {
            return None;
        }
        let claude_running = self
            .panes
            .values()
            .any(|pane| pane.agent.as_ref().is_some_and(|session| session.kind == AgentKind::Claude));
        claude_running.then(|| "Set up Claude Code status…".to_owned())
    }

    /// Explains the hook installation and asks for a decision.
    pub(super) fn offer_hooks(&mut self) {
        let lines = if self.claude_hooks {
            vec!["Claude Code's status hooks are already installed.".to_owned()]
        } else {
            vec![
                "ale-term can show when Claude Code is working, has finished,".to_owned(),
                "or needs you. For that, Claude Code has to report its status.".to_owned(),
                String::new(),
                "This adds hooks to ~/.claude/settings.json that notify ale-term".to_owned(),
                "on this Mac only. Your other settings are kept, a backup of the".to_owned(),
                "file is saved next to it, and in any other terminal the hooks".to_owned(),
                "do nothing. Sessions already running pick it up after a restart.".to_owned(),
            ]
        };
        let hint = if self.claude_hooks { "Press Enter or Esc to close" } else { "Enter: install  ·  Esc: not now" };
        self.overlay = Overlay::Confirm(Box::new(Confirm {
            title: "Show Claude Code's status?".into(),
            lines,
            hint: hint.into(),
            template: None,
            install_hooks: !self.claude_hooks,
        }));
        self.redraw();
    }

    pub(super) fn install_hooks(&mut self) {
        let Some(path) = install::claude_settings_path() else { return };
        match install::install_claude_at(&path) {
            Ok(()) => self.claude_hooks = true,
            Err(error) => self.show_message("Hooks were not installed", vec![error]),
        }
        self.redraw();
    }

    pub(super) fn remove_hooks(&mut self) {
        let Some(path) = install::claude_settings_path() else { return };
        match install::uninstall_claude_at(&path) {
            Ok(()) => {
                self.claude_hooks = false;
                // Removing them is a decision too: do not nag afterwards.
                self.decline_hooks();
                self.show_message("Hooks removed", vec!["Claude Code's settings no longer mention ale-term.".into()]);
            }
            Err(error) => self.show_message("Hooks were not removed", vec![error]),
        }
    }

    /// Remembers that the user said no, so the hint stops appearing.
    pub(super) fn decline_hooks(&mut self) {
        self.hooks_declined = true;
        if let Some(dir) = &self.state_dir {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(dir.join("hooks-declined"), "");
        }
    }
}

/// Runs a helper program without waiting for it. A small thread collects
/// its exit status so it does not linger as a zombie process.
fn run_detached(program: &str, args: &[&str]) {
    match std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            let _ = std::thread::Builder::new().name("reaper".into()).spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => log::warn!("cannot run {program}: {e}"),
    }
}
