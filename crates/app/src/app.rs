//! The application: one window holding tabs, each tab holding split panes,
//! each pane running a shell.
//!
//! `App` reacts to three kinds of events: window events from the operating
//! system (keys, mouse, resize), events from terminal sessions (new output,
//! title changes, exit) and config-file changes. Nothing is drawn unless one
//! of those events asks for it, so an idle window uses no CPU.

mod agents;
mod frame;
mod mouse;
mod overlays;
mod workspaces;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ale_config::{Config, Palette};
use ale_core::vt::grid::Scroll;
use ale_core::vt::grid::Dimensions;
use ale_core::vt::index::{Boundary, Column, Direction as SearchDirection, Line, Point, Side};
use ale_core::vt::term::cell::Flags;
use ale_core::vt::term::search::RegexSearch;
use ale_core::vt::selection::{Selection, SelectionType};
use ale_core::vt::term::{self, TermMode};
use ale_core::vt::vte::ansi::{Color as AnsiColor, NamedColor, Rgb as AnsiRgb};
use ale_core::{GridSize, SpawnOptions, TermEvent, Terminal};
use ale_render::{FontSpec, Renderer};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Theme, Window, WindowId};

use crate::Options;
use crate::actions::{self, Action};
use crate::links::{self, Target};
use crate::overlay::{self, InputKind, ListKind, Overlay, PaletteLayout};
use crate::workspace::{self, Approvals, TabId, Workspace, WorkspaceId};
use workspaces::Choice;
use crate::chrome::{TabBar, TabBarHit};
use crate::draw::{self, PaneStyle, color};
use crate::input::{self, MouseAction, button};
use crate::layout::{Axis, Direction, Divider, Layout, Node, PaneId, Rect};

/// Events sent to the main thread from background threads.
pub enum UserEvent {
    Term(PaneId, TermEvent),
    /// A status report from an agent's hook.
    Agent(ale_agents::HookMessage),
    ConfigChanged,
}

struct Pane {
    terminal: Terminal,
    /// Title set by the program (via escape sequence), if any.
    title: Option<String>,
    /// When that title was set, to tell a title set by the running program
    /// from a stale one left by the shell.
    title_set: Instant,
    /// Shown when the program set no title: the working folder's name.
    fallback_title: String,
    fallback_checked: Instant,
    /// Leftover trackpad scroll, in pixels, not yet amounting to a line.
    scroll_remainder: f32,
    /// Text to type once the shell shows its first prompt (a template's
    /// command, or a restored command line). Typing it any earlier would
    /// have the terminal echo it before the prompt appears.
    pending_input: Option<Vec<u8>>,
    created: Instant,
    /// The coding agent running here, if any.
    agent: Option<ale_agents::Session>,
}

struct Tab {
    id: TabId,
    /// The workspace this tab is shown in.
    workspace: WorkspaceId,
    /// Title given by the user or a template; otherwise automatic.
    custom_title: Option<String>,
    /// Stays in its workspace even when its folder changes project.
    pinned: bool,
    root: Node,
    /// The pane that receives keyboard input.
    focused: PaneId,
}

/// What a held-down left mouse button is currently doing.
enum Drag {
    None,
    /// Selecting text in a pane.
    Select(PaneId),
    /// Resizing a split.
    Divider(Divider),
    /// The click went to a program that tracks the mouse itself.
    Reported(PaneId),
}

struct Mouse {
    x: f32,
    y: f32,
    drag: Drag,
    /// For telling double and triple clicks apart.
    last_click: Instant,
    click_count: u8,
    /// Last cell reported to a mouse-tracking program, to avoid repeats.
    last_cell: Option<(PaneId, usize, usize)>,
}

/// A link under the mouse while Cmd is held.
struct HoverLink {
    pane: PaneId,
    /// First and last cell of the link, inclusive.
    start: Point,
    end: Point,
    target: Target,
}

pub struct App {
    options: Options,
    proxy: EventLoopProxy<UserEvent>,
    config: Config,
    palette: Palette,
    /// Font size multiplier from Cmd +/-.
    zoom: f32,

    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    focused: bool,

    panes: HashMap<PaneId, Pane>,
    /// Every tab of every workspace; the tab bar shows those of
    /// `active_ws`.
    tabs: Vec<Tab>,
    /// Index of the tab on screen. It always belongs to `active_ws`.
    active: usize,
    workspaces: Vec<Workspace>,
    active_ws: WorkspaceId,
    /// Source of ids for panes, tabs and workspaces.
    next_pane_id: PaneId,
    /// Per-project template files the user has allowed to run commands.
    approvals: Approvals,
    /// Where the session and approvals are stored.
    state_dir: Option<PathBuf>,
    /// When to write the session to disk next, if it changed.
    save_at: Option<Instant>,
    /// Listens for status reports from agent hooks.
    agent_server: Option<ale_agents::Server>,
    sidebar: agents::SidebarState,
    /// Claude Code's settings already contain our status hooks.
    claude_hooks: bool,
    /// The user said no to installing them.
    hooks_declined: bool,
    last_sound: Instant,
    /// When the sidebar's elapsed times are next refreshed.
    elapsed_refresh: Option<Instant>,

    modifiers: ModifiersState,
    mouse: Mouse,
    overlay: Overlay,
    /// The last search text, so Find Next works after the bar is closed.
    last_search: String,
    hover_link: Option<HoverLink>,
    /// Folder with the shell integration script, when enabled and installed.
    integration: Option<PathBuf>,
    snapshot_at: Option<Instant>,
    /// When the process started, to report time to first frame.
    launched: Instant,
    first_frame_logged: bool,
}

impl App {
    pub fn new(options: Options, proxy: EventLoopProxy<UserEvent>) -> App {
        let (config, error) = ale_config::load();
        if let Some(error) = error {
            log::warn!("config: {error}");
        }
        let palette = config.palette(true);
        let state_dir = workspace::state_dir();
        let sidebar_visible = config.agents.sidebar;
        App {
            options,
            proxy,
            config,
            palette,
            zoom: 1.0,
            window: None,
            renderer: None,
            focused: true,
            panes: HashMap::new(),
            tabs: Vec::new(),
            active: 0,
            workspaces: Vec::new(),
            active_ws: 0,
            next_pane_id: 1,
            approvals: state_dir.as_deref().map(Approvals::load).unwrap_or_default(),
            state_dir,
            save_at: None,
            agent_server: None,
            sidebar: agents::SidebarState { visible: sidebar_visible, ..Default::default() },
            claude_hooks: false,
            hooks_declined: false,
            last_sound: Instant::now(),
            elapsed_refresh: None,
            modifiers: ModifiersState::empty(),
            mouse: Mouse {
                x: 0.0,
                y: 0.0,
                drag: Drag::None,
                last_click: Instant::now(),
                click_count: 0,
                last_cell: None,
            },
            overlay: Overlay::None,
            last_search: String::new(),
            hover_link: None,
            integration: None,
            snapshot_at: None,
            launched: Instant::now(),
            first_frame_logged: false,
        }
    }

    // ---- Geometry -------------------------------------------------------

    fn scale(&self) -> f32 {
        self.window.as_ref().map_or(1.0, |w| w.scale_factor() as f32)
    }

    fn font_spec(&self) -> FontSpec {
        FontSpec {
            family: self.config.font.family.clone(),
            size_px: self.config.font.size * self.zoom * self.scale(),
            line_height: self.config.font.line_height,
        }
    }

    fn padding(&self) -> f32 {
        (self.config.window.padding * self.scale()).round()
    }

    fn window_size(&self) -> (f32, f32) {
        self.renderer.as_ref().map_or((1.0, 1.0), |r| {
            let (w, h) = r.size();
            (w as f32, h as f32)
        })
    }

    fn tab_bar(&self) -> TabBar {
        let fullscreen = self.window.as_ref().is_some_and(|w| w.fullscreen().is_some());
        let label = TabBar::workspace_label(self.workspace_name());
        let label_width = self.renderer.as_ref().map_or(0.0, |r| r.text_width(&label));
        // With the sidebar open the window buttons sit over it, not the bar.
        let lights = !fullscreen && !self.sidebar.visible;
        let (count, width) = (self.visible_tabs().len(), self.window_size().0);
        TabBar::layout(count, width, self.scale(), self.sidebar_width(), lights, label_width)
    }

    /// The area below the tab bar, where panes go.
    fn content_rect(&self) -> Rect {
        let (width, height) = self.window_size();
        let bar = TabBar::height(self.scale());
        let left = self.sidebar_width();
        Rect::new(left, bar, (width - left).max(0.0), (height - bar).max(0.0))
    }

    fn tab_layout(&self, tab: &Tab) -> Layout {
        let gap = self.scale().round().max(1.0);
        tab.root.layout(self.content_rect(), gap)
    }

    fn active_layout(&self) -> Layout {
        self.tabs.get(self.active).map(|tab| self.tab_layout(tab)).unwrap_or_default()
    }

    /// How many cells fit in a pane of the given pixel size.
    fn grid_size(&self, rect: Rect) -> GridSize {
        let metrics = match &self.renderer {
            Some(renderer) => renderer.metrics(),
            None => return GridSize::default(),
        };
        let padding = self.padding();
        let columns = ((rect.w - padding * 2.0) / metrics.width).floor().clamp(2.0, 1000.0);
        let lines = ((rect.h - padding * 2.0) / metrics.height).floor().clamp(1.0, 1000.0);
        GridSize {
            columns: columns as u16,
            lines: lines as u16,
            cell_width: metrics.width as u16,
            cell_height: metrics.height as u16,
        }
    }

    /// Resizes every terminal to fit its pane. Called whenever the window,
    /// the font or the split layout changes.
    fn relayout(&mut self) {
        for tab in &self.tabs {
            for (id, rect) in self.tab_layout(tab).panes {
                if let Some(pane) = self.panes.get(&id) {
                    pane.terminal.resize(self.grid_size(rect));
                }
            }
        }
        self.mark_dirty();
        self.redraw();
    }

    fn redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// The cell under a window position, clamped to the pane's grid, and
    /// which half of the cell the position is in.
    fn cell_at(&self, pane: PaneId, rect: Rect, x: f32, y: f32) -> (usize, usize, Side) {
        let metrics = self.renderer.as_ref().map(|r| r.metrics());
        let (Some(metrics), Some(pane)) = (metrics, self.panes.get(&pane)) else {
            return (0, 0, Side::Left);
        };
        let size = pane.terminal.size();
        let padding = self.padding();
        let fx = ((x - rect.x - padding) / metrics.width).max(0.0);
        let fy = ((y - rect.y - padding) / metrics.height).max(0.0);
        let column = (fx as usize).min(size.columns.saturating_sub(1) as usize);
        let line = (fy as usize).min(size.lines.saturating_sub(1) as usize);
        let side = if fx.fract() < 0.5 { Side::Left } else { Side::Right };
        (column, line, side)
    }

    // ---- Panes and tabs ---------------------------------------------------

    fn focused_pane_id(&self) -> Option<PaneId> {
        self.tabs.get(self.active).map(|tab| tab.focused)
    }

    fn focused_pane(&self) -> Option<&Pane> {
        self.panes.get(&self.focused_pane_id()?)
    }

    /// Starts a shell in a new pane. `rect` is where the pane will be, so the
    /// shell starts at the right size.
    fn spawn_pane(&mut self, working_directory: Option<PathBuf>, rect: Rect) -> Option<PaneId> {
        let id = self.next_id();

        let proxy = self.proxy.clone();
        let callback = Arc::new(move |id, event| {
            let _ = proxy.send_event(UserEvent::Term(id, event));
        });
        let terminal = &self.config.terminal;
        let options = SpawnOptions {
            shell: terminal.shell.clone().map(|shell| (shell, terminal.shell_args.clone())),
            working_directory: working_directory
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from)),
            scrollback: terminal.scrollback,
            size: self.grid_size(rect),
            integration: self.integration.clone().filter(|_| terminal.shell_integration),
            env: self.agent_env(id),
        };
        match Terminal::spawn(id, options, callback) {
            Ok(terminal) => {
                let pane = Pane {
                    terminal,
                    title: None,
                    title_set: Instant::now(),
                    fallback_title: "shell".into(),
                    // Already "expired", so the first draw looks up the folder.
                    fallback_checked: Instant::now() - Duration::from_secs(60),
                    scroll_remainder: 0.0,
                    pending_input: None,
                    created: Instant::now(),
                    agent: None,
                };
                self.panes.insert(id, pane);
                Some(id)
            }
            Err(e) => {
                log::error!("cannot start a shell: {e}");
                None
            }
        }
    }

    /// The folder a new pane should start in: the focused pane's.
    fn inherited_directory(&self) -> Option<PathBuf> {
        self.focused_pane().and_then(|pane| pane.terminal.working_directory())
    }

    fn new_tab(&mut self, working_directory: Option<PathBuf>) {
        let directory = working_directory.or_else(|| self.inherited_directory());
        if let Some(id) = self.spawn_pane(directory.clone(), self.content_rect()) {
            // The very first tab has no workspace to join yet.
            let workspace = if self.tabs.is_empty() {
                self.workspace_for(directory.as_deref().and_then(workspace::project_root))
            } else {
                self.active_ws
            };
            // Next to the current tab, like browsers do.
            let index = self.add_tab(self.active + 1, id, workspace);
            self.activate(index);
            self.sync_workspace(index);
        }
    }

    fn split(&mut self, axis: Axis) {
        let Some(target) = self.focused_pane_id() else { return };
        let Some(rect) = self.active_layout().rect_of(target) else { return };
        // The new pane gets about half of the pane it splits.
        let half = match axis {
            Axis::Horizontal => Rect::new(rect.x, rect.y, rect.w / 2.0, rect.h),
            Axis::Vertical => Rect::new(rect.x, rect.y, rect.w, rect.h / 2.0),
        };
        let Some(id) = self.spawn_pane(self.inherited_directory(), half) else { return };
        let tab = &mut self.tabs[self.active];
        tab.root.split(target, axis, id);
        tab.focused = id;
        self.relayout();
    }

    /// Closes a pane, and its tab when it was the tab's last pane. Returns
    /// true when no tabs are left and the app should quit.
    fn close_pane(&mut self, id: PaneId) -> bool {
        self.panes.remove(&id);
        if let Some(renderer) = &mut self.renderer {
            renderer.retain_images(|key| key >> 32 != id);
        }
        let Some(index) = self.tabs.iter().position(|tab| tab.root.panes().contains(&id)) else {
            return self.tabs.is_empty();
        };
        let tab = &mut self.tabs[index];
        let root = std::mem::replace(&mut tab.root, Node::Leaf(id));
        match root.remove(id) {
            Some(root) => {
                if tab.focused == id {
                    tab.focused = root.panes()[0];
                }
                tab.root = root;
            }
            None => {
                let removed = self.tabs.remove(index);
                self.after_tab_removed(index, removed.workspace, index == self.active);
            }
        }
        self.relayout();
        self.tabs.is_empty()
    }

    fn close_tab(&mut self, index: usize) -> bool {
        let Some(tab) = self.tabs.get(index) else { return false };
        let mut done = false;
        for id in tab.root.panes() {
            done = self.close_pane(id);
        }
        done
    }

    /// Shows the tab at position `index` of the tab bar.
    fn select_tab(&mut self, index: usize) {
        if let Some(&tab) = self.visible_tabs().get(index)
            && tab != self.active
        {
            self.activate(tab);
        }
    }

    /// Moves `offset` tabs right (or left when negative), wrapping around.
    fn cycle_tab(&mut self, offset: isize) {
        let visible = self.visible_tabs();
        if let Some(current) = visible.iter().position(|tab| *tab == self.active) {
            let next = (current as isize + offset).rem_euclid(visible.len() as isize) as usize;
            self.select_tab(next);
        }
    }

    fn focus_pane(&mut self, id: PaneId) {
        if let Some(tab) = self.tabs.get_mut(self.active)
            && tab.focused != id
        {
            tab.focused = id;
            // The tab is now "in" the newly focused pane's folder.
            self.sync_workspace(self.active);
            self.mark_seen();
            self.redraw();
        }
    }

    fn cycle_pane(&mut self, offset: isize) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let panes = tab.root.panes();
        let Some(current) = panes.iter().position(|id| *id == tab.focused) else { return };
        let next = (current as isize + offset).rem_euclid(panes.len() as isize) as usize;
        self.focus_pane(panes[next]);
    }

    fn focus_direction(&mut self, direction: Direction) {
        let Some(from) = self.focused_pane_id() else { return };
        if let Some(to) = self.active_layout().neighbor(from, direction) {
            self.focus_pane(to);
        }
    }

    // ---- Settings ---------------------------------------------------------

    fn system_is_dark(&self) -> bool {
        self.window.as_ref().and_then(|w| w.theme()) != Some(Theme::Light)
    }

    fn apply_theme(&mut self) {
        self.palette = self.config.palette(self.system_is_dark());
        self.redraw();
    }

    fn apply_font(&mut self) {
        let spec = self.font_spec();
        if let Some(renderer) = &mut self.renderer {
            renderer.set_font(&spec);
        }
        self.relayout();
    }

    fn apply_option_key(&self) {
        #[cfg(target_os = "macos")]
        if let Some(window) = &self.window {
            use winit::platform::macos::{OptionAsAlt, WindowExtMacOS};
            let mode = if self.config.terminal.option_as_meta {
                OptionAsAlt::Both
            } else {
                OptionAsAlt::None
            };
            window.set_option_as_alt(mode);
        }
    }

    fn set_zoom(&mut self, zoom: f32) {
        let zoom = zoom.clamp(0.4, 4.0);
        if zoom != self.zoom {
            self.zoom = zoom;
            self.apply_font();
        }
    }

    /// Re-reads the config file. A file with errors is ignored so a typo
    /// mid-edit never breaks the running terminal.
    fn reload_config(&mut self) {
        match ale_config::reload() {
            Ok(config) if config != self.config => {
                self.config = config;
                self.apply_theme();
                self.apply_font();
                self.apply_option_key();
            }
            Ok(_) => {}
            Err(e) => log::warn!("config not reloaded: {e}"),
        }
    }

    // ---- Keyboard ---------------------------------------------------------

    /// Runs a command, whether it came from a shortcut or the palette.
    fn perform(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        match action {
            Action::NewTab => self.new_tab(None),
            Action::ClosePane => {
                if let Some(id) = self.focused_pane_id()
                    && self.close_pane(id)
                {
                    event_loop.exit();
                }
            }
            Action::CloseTab => {
                if self.close_tab(self.active) {
                    event_loop.exit();
                }
            }
            Action::RenameTab => {
                let query = self.tabs.get(self.active).and_then(|t| t.custom_title.clone()).unwrap_or_default();
                self.overlay = Overlay::Input { kind: InputKind::RenameTab, query };
                self.redraw();
            }
            Action::SwitchWorkspace => self.open_list(ListKind::Workspaces),
            Action::NextWorkspace => self.cycle_workspace(1),
            Action::PreviousWorkspace => self.cycle_workspace(-1),
            Action::NewWorkspace => {
                self.overlay = Overlay::Input { kind: InputKind::NewWorkspace, query: String::new() };
                self.redraw();
            }
            Action::RenameWorkspace => {
                let query = self.workspace_name().to_owned();
                self.overlay = Overlay::Input { kind: InputKind::RenameWorkspace, query };
                self.redraw();
            }
            Action::MoveTab => self.open_list(ListKind::MoveTab),
            Action::SetUpWorkspace => self.set_up_workspace(),
            Action::ToggleSidebar => {
                self.sidebar.visible = !self.sidebar.visible;
                self.sidebar.searching = false;
                self.relayout();
            }
            Action::NextAttention => self.next_attention(),
            Action::SetUpHooks => self.offer_hooks(),
            Action::RemoveHooks => self.remove_hooks(),
            Action::SplitRight => self.split(Axis::Horizontal),
            Action::SplitDown => self.split(Axis::Vertical),
            Action::NextTab => self.cycle_tab(1),
            Action::PreviousTab => self.cycle_tab(-1),
            Action::SelectTab(index) => {
                // Cmd+9 always goes to the last tab.
                let last = self.visible_tabs().len().saturating_sub(1);
                self.select_tab(if index == 8 { last } else { (index as usize).min(last) });
            }
            Action::NextPane => self.cycle_pane(1),
            Action::PreviousPane => self.cycle_pane(-1),
            Action::FocusPane(direction) => self.focus_direction(direction),
            Action::Copy => self.copy(),
            Action::Paste => self.paste(),
            Action::Find => self.open_search(),
            Action::FindNext => self.search_step(SearchDirection::Right, true),
            Action::FindPrevious => self.search_step(SearchDirection::Left, true),
            Action::PreviousPrompt => self.jump_to_prompt(true),
            Action::NextPrompt => self.jump_to_prompt(false),
            Action::ScrollPageUp => self.scroll(Scroll::PageUp),
            Action::ScrollPageDown => self.scroll(Scroll::PageDown),
            Action::ScrollToTop => self.scroll(Scroll::Top),
            Action::ScrollToBottom => self.scroll(Scroll::Bottom),
            Action::ClearHistory => self.clear_history(),
            Action::ZoomIn => self.set_zoom(self.zoom * 1.1),
            Action::ZoomOut => self.set_zoom(self.zoom / 1.1),
            Action::ZoomReset => self.set_zoom(1.0),
            Action::ToggleFullscreen => {
                if let Some(window) = &self.window {
                    let fullscreen = window.fullscreen().is_some();
                    window.set_fullscreen(
                        (!fullscreen).then_some(winit::window::Fullscreen::Borderless(None)),
                    );
                }
            }
            Action::CommandPalette => self.open_list(ListKind::Commands),
            Action::OpenConfig => open_config_file(),
            Action::Quit => event_loop.exit(),
        }
    }

    fn open_list(&mut self, kind: ListKind) {
        self.overlay = Overlay::Palette { kind, query: String::new(), selected: 0 };
        self.redraw();
    }

    fn scroll(&mut self, scroll: Scroll) {
        if let Some(pane) = self.focused_pane() {
            pane.terminal.lock().scroll_display(scroll);
            self.redraw();
        }
    }

    fn key(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) {
        if event.state != ElementState::Pressed {
            return;
        }
        let action =
            actions::from_key(&event.key_without_modifiers(), event.physical_key, self.modifiers);
        if self.overlay.is_open() {
            return self.overlay_key(event, action, event_loop);
        }
        if self.sidebar_key(event) {
            return;
        }
        if let Some(action) = action {
            return self.perform(action, event_loop);
        }
        let Some(pane) = self.focused_pane() else { return };
        let mode = *pane.terminal.lock().mode();
        let base = match event.key_without_modifiers() {
            Key::Character(text) => text.chars().next(),
            _ => None,
        };
        let bytes = input::encode_key(
            &event.logical_key,
            event.text.as_deref(),
            base,
            self.modifiers,
            mode,
            self.config.terminal.option_as_meta,
        );
        if let Some(bytes) = bytes {
            self.send_input(bytes);
        }
    }

    // ---- Terminal events --------------------------------------------------

    fn term_event(&mut self, id: PaneId, event: TermEvent, event_loop: &ActiveEventLoop) {
        match event {
            TermEvent::Wakeup => {
                self.agent_output(id);
                self.redraw();
            }
            TermEvent::Title(title) => {
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.title = Some(title).filter(|t| !t.trim().is_empty());
                    pane.title_set = Instant::now();
                    self.redraw();
                }
            }
            TermEvent::ResetTitle => {
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.title = None;
                    self.redraw();
                }
            }
            TermEvent::Exit => {
                if self.panes.contains_key(&id) && self.close_pane(id) {
                    event_loop.exit();
                }
            }
            TermEvent::ClipboardStore(text) => set_clipboard(text),
            TermEvent::ClipboardLoad(format) => {
                if let (Some(pane), Some(text)) = (self.panes.get(&id), get_clipboard()) {
                    pane.terminal.write(format(&text).into_bytes());
                }
            }
            TermEvent::ColorRequest(index, format) => {
                if let Some(pane) = self.panes.get(&id) {
                    let rgb = {
                        let term = pane.terminal.lock();
                        let named = match index {
                            256 => AnsiColor::Named(NamedColor::Foreground),
                            257 => AnsiColor::Named(NamedColor::Background),
                            258 => AnsiColor::Named(NamedColor::Cursor),
                            index => AnsiColor::Indexed(index.min(255) as u8),
                        };
                        draw::resolve(named, term.colors(), &self.palette)
                    };
                    let reply = format(AnsiRgb { r: rgb.r, g: rgb.g, b: rgb.b });
                    pane.terminal.write(reply.into_bytes());
                }
            }
            TermEvent::ShellChanged => {
                // The folder may have changed project: regroup the tab.
                if let Some(index) = self.tabs.iter().position(|tab| tab.focused == id) {
                    self.sync_workspace(index);
                }
                // A command started or ended: an agent may have too.
                self.sync_agent(id);
                self.mark_dirty();
                self.redraw();
            }
            TermEvent::PromptReady => self.flush_pending_input(id),
            TermEvent::Bell => self.agent_attention(id, None),
            TermEvent::Notify(message) => self.agent_attention(id, Some(message)),
        }
    }

}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let size = LogicalSize::new(self.config.window.width, self.config.window.height);
        let mut attributes = Window::default_attributes()
            .with_title("ale-term")
            .with_inner_size(size)
            .with_min_inner_size(LogicalSize::new(320.0, 200.0));
        #[cfg(target_os = "macos")]
        {
            // Let our tab bar occupy the title bar area, next to the traffic
            // lights, instead of sitting under a separate title strip.
            use winit::platform::macos::WindowAttributesExtMacOS;
            attributes = attributes
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true);
        }
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                eprintln!("ale-term: cannot open a window: {e}");
                return event_loop.exit();
            }
        };
        window.set_ime_allowed(true);
        self.window = Some(window.clone());
        self.palette = self.config.palette(self.system_is_dark());
        self.apply_option_key();

        let size = window.inner_size();
        let started = Instant::now();
        let renderer = pollster::block_on(Renderer::new(
            window.clone(),
            size.width,
            size.height,
            &self.font_spec(),
        ));
        log::info!("renderer ready in {:?}", started.elapsed());
        match renderer {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(e) => {
                eprintln!("ale-term: cannot set up the GPU: {e}");
                return event_loop.exit();
            }
        }

        if self.config.terminal.shell_integration
            && let Some(dir) = ale_config::config_path().as_deref().and_then(|p| p.parent())
        {
            match ale_core::integration::install(dir) {
                Ok(dir) => self.integration = Some(dir),
                Err(e) => log::warn!("shell integration not installed: {e}"),
            }
        }

        self.start_agent_server();

        // Pick up where the last run left off, unless asked for a specific
        // folder.
        let restored = self.config.window.restore
            && self.options.working_directory.is_none()
            && self.restore_session();
        if !restored {
            self.new_tab(self.options.working_directory.clone());
        }
        if self.tabs.is_empty() {
            return event_loop.exit();
        }
        if let (Some(text), Some(pane)) = (&self.options.type_text, self.focused_pane()) {
            pane.terminal.write(text.replace("\\n", "\r").into_bytes());
        }
        if self.options.demo {
            self.split(Axis::Horizontal);
            self.split(Axis::Vertical);
            self.new_tab(None);
            self.select_tab(0);
        }
        if self.options.snapshot.is_some() {
            self.snapshot_at =
                Some(Instant::now() + Duration::from_millis(self.options.snapshot_delay_ms));
        }

        let proxy = self.proxy.clone();
        ale_config::watch(move || {
            let _ = proxy.send_event(UserEvent::ConfigChanged);
        });
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Term(id, event) => self.term_event(id, event, event_loop),
            UserEvent::ConfigChanged => self.reload_config(),
            UserEvent::Agent(message) => self.hook_message(message),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
                self.relayout();
            }
            WindowEvent::ScaleFactorChanged { .. } => self.apply_font(),
            WindowEvent::ThemeChanged(_) => self.apply_theme(),
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                self.mark_seen();
                // Tell programs that asked to know (e.g. editors that
                // auto-save or dim on focus loss).
                if let Some(pane) = self.focused_pane()
                    && pane.terminal.lock().mode().contains(TermMode::FOCUS_IN_OUT)
                {
                    pane.terminal.write(if focused { &b"\x1b[I"[..] } else { &b"\x1b[O"[..] });
                }
                self.redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                // Pressing or releasing Cmd turns link highlighting on or off.
                self.update_hover_link();
            }
            WindowEvent::KeyboardInput { event, .. } => self.key(&event, event_loop),
            // Text composed by the system: accents typed with dead keys,
            // emoji picker, input methods.
            WindowEvent::Ime(Ime::Commit(text)) => {
                if self.overlay.is_open() {
                    self.overlay_type(&text);
                } else {
                    self.send_input(text.into_bytes());
                }
            }
            WindowEvent::CursorMoved { position, .. } => self.mouse_moved(position),
            WindowEvent::MouseInput { state, button, .. } => {
                self.mouse_button(state, button, event_loop)
            }
            WindowEvent::MouseWheel { delta, .. } => self.mouse_wheel(delta),
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.save_session();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Everything below may ask to be woken at some time; we sleep until
        // the earliest, or until the next event if nobody asks.
        let mut wake: Option<Instant> = None;
        let mut wake_at = |at: Instant| wake = Some(wake.map_or(at, |w: Instant| w.min(at)));

        // Write the session a moment after the last change.
        if let Some(at) = self.save_at {
            if Instant::now() >= at {
                self.save_session();
            } else {
                wake_at(at);
            }
        }
        if let Some(at) = self.tick_agents() {
            wake_at(at);
        }
        if let Some(at) = self.refresh_elapsed() {
            wake_at(at);
        }

        // Test hook: run scripted steps shortly before the snapshot.
        if let Some(at) = self.snapshot_at
            && !self.options.script.is_empty()
            && Instant::now() >= self.script_time(at)
        {
            for step in std::mem::take(&mut self.options.script) {
                if let Some(text) = step.strip_prefix("type:") {
                    self.overlay_type(text);
                } else if step == "enter" {
                    self.overlay_confirm(false, event_loop);
                } else if let Some(command) = actions::COMMANDS.iter().find(|c| c.1 == step) {
                    self.perform(command.0, event_loop);
                } else {
                    eprintln!("ale-term: unknown script step {step:?}");
                }
            }
        }
        if let Some(at) = self.snapshot_at {
            if Instant::now() >= at {
                self.take_snapshot();
                event_loop.exit();
            } else {
                wake_at(if self.options.script.is_empty() { at } else { self.script_time(at) });
            }
        }
        event_loop.set_control_flow(match wake {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }
}

fn set_clipboard(text: String) {
    match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text)) {
        Ok(()) => {}
        Err(e) => log::warn!("cannot copy to the clipboard: {e}"),
    }
}

fn get_clipboard() -> Option<String> {
    arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()).ok()
}

/// Opens the config file in the default editor (Cmd+,).
fn open_config_file() {
    if let Some(path) = ale_config::config_path() {
        let _ = std::process::Command::new("open").arg("-t").arg(path).spawn();
    }
}

/// Writes RGBA pixels as an uncompressed 24-bit BMP. BMP needs no library
/// and is only used for test snapshots.
fn write_bmp(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    let row_size = (width * 3).div_ceil(4) * 4;
    let data_size = row_size * height;
    let mut out = Vec::with_capacity(54 + data_size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + data_size).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    // Rows are stored bottom-up, pixels as blue-green-red.
    for row in (0..height as usize).rev() {
        let start = row * width as usize * 4;
        for pixel in rgba[start..start + width as usize * 4].chunks(4) {
            out.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        out.resize(out.len() + (row_size - width * 3) as usize, 0);
    }
    std::fs::write(path, out)
}

/// Opens a link with the system's default app. The target is passed as a
/// single argument, never through a shell.
fn open_target(target: &Target) {
    let mut command = std::process::Command::new("open");
    match target {
        Target::Url(url) => command.arg(url),
        Target::File(path) => command.arg(path),
    };
    if let Err(e) = command.spawn() {
        log::warn!("cannot open link: {e}");
    }
}

/// The short name shown for a folder: its last component, `~` for home.
fn folder_name(directory: &std::path::Path) -> String {
    if std::env::var_os("HOME").is_some_and(|home| directory == std::path::Path::new(&home)) {
        return "~".into();
    }
    directory.file_name().map_or_else(|| "/".into(), |name| name.to_string_lossy().into_owned())
}

/// The renderer key for an image: pane id in the high half, the pane's own
/// image id in the low half.
fn image_key(pane: PaneId, image: u32) -> u64 {
    (pane << 32) | image as u64
}
