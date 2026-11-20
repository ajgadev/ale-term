//! The application: one window holding tabs, each tab holding split panes,
//! each pane running a shell.
//!
//! `App` reacts to three kinds of events: window events from the operating
//! system (keys, mouse, resize), events from terminal sessions (new output,
//! title changes, exit) and config-file changes. Nothing is drawn unless one
//! of those events asks for it, so an idle window uses no CPU.

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
        TabBar::layout(self.visible_tabs().len(), self.window_size().0, self.scale(), fullscreen, label_width)
    }

    /// The area below the tab bar, where panes go.
    fn content_rect(&self) -> Rect {
        let (width, height) = self.window_size();
        let bar = TabBar::height(self.scale());
        Rect::new(0.0, bar, width, (height - bar).max(0.0))
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
            env: Vec::new(),
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

    // ---- Overlays: command palette and search -----------------------------

    /// Keyboard handling while the palette or search bar is open: keys edit
    /// the query instead of going to the shell.
    fn overlay_key(&mut self, event: &KeyEvent, action: Option<Action>, event_loop: &ActiveEventLoop) {
        let shift = self.modifiers.shift_key();
        let command = self.modifiers.super_key() || self.modifiers.control_key();
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => return self.close_overlay(),
            Key::Named(NamedKey::Enter) => return self.overlay_confirm(shift, event_loop),
            Key::Named(NamedKey::ArrowUp) => return self.overlay_move(-1),
            Key::Named(NamedKey::ArrowDown) => return self.overlay_move(1),
            Key::Named(NamedKey::Backspace) => {
                if let Some(query) = self.overlay.query_mut() {
                    if command {
                        query.clear();
                    } else {
                        query.pop();
                    }
                }
                return self.overlay_changed();
            }
            _ => {}
        }
        match action {
            // A few commands make sense while an overlay is open.
            Some(Action::Paste) => {
                let text = get_clipboard().unwrap_or_default();
                self.overlay_type(text.lines().next().unwrap_or(""));
            }
            Some(action @ (Action::FindNext | Action::FindPrevious | Action::Quit)) => {
                self.perform(action, event_loop);
            }
            Some(Action::CommandPalette | Action::Find | Action::SwitchWorkspace) => self.close_overlay(),
            Some(_) => {}
            None if !command => {
                if let Some(text) = event.text.as_deref() {
                    self.overlay_type(text);
                }
            }
            None => {}
        }
    }

    fn overlay_type(&mut self, text: &str) {
        if let Some(query) = self.overlay.query_mut() {
            query.extend(text.chars().filter(|c| !c.is_control()));
            self.overlay_changed();
        }
    }

    /// The query was edited.
    fn overlay_changed(&mut self) {
        match &mut self.overlay {
            Overlay::Palette { selected, .. } => *selected = 0,
            Overlay::Search { query, .. } => {
                self.last_search = query.clone();
                self.search_step(SearchDirection::Left, false);
            }
            Overlay::None | Overlay::Input { .. } | Overlay::Confirm(_) => {}
        }
        self.redraw();
    }

    fn overlay_move(&mut self, offset: isize) {
        match &self.overlay {
            Overlay::Palette { kind, query, selected } => {
                let count = self.list_items(*kind, query).len() as isize;
                let next = (*selected as isize + offset).rem_euclid(count.max(1)) as usize;
                if let Overlay::Palette { selected, .. } = &mut self.overlay {
                    *selected = next;
                }
                self.redraw();
            }
            // Up goes to older matches, down to newer ones.
            Overlay::Search { .. } => {
                let direction = if offset < 0 { SearchDirection::Left } else { SearchDirection::Right };
                self.search_step(direction, true);
            }
            Overlay::None | Overlay::Input { .. } | Overlay::Confirm(_) => {}
        }
    }

    fn overlay_confirm(&mut self, shift: bool, event_loop: &ActiveEventLoop) {
        match &self.overlay {
            Overlay::Palette { kind, query, selected } => {
                let chosen = self.list_items(*kind, query).get(*selected).map(|item| item.choice);
                self.close_overlay();
                self.run_choice(chosen, event_loop);
            }
            Overlay::Input { kind, query } => {
                let (kind, text) = (*kind, query.clone());
                self.close_overlay();
                self.submit_input(kind, &text);
            }
            Overlay::Confirm(_) => self.confirm_accepted(),
            Overlay::Search { .. } => {
                let direction = if shift { SearchDirection::Right } else { SearchDirection::Left };
                self.search_step(direction, true);
            }
            Overlay::None => {}
        }
    }

    fn run_choice(&mut self, choice: Option<Choice>, event_loop: &ActiveEventLoop) {
        match choice {
            Some(Choice::Run(action)) => self.perform(action, event_loop),
            Some(choice) => self.choose(choice),
            None => {}
        }
    }

    fn close_overlay(&mut self) {
        self.overlay = Overlay::None;
        self.redraw();
    }

    fn open_search(&mut self) {
        let Some(pane) = self.focused_pane_id() else { return };
        // Start from the selected text when there is some on one line.
        let selected = self.focused_pane().and_then(|p| p.terminal.lock().selection_to_string());
        let query = match selected {
            Some(text) if !text.trim().is_empty() && !text.trim().contains('\n') => text.trim().to_owned(),
            _ => self.last_search.clone(),
        };
        self.last_search = query.clone();
        self.overlay = Overlay::Search { pane, query, failed: false };
        self.search_step(SearchDirection::Left, false);
    }

    /// Finds a match for the current search text and selects it.
    ///
    /// `advance` moves on from the current match (Find Next/Previous);
    /// without it the search restarts from the current match, which is what
    /// typing more letters should do. Left means towards older output.
    fn search_step(&mut self, direction: SearchDirection, advance: bool) {
        let pane_id = match &self.overlay {
            Overlay::Search { pane, .. } => Some(*pane),
            _ => self.focused_pane_id(),
        };
        let query = self.last_search.clone();
        let Some(pane) = pane_id.and_then(|id| self.panes.get(&id)) else { return };

        let mut found = false;
        if !query.is_empty()
            && let Ok(mut regex) = RegexSearch::new(&overlay::regex_escape(&query))
        {
            let mut term = pane.terminal.lock();
            let current = term.selection.as_ref().and_then(|s| s.to_range(&*term));
            let bottom = Point::new(term.bottommost_line(), term.last_column());
            let top = Point::new(term.topmost_line(), Column(0));
            let origin = match (current, direction, advance) {
                (Some(range), SearchDirection::Left, true) => range.start.sub(&*term, Boundary::None, 1),
                (Some(range), SearchDirection::Right, true) => range.end.add(&*term, Boundary::None, 1),
                (Some(range), _, false) => range.end,
                (None, ..) => Point::new(
                    Line(term.screen_lines() as i32 - 1 - term.grid().display_offset() as i32),
                    term.last_column(),
                ),
            };
            let side = if direction == SearchDirection::Left { Side::Right } else { Side::Left };
            // Search from the origin; when nothing is left that way, wrap
            // around to the other end.
            let wrap_origin = if direction == SearchDirection::Left { bottom } else { top };
            let hit = term
                .search_next(&mut regex, origin, direction, side, None)
                .or_else(|| term.search_next(&mut regex, wrap_origin, direction, side, None));
            if let Some(hit) = hit {
                let mut selection = Selection::new(SelectionType::Simple, *hit.start(), Side::Left);
                selection.update(*hit.end(), Side::Right);
                term.selection = Some(selection);
                term.scroll_to_point(*hit.start());
                found = true;
            } else {
                term.selection = None;
            }
        }
        if let Overlay::Search { failed, query, .. } = &mut self.overlay {
            *failed = !found && !query.is_empty();
        }
        self.redraw();
    }

    /// Scrolls so the previous (or next) shell prompt is at the top of the
    /// pane. Needs shell integration, which tags prompt cells.
    fn jump_to_prompt(&mut self, previous: bool) {
        let Some(pane) = self.focused_pane() else { return };
        let mut term = pane.terminal.lock();
        let offset = term.grid().display_offset() as i32;
        let (first, last) = (term.topmost_line().0, term.bottommost_line().0);
        let tagged = |line: i32| {
            term.grid()[Line(line)][Column(0)]
                .hyperlink()
                .is_some_and(|link| link.uri().starts_with(ale_core::PROMPT_TAG))
        };
        // A prompt can span several lines; its start is the first of them.
        let is_start = |line: i32| tagged(line) && (line == first || !tagged(line - 1));
        let top = -offset;
        let target = if previous {
            (first..top).rev().find(|line| is_start(*line))
        } else {
            (top + 1..=last).find(|line| is_start(*line))
        };
        if let Some(line) = target {
            term.scroll_display(Scroll::Delta(-line - offset));
        } else if !previous {
            term.scroll_display(Scroll::Bottom);
        }
        drop(term);
        self.redraw();
    }

    /// Sends typed bytes to the focused pane. Typing snaps the view back to
    /// the bottom and drops the selection, as in other terminals.
    fn send_input(&mut self, bytes: Vec<u8>) {
        let Some(pane) = self.focused_pane() else { return };
        {
            let mut term = pane.terminal.lock();
            term.scroll_display(Scroll::Bottom);
            term.selection = None;
        }
        pane.terminal.write(bytes);
        self.redraw();
    }

    fn copy(&mut self) {
        let Some(pane) = self.focused_pane() else { return };
        let text = pane.terminal.lock().selection_to_string();
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            set_clipboard(text);
        }
    }

    fn paste(&mut self) {
        let Some(pane) = self.focused_pane() else { return };
        let Some(text) = get_clipboard() else { return };
        let mode = *pane.terminal.lock().mode();
        self.send_input(input::encode_paste(&text, mode));
    }

    /// Cmd+K: forget the scrollback and ask the shell to redraw its prompt.
    fn clear_history(&mut self) {
        let Some(pane) = self.focused_pane() else { return };
        pane.terminal.lock().grid_mut().clear_history();
        pane.terminal.write(&b"\x0c"[..]);
        self.redraw();
    }

    // ---- Mouse ------------------------------------------------------------

    /// Whether the program in `pane` asked to receive mouse events itself.
    /// Holding Shift always gives the mouse back to the terminal.
    fn mouse_reporting(&self, pane: PaneId) -> Option<TermMode> {
        if self.modifiers.shift_key() {
            return None;
        }
        let mode = *self.panes.get(&pane)?.terminal.lock().mode();
        mode.intersects(TermMode::MOUSE_MODE).then_some(mode)
    }

    fn report_mouse(&mut self, pane: PaneId, rect: Rect, action: MouseAction, code: u8, mode: TermMode) {
        let (column, line, _) = self.cell_at(pane, rect, self.mouse.x, self.mouse.y);
        if action == MouseAction::Motion {
            if self.mouse.last_cell == Some((pane, column, line)) {
                return;
            }
            self.mouse.last_cell = Some((pane, column, line));
        }
        let bytes = input::encode_mouse(action, code, column, line, self.modifiers, mode);
        if let (Some(bytes), Some(pane)) = (bytes, self.panes.get(&pane)) {
            pane.terminal.write(bytes);
        }
    }

    fn divider_at(&self, layout: &Layout, x: f32, y: f32) -> Option<Divider> {
        let grab = 4.0 * self.scale();
        layout.dividers.iter().copied().find(|d| d.rect.inflate(grab).contains(x, y))
    }

    fn mouse_button(&mut self, state: ElementState, mouse_button: MouseButton, event_loop: &ActiveEventLoop) {
        let (x, y) = (self.mouse.x, self.mouse.y);
        let code = match mouse_button {
            MouseButton::Left => button::LEFT,
            MouseButton::Middle => button::MIDDLE,
            MouseButton::Right => button::RIGHT,
            _ => return,
        };

        if state == ElementState::Released {
            if mouse_button == MouseButton::Left {
                if let Drag::Reported(pane) = self.mouse.drag
                    && let Some(rect) = self.active_layout().rect_of(pane)
                    && let Some(mode) = self.mouse_reporting(pane)
                {
                    self.report_mouse(pane, rect, MouseAction::Release, code, mode);
                }
                self.mouse.drag = Drag::None;
            } else if let Some(pane) = self.active_layout().pane_at(x, y)
                && let Some(rect) = self.active_layout().rect_of(pane)
                && let Some(mode) = self.mouse_reporting(pane)
            {
                self.report_mouse(pane, rect, MouseAction::Release, code, mode);
            }
            return;
        }

        // An open palette takes every click: on a row runs it, elsewhere
        // closes the palette.
        if self.overlay.is_modal() {
            let mut chosen = None;
            if let Overlay::Palette { kind, query, selected } = &self.overlay {
                let items = self.list_items(*kind, query);
                chosen = self
                    .palette_layout(items.len(), *selected)
                    .and_then(|layout| layout.row_at(x, y))
                    .and_then(|index| items.get(index))
                    .map(|item| item.choice);
            }
            self.close_overlay();
            if mouse_button == MouseButton::Left {
                self.run_choice(chosen, event_loop);
            }
            return;
        }

        // Cmd+click opens the link under the mouse.
        if mouse_button == MouseButton::Left
            && self.modifiers.super_key()
            && let Some(link) = &self.hover_link
        {
            open_target(&link.target);
            return;
        }

        // Tab bar.
        if let Some(hit) = self.tab_bar().hit(x, y) {
            match (hit, mouse_button) {
                (TabBarHit::Tab(index), MouseButton::Left) => self.select_tab(index),
                (TabBarHit::Tab(index), MouseButton::Middle) => {
                    if let Some(&tab) = self.visible_tabs().get(index)
                        && self.close_tab(tab)
                    {
                        event_loop.exit();
                    }
                }
                (TabBarHit::Workspace, MouseButton::Left) => self.open_list(ListKind::Workspaces),
                (TabBarHit::NewTab, MouseButton::Left) => self.new_tab(None),
                (TabBarHit::Empty, MouseButton::Left) => {
                    let clicks = self.register_click();
                    if let Some(window) = &self.window {
                        if clicks == 2 {
                            window.set_maximized(!window.is_maximized());
                        } else {
                            let _ = window.drag_window();
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        let layout = self.active_layout();
        if mouse_button == MouseButton::Left
            && let Some(divider) = self.divider_at(&layout, x, y)
        {
            self.mouse.drag = Drag::Divider(divider);
            return;
        }
        let Some(pane) = layout.pane_at(x, y) else { return };
        let Some(rect) = layout.rect_of(pane) else { return };
        self.focus_pane(pane);

        if let Some(mode) = self.mouse_reporting(pane) {
            self.mouse.last_cell = None;
            self.report_mouse(pane, rect, MouseAction::Press, code, mode);
            if mouse_button == MouseButton::Left {
                self.mouse.drag = Drag::Reported(pane);
            }
            return;
        }
        if mouse_button != MouseButton::Left {
            return;
        }

        // Start a selection: single click selects by character, double by
        // word, triple by line.
        let kind = match self.register_click() {
            1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        let (column, line, side) = self.cell_at(pane, rect, x, y);
        if let Some(pane_state) = self.panes.get(&pane) {
            let mut term = pane_state.terminal.lock();
            let point = term::viewport_to_point(term.grid().display_offset(), Point::new(line, Column(column)));
            term.selection = Some(Selection::new(kind, point, side));
        }
        self.mouse.drag = Drag::Select(pane);
        self.redraw();
    }

    /// Counts consecutive clicks; returns 1, 2 or 3.
    fn register_click(&mut self) -> u8 {
        let now = Instant::now();
        let quick = now.duration_since(self.mouse.last_click) < Duration::from_millis(400);
        self.mouse.click_count = if quick { self.mouse.click_count % 3 + 1 } else { 1 };
        self.mouse.last_click = now;
        self.mouse.click_count
    }

    fn mouse_moved(&mut self, position: PhysicalPosition<f64>) {
        self.mouse.x = position.x as f32;
        self.mouse.y = position.y as f32;
        let (x, y) = (self.mouse.x, self.mouse.y);
        let layout = self.active_layout();

        match self.mouse.drag {
            Drag::Divider(divider) => {
                let area = divider.parent;
                let ratio = match divider.axis {
                    Axis::Horizontal => (x - area.x) / area.w.max(1.0),
                    Axis::Vertical => (y - area.y) / area.h.max(1.0),
                };
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.root.set_ratio(divider.index, ratio);
                }
                self.relayout();
            }
            Drag::Select(pane) => {
                if let Some(rect) = layout.rect_of(pane) {
                    let (column, line, side) = self.cell_at(pane, rect, x, y);
                    if let Some(pane) = self.panes.get(&pane) {
                        let mut term = pane.terminal.lock();
                        let offset = term.grid().display_offset();
                        let point = term::viewport_to_point(offset, Point::new(line, Column(column)));
                        if let Some(selection) = term.selection.as_mut() {
                            selection.update(point, side);
                        }
                    }
                    self.redraw();
                }
            }
            Drag::Reported(pane) => {
                if let Some(rect) = layout.rect_of(pane)
                    && let Some(mode) = self.mouse_reporting(pane)
                    && mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
                {
                    self.report_mouse(pane, rect, MouseAction::Motion, button::LEFT, mode);
                }
            }
            Drag::None => {
                if let Some(pane) = layout.pane_at(x, y)
                    && let Some(rect) = layout.rect_of(pane)
                    && let Some(mode) = self.mouse_reporting(pane)
                    && mode.contains(TermMode::MOUSE_MOTION)
                {
                    self.report_mouse(pane, rect, MouseAction::Motion, button::NONE, mode);
                }
            }
        }

        self.update_hover_link();

        // Show what a click would do.
        let icon = if self.tab_bar().bar.contains(x, y) || self.overlay.is_modal() {
            CursorIcon::Default
        } else if self.hover_link.is_some() {
            CursorIcon::Pointer
        } else {
            match self.divider_at(&layout, x, y) {
                Some(Divider { axis: Axis::Horizontal, .. }) => CursorIcon::ColResize,
                Some(Divider { axis: Axis::Vertical, .. }) => CursorIcon::RowResize,
                None => CursorIcon::Text,
            }
        };
        if let Some(window) = &self.window {
            window.set_cursor(icon);
        }
    }

    fn palette_layout(&self, count: usize, selected: usize) -> Option<PaletteLayout> {
        let metrics = self.renderer.as_ref()?.metrics();
        let cell = (metrics.width, metrics.height);
        Some(PaletteLayout::new(self.window_size(), cell, self.scale(), count, selected))
    }

    /// Recomputes which link (if any) is under the mouse. Links are only
    /// active while Cmd is held, so normal clicks and selection are never
    /// affected.
    fn update_hover_link(&mut self) {
        let (x, y) = (self.mouse.x, self.mouse.y);
        let mut link = None;
        if self.modifiers.super_key() && !self.overlay.is_open() && matches!(self.mouse.drag, Drag::None) {
            let layout = self.active_layout();
            if let Some(pane) = layout.pane_at(x, y)
                && let Some(rect) = layout.rect_of(pane)
            {
                link = self.link_at(pane, rect, x, y);
            }
        }
        let changed = match (&link, &self.hover_link) {
            (None, None) => false,
            (Some(a), Some(b)) => a.pane != b.pane || a.start != b.start || a.end != b.end,
            _ => true,
        };
        self.hover_link = link;
        if changed {
            self.redraw();
        }
    }

    /// Finds a link at a window position: an explicit hyperlink set by the
    /// program, a web address, or a path to an existing file.
    fn link_at(&self, pane_id: PaneId, rect: Rect, x: f32, y: f32) -> Option<HoverLink> {
        let pane = self.panes.get(&pane_id)?;
        let (column, line, _) = self.cell_at(pane_id, rect, x, y);
        let term = pane.terminal.lock();
        let point = term::viewport_to_point(term.grid().display_offset(), Point::new(line, Column(column)));
        let grid = term.grid();
        let last_column = term.last_column();
        let wraps = |line: Line| grid[line][last_column].flags.contains(Flags::WRAPLINE);

        // Explicit hyperlink (OSC 8): covers the neighbouring cells that
        // carry the same address.
        if let Some(hyperlink) = grid[point.line][point.column].hyperlink() {
            let uri = hyperlink.uri();
            let allowed = ["https://", "http://", "file://", "mailto:"].iter().any(|s| uri.starts_with(s));
            if !allowed {
                return None;
            }
            let same = |column: Column| {
                grid[point.line][column].hyperlink().is_some_and(|other| other.uri() == uri)
            };
            let (mut start, mut end) = (point.column, point.column);
            while start.0 > 0 && same(start - 1) {
                start -= 1;
            }
            while end < last_column && same(end + 1) {
                end += 1;
            }
            return Some(HoverLink {
                pane: pane_id,
                start: Point::new(point.line, start),
                end: Point::new(point.line, end),
                target: Target::Url(uri.to_owned()),
            });
        }

        // Otherwise look at the text. A long address may be wrapped over
        // several rows, so gather the whole logical line first.
        let mut first = point.line;
        while first > term.topmost_line() && point.line - first < 20 && wraps(first - 1) {
            first -= 1;
        }
        let mut last = point.line;
        while last < term.bottommost_line() && last - point.line < 20 && wraps(last) {
            last += 1;
        }
        let mut chars = Vec::new();
        let mut points = Vec::new();
        let mut at = None;
        let mut current = first;
        while current <= last {
            for column in 0..term.columns() {
                let cell = &grid[current][Column(column)];
                let cell_point = Point::new(current, Column(column));
                if cell_point == point {
                    at = Some(chars.len());
                }
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                chars.push(if cell.c == '\0' { ' ' } else { cell.c });
                points.push(cell_point);
            }
            current += 1;
        }
        let at = at?.min(chars.len().checked_sub(1)?);
        drop(term);

        let found = links::url_at(&chars, at).or_else(|| {
            let cwd = pane.terminal.working_directory();
            links::path_at(&chars, at, cwd.as_deref(), |path| path.exists())
        })?;
        Some(HoverLink {
            pane: pane_id,
            start: points[found.start],
            end: points[found.end - 1],
            target: found.target,
        })
    }

    fn mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let (x, y) = (self.mouse.x, self.mouse.y);
        let layout = self.active_layout();
        let Some(id) = layout.pane_at(x, y) else { return };
        let Some(rect) = layout.rect_of(id) else { return };
        let Some(cell_height) = self.renderer.as_ref().map(|r| r.metrics().height) else { return };
        let multiplier = self.config.terminal.scroll_multiplier;
        let Some(pane) = self.panes.get_mut(&id) else { return };

        // Turn the wheel or trackpad movement into whole lines, keeping the
        // remainder so slow trackpad scrolling still adds up.
        let pixels = match delta {
            MouseScrollDelta::LineDelta(_, lines) => lines * cell_height * multiplier,
            MouseScrollDelta::PixelDelta(position) => position.y as f32,
        };
        pane.scroll_remainder += pixels;
        let lines = (pane.scroll_remainder / cell_height).trunc();
        pane.scroll_remainder -= lines * cell_height;
        let lines = lines as i32;
        if lines == 0 {
            return;
        }

        let mode = *pane.terminal.lock().mode();
        if let Some(mode) = self.mouse_reporting(id) {
            // The program handles scrolling itself.
            let code = if lines > 0 { button::WHEEL_UP } else { button::WHEEL_DOWN };
            for _ in 0..lines.unsigned_abs().min(20) {
                self.mouse.last_cell = None;
                self.report_mouse(id, rect, MouseAction::Press, code, mode);
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            && !self.modifiers.shift_key()
        {
            // Full-screen programs without mouse support (less, man) scroll
            // with the arrow keys.
            let key = if lines > 0 { b'A' } else { b'B' };
            let prefix = if mode.contains(TermMode::APP_CURSOR) { b'O' } else { b'[' };
            let mut bytes = Vec::new();
            for _ in 0..lines.unsigned_abs().min(100) {
                bytes.extend_from_slice(&[0x1b, prefix, key]);
            }
            if let Some(pane) = self.panes.get(&id) {
                pane.terminal.write(bytes);
            }
        } else if let Some(pane) = self.panes.get(&id) {
            pane.terminal.lock().scroll_display(Scroll::Delta(lines));
            self.redraw();
        }
    }

    // ---- Terminal events --------------------------------------------------

    fn term_event(&mut self, id: PaneId, event: TermEvent, event_loop: &ActiveEventLoop) {
        match event {
            TermEvent::Wakeup => self.redraw(),
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
                self.mark_dirty();
                self.redraw();
            }
            TermEvent::PromptReady => self.flush_pending_input(id),
            TermEvent::Bell => {}
        }
    }

    // ---- Drawing ----------------------------------------------------------

    /// Queues text to be typed into a pane when its shell is ready.
    fn type_when_ready(&mut self, id: PaneId, bytes: Vec<u8>) {
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.pending_input = Some(bytes);
        }
    }

    fn flush_pending_input(&mut self, id: PaneId) {
        if let Some(pane) = self.panes.get_mut(&id)
            && let Some(bytes) = pane.pending_input.take()
        {
            pane.terminal.write(bytes);
        }
    }

    /// Shells without integration never announce their prompt; give them a
    /// few seconds to start and then type anyway.
    fn flush_overdue_input(&mut self) {
        let overdue: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, pane)| pane.pending_input.is_some() && pane.created.elapsed() > Duration::from_secs(3))
            .map(|(id, _)| *id)
            .collect();
        for id in overdue {
            self.flush_pending_input(id);
        }
    }

    /// Refreshes the folder-name titles of panes without shell integration.
    /// At most once a second per pane, since it asks the OS each time.
    /// (Integrated shells report their folder themselves.)
    fn refresh_fallback_titles(&mut self) {
        let mut stale = Vec::new();
        for tab in &self.tabs {
            let Some(pane) = self.panes.get_mut(&tab.focused) else { continue };
            if pane.fallback_checked.elapsed() < Duration::from_secs(1) {
                continue;
            }
            pane.fallback_checked = Instant::now();
            if let Some(directory) = pane.terminal.working_directory() {
                pane.fallback_title = folder_name(&directory);
            }
            if !pane.terminal.shell().integrated {
                stale.push(tab.id);
            }
        }
        // Without shell integration nothing tells us when the folder
        // changes, so regroup these tabs on the same slow schedule.
        for id in stale {
            if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                self.sync_workspace(index);
            }
        }
    }

    /// The label for a tab, from its focused pane.
    ///
    /// With shell integration we know what is going on: at a prompt the tab
    /// shows the folder; while a command runs it shows the title that
    /// program set, or else the command itself. Without integration we can
    /// only show the last title set, or the folder.
    fn tab_title(&self, tab: &Tab) -> String {
        if let Some(title) = &tab.custom_title {
            return title.clone();
        }
        let Some(pane) = self.panes.get(&tab.focused) else { return String::new() };
        let shell = pane.terminal.shell();
        if !shell.integrated {
            return pane.title.clone().unwrap_or_else(|| pane.fallback_title.clone());
        }
        let folder = shell.cwd.as_deref().map(folder_name).unwrap_or_else(|| pane.fallback_title.clone());
        let Some(command) = shell.command else { return folder };
        let set_by_program =
            shell.command_started.is_some_and(|started| pane.title_set > started);
        match (&pane.title, set_by_program) {
            (Some(title), true) => title.clone(),
            _ if command.is_empty() => folder,
            _ => format!("{command} — {folder}"),
        }
    }

    /// Describes the whole window to the renderer.
    fn build_frame(&mut self) {
        self.refresh_fallback_titles();
        self.flush_overdue_input();
        let layout = self.active_layout();
        let tab_bar = self.tab_bar();
        let visible = self.visible_tabs();
        let titles: Vec<String> = visible.iter().map(|i| self.tab_title(&self.tabs[*i])).collect();
        let active_position = visible.iter().position(|i| *i == self.active).unwrap_or(0);
        let workspace_name = self.workspace_name().to_owned();
        // Rows of an open list overlay, computed before the renderer is
        // borrowed.
        let list_rows: Vec<(String, String)> = match &self.overlay {
            Overlay::Palette { kind, query, .. } => {
                self.list_items(*kind, query).into_iter().map(|item| (item.title, item.detail)).collect()
            }
            _ => Vec::new(),
        };
        let focused_pane = self.focused_pane_id();
        let scale = self.scale();
        let padding = self.padding();
        let window = self.window_size();
        let Some(renderer) = self.renderer.as_mut() else { return };
        let palette = &self.palette;
        let metrics = renderer.metrics();

        // Move newly decoded images to the GPU (for every pane, so images
        // printed in background tabs do not pile up in memory), then keep
        // total image memory within the configured budget.
        for (id, pane) in &self.panes {
            let (added, removed) = pane.terminal.take_images();
            for image in added {
                renderer.upload_image(image_key(*id, image.id), image.width, image.height, &image.rgba);
            }
            for image_id in removed {
                renderer.remove_image(image_key(*id, image_id));
            }
        }
        renderer.limit_image_memory(self.config.terminal.image_memory_mb as usize * 1024 * 1024);

        renderer.begin(color(palette.background));
        for (id, rect) in &layout.panes {
            let Some(pane) = self.panes.get(id) else { continue };
            let is_focused = Some(*id) == focused_pane;
            let link = self.hover_link.as_ref().filter(|l| l.pane == *id).map(|l| (l.start, l.end));
            let style =
                PaneStyle {
                palette,
                metrics,
                padding,
                focused: is_focused && self.focused,
                link,
                ligatures: self.config.font.ligatures,
                image_key_base: image_key(*id, 0),
            };
            draw::pane(renderer, &pane.terminal.lock(), *rect, &style);
            if !is_focused {
                // Fade panes that are not receiving input.
                let [r, g, b, _] = color(palette.background);
                renderer.rect(rect.x, rect.y, rect.w, rect.h, [r, g, b, 70]);
            }
        }
        for divider in &layout.dividers {
            let Rect { x, y, w, h } = divider.rect;
            renderer.rect(x, y, w, h, color(palette.border));
        }
        tab_bar.draw(renderer, &workspace_name, &titles, active_position, scale, palette);

        match &self.overlay {
            Overlay::Palette { kind, query, selected } => {
                let placeholder = kind.placeholder();
                overlay::draw_palette(renderer, placeholder, query, *selected, &list_rows, window, scale, palette);
            }
            Overlay::Input { kind, query } => {
                overlay::draw_palette(renderer, kind.placeholder(), query, 0, &[], window, scale, palette);
            }
            Overlay::Confirm(confirm) => overlay::draw_confirm(renderer, confirm, window, scale, palette),
            Overlay::Search { pane, query, failed } => {
                if let Some(rect) = layout.rect_of(*pane) {
                    overlay::draw_search(renderer, query, *failed, rect, scale, palette);
                }
            }
            Overlay::None => {}
        }
    }

    fn draw(&mut self) {
        self.build_frame();
        let again = self.renderer.as_mut().is_some_and(|r| r.present());
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            log::info!("first frame {:?} after launch", self.launched.elapsed());
        }
        if again {
            self.redraw();
        }
    }

    /// When scripted test steps run: 1.5 seconds after launch (once the
    /// first shell is up), but always before the snapshot.
    fn script_time(&self, snapshot_at: Instant) -> Instant {
        (self.launched + Duration::from_millis(1500)).min(snapshot_at - Duration::from_millis(600))
    }

    /// Saves a picture of the window (for automated visual checks).
    fn take_snapshot(&mut self) {
        let Some(path) = self.options.snapshot.clone() else { return };
        // Twice: the first pass may fill the glyph atlas and ask for a redo.
        self.build_frame();
        self.build_frame();
        if let Some(renderer) = self.renderer.as_mut() {
            let (width, height, rgba) = renderer.capture();
            if let Err(e) = write_bmp(&path, width, height, &rgba) {
                eprintln!("ale-term: cannot write {}: {e}", path.display());
            }
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
        // Write the session a moment after the last change.
        if let Some(at) = self.save_at {
            if Instant::now() >= at {
                self.save_session();
            } else if self.snapshot_at.is_none() {
                event_loop.set_control_flow(ControlFlow::WaitUntil(at));
            }
        } else if self.snapshot_at.is_none() {
            event_loop.set_control_flow(ControlFlow::Wait);
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
                let next = if self.options.script.is_empty() { at } else { self.script_time(at) };
                event_loop.set_control_flow(ControlFlow::WaitUntil(next));
            }
        }
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
