//! The usage screen in the app: opening it, reading the agents' records
//! in the background, choosing what it shows, and its keys and clicks.

use ale_agents::AgentState;
use ale_agents::usage::{JOURNAL, Query, Report, Store, Stretch};

use super::*;
use crate::usage::{self, Hit, MenuRow, RANGES, Sizes, View};

pub struct UsageState {
    /// The totals; loaded when the screen first opens.
    store: Option<Store>,
    /// The agents' records are being read on another thread.
    scanning: bool,
    /// Index into [`RANGES`].
    range: usize,
    /// The only project on show (empty for folders outside any project);
    /// `None` shows every workspace.
    project: Option<String>,
    /// The only agent on show, by its short name.
    agent: Option<String>,
    /// The open workspace menu: its highlighted row and the first row shown.
    menu: Option<(usize, usize)>,
    /// What the screen shows, added up when any of the above changes.
    report: Option<Report>,
    /// The workspace menu's rows, and the project each one stands for.
    rows: Vec<MenuRow>,
    projects: Vec<Option<String>>,
}

impl Default for UsageState {
    fn default() -> Self {
        UsageState {
            store: None,
            scanning: false,
            // Thirty days.
            range: 1,
            project: None,
            agent: None,
            menu: None,
            report: None,
            rows: Vec::new(),
            projects: Vec::new(),
        }
    }
}

/// What ale-term has written to its usage journal about the agent in a
/// pane. Kept only for agents whose own records it cannot read.
#[derive(Default)]
pub struct Activity {
    /// When the stretch of work in progress began, and whether a prompt
    /// from the user began it.
    working: Option<(Instant, bool)>,
    /// When the last stretch ended.
    ended: Option<Instant>,
    /// The session has been counted as a task.
    counted: bool,
    /// The last hour (since 1970) the session was counted as active in.
    hour: Option<u64>,
    /// The agent last stopped to ask the user something.
    answered: bool,
}

/// Work that picks up this soon after it stopped is the same turn: an
/// agent followed by watching its pane seems to pause now and then.
const SAME_TURN: Duration = Duration::from_secs(10);

const ALL_WORKSPACES: &str = "All workspaces";

impl UsageState {
    pub fn view(&self, pointer: (f32, f32)) -> View<'_> {
        let chosen = self.rows.iter().find(|row| row.checked);
        View {
            report: self.report.as_ref(),
            scanning: self.scanning,
            range: self.range,
            workspace: chosen.map_or(ALL_WORKSPACES, |row| row.label.as_str()),
            rows: &self.rows,
            menu: self.menu,
            agent: self.agent.as_deref(),
            pointer,
        }
    }
}

/// Local time's distance from UTC at a time, in seconds.
fn utc_offset(seconds: u64) -> i64 {
    let time = seconds as libc::time_t;
    // SAFETY: `localtime_r` writes only to the `tm` it is given, which is
    // a plain structure of numbers and a pointer it sets itself.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() { 0 } else { tm.tm_gmtoff as i64 }
    }
}

/// A folder as the user would write it, with `~` for the home folder.
fn short_path(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => format!("~{}", &path[home.len()..]),
        _ => path.to_owned(),
    }
}

impl App {
    /// Notes the work of an agent whose records cannot be read: when it
    /// stops working (or `ended`: its session is over), the stretch goes
    /// into the usage journal.
    pub(super) fn journal_activity(&mut self, pane_id: PaneId, ended: bool) {
        let Some(pane) = self.panes.get_mut(&pane_id) else { return };
        let Some(session) = pane.agent.as_ref().filter(|session| ale_agents::usage::journaled(session.kind)) else {
            return;
        };
        let now = Instant::now();
        let activity = &mut pane.activity;
        if session.state == AgentState::Working && !ended {
            if activity.working.is_none() {
                // Carrying on after a question, or after a pause too
                // short to be one, is not a new turn.
                let resumed = activity.ended.is_some_and(|ended| now.duration_since(ended) < SAME_TURN);
                activity.working = Some((now, !resumed && !activity.answered));
            }
            activity.answered = false;
            return;
        }
        // Stopping to ask something: what follows is the same turn.
        activity.answered = session.state == AgentState::NeedsYou;
        if let Some((since, turn)) = activity.working.take() {
            let time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_millis() as u64);
            let hour = time / 3_600_000;
            let stretch = Stretch {
                time,
                agent: session.kind.slug().to_owned(),
                cwd: session.cwd.clone().or_else(|| pane.terminal.working_directory()),
                millis: now.duration_since(since).as_millis() as u64,
                task: !std::mem::replace(&mut activity.counted, true),
                turn,
                active: activity.hour.replace(hour) != Some(hour),
            };
            activity.ended = Some(now);
            if let Some(dir) = &self.state_dir
                && let Err(e) = ale_agents::usage::append_stretch(&dir.join(JOURNAL), &stretch)
            {
                log::warn!("cannot write the usage journal: {e}");
            }
        }
        if ended {
            *activity = Activity::default();
        }
    }

    /// Shows the usage screen, or puts it away.
    pub(super) fn toggle_usage(&mut self) {
        if matches!(self.overlay, Overlay::Usage) {
            return self.close_overlay();
        }
        if self.usage.store.is_none() {
            self.usage.store = self.state_dir.as_deref().map(Store::load);
        }
        self.usage.menu = None;
        self.refresh_usage();
        self.overlay = Overlay::Usage;
        self.scan_usage();
        self.redraw();
    }

    /// Reads what is new in the agents' records, on another thread; the
    /// totals arrive as `UserEvent::UsageScanned`.
    fn scan_usage(&mut self) {
        if self.usage.scanning {
            return;
        }
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
        let state_dir = self.state_dir.clone();
        // Without a place to keep them, the totals live in this window.
        let kept = if state_dir.is_none() { self.usage.store.clone() } else { None };
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new().name("scan-usage".into()).spawn(move || {
            let started = Instant::now();
            let mut store = state_dir.as_deref().map(Store::load).or(kept).unwrap_or_default();
            let journal = state_dir.as_deref().map(|dir| dir.join(JOURNAL));
            let changed = store.scan(&home, journal.as_deref(), &workspace::project_root);
            if changed && let Err(e) = store.save() {
                log::warn!("cannot save the usage totals: {e}");
            }
            log::info!("agent usage read in {:?}", started.elapsed());
            let _ = proxy.send_event(UserEvent::UsageScanned(Box::new(store)));
        });
        match spawned {
            Ok(_) => self.usage.scanning = true,
            Err(e) => log::warn!("cannot read agent usage: {e}"),
        }
    }

    /// The records have been read.
    pub(super) fn usage_scanned(&mut self, store: Store) {
        self.usage.scanning = false;
        self.usage.store = Some(store);
        if matches!(self.overlay, Overlay::Usage) {
            self.refresh_usage();
            self.redraw();
        }
    }

    /// Adds the totals up again for what the screen is set to show.
    fn refresh_usage(&mut self) {
        let state = &mut self.usage;
        let Some(store) = state.store.as_ref().filter(|store| !store.is_empty() || !state.scanning) else {
            state.report = None;
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let everyone = Query { days: RANGES[state.range].1, project: state.project.as_deref(), agent: None, now };
        let mut report = store.report(&Query { agent: state.agent.as_deref(), ..everyone }, &utc_offset);
        // An agent picked earlier that these days do not have: show all.
        if state.agent.as_ref().is_some_and(|agent| !report.agents.iter().any(|used| used.agent == *agent)) {
            report = store.report(&everyone, &utc_offset);
            state.agent = None;
        }
        state.report = Some(report);

        // The workspace menu: everything, then each project used in these
        // days, the open ones first. The one on show is always listed.
        let used = state.report.iter().flat_map(|report| &report.projects).map(|project| project.project.clone());
        let mut projects: Vec<String> = used.collect();
        if let Some(project) = &state.project
            && !projects.contains(project)
        {
            projects.push(project.clone());
        }
        let open = |project: &String| {
            self.workspaces.iter().any(|workspace| match &workspace.root {
                Some(root) => root.to_string_lossy() == *project,
                None => project.is_empty(),
            })
        };
        projects.sort_by_key(|project| !open(project));
        state.rows = vec![MenuRow { label: ALL_WORKSPACES.into(), detail: String::new(), checked: state.project.is_none() }];
        state.projects = vec![None];
        for project in projects {
            let label = if project.is_empty() {
                workspace::GENERAL.to_owned()
            } else {
                workspace::root_name(Path::new(&project))
            };
            let detail = if project.is_empty() { "outside any project".into() } else { short_path(&project) };
            state.rows.push(MenuRow { label, detail, checked: state.project.as_ref() == Some(&project) });
            state.projects.push(Some(project));
        }
    }

    fn usage_layout(&self) -> Option<usage::Layout> {
        let metrics = self.renderer.as_ref()?.metrics();
        let scale = self.scale();
        let sizes = Sizes::of(&self.usage.view((0.0, 0.0)));
        Some(usage::Layout::new(self.window_size(), (metrics.width, metrics.height), scale, TabBar::height(scale), sizes))
    }

    /// What of the usage screen is at a window position.
    pub(super) fn usage_hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.usage_layout()?.hit(x, y)
    }

    /// How many rows the workspace menu shows at once.
    fn usage_menu_room(&self) -> usize {
        self.usage_layout().map_or(3, |layout| layout.menu_room(self.window_size()))
    }

    fn open_usage_menu(&mut self) {
        let selected = self.usage.rows.iter().position(|row| row.checked).unwrap_or(0);
        let first = usage::menu_scroll(selected, 0, self.usage_menu_room());
        self.usage.menu = Some((selected, first));
    }

    /// Shows one workspace's usage: the menu row at `index`.
    fn pick_usage_row(&mut self, index: usize) {
        if let Some(project) = self.usage.projects.get(index) {
            self.usage.project = project.clone();
        }
        self.usage.menu = None;
        self.refresh_usage();
    }

    pub(super) fn usage_click(&mut self, x: f32, y: f32) {
        let hit = self.usage_hit(x, y);
        // With the menu open, a click picks a row or just closes it.
        if self.usage.menu.is_some() {
            match hit {
                Some(Hit::MenuRow(index)) => self.pick_usage_row(index),
                _ => self.usage.menu = None,
            }
            return self.redraw();
        }
        match hit {
            Some(Hit::Close) => return self.close_overlay(),
            Some(Hit::Range(index)) => {
                self.usage.range = index;
                self.refresh_usage();
            }
            Some(Hit::Workspace) => self.open_usage_menu(),
            Some(Hit::Agent(index)) => {
                let agent = self.usage.report.as_ref().and_then(|report| report.agents.get(index));
                let agent = agent.map(|agent| agent.agent.clone());
                // A second click on the agent on show brings the rest back.
                self.usage.agent = if self.usage.agent == agent { None } else { agent };
                self.refresh_usage();
            }
            Some(Hit::MenuRow(_) | Hit::Bar(_) | Hit::Hour(..)) | None => return,
        }
        self.redraw();
    }

    /// Keys of the usage screen. Returns whether the key was one of them.
    pub(super) fn usage_key(&mut self, event: &KeyEvent) -> bool {
        let Key::Named(key) = &event.logical_key else { return false };
        match (key, self.usage.menu) {
            (NamedKey::Escape, Some(_)) => self.usage.menu = None,
            (NamedKey::ArrowUp | NamedKey::ArrowDown, Some((selected, first))) => {
                let count = self.usage.rows.len().max(1) as isize;
                let offset = if *key == NamedKey::ArrowUp { -1 } else { 1 };
                let selected = (selected as isize + offset).rem_euclid(count) as usize;
                self.usage.menu = Some((selected, usage::menu_scroll(selected, first, self.usage_menu_room())));
            }
            (NamedKey::Enter | NamedKey::Space, Some((selected, _))) => self.pick_usage_row(selected),
            (NamedKey::Tab, Some(_)) => self.usage.menu = None,
            (NamedKey::Tab | NamedKey::ArrowDown, None) => self.open_usage_menu(),
            (NamedKey::ArrowLeft | NamedKey::ArrowRight, None) => {
                let range = if *key == NamedKey::ArrowLeft {
                    self.usage.range.saturating_sub(1)
                } else {
                    (self.usage.range + 1).min(RANGES.len() - 1)
                };
                if range != self.usage.range {
                    self.usage.range = range;
                    self.refresh_usage();
                }
            }
            _ => return false,
        }
        self.redraw();
        true
    }
}
