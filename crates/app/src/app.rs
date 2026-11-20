//! The application: one window holding tabs, each tab holding split panes,
//! each pane running a shell.
//!
//! `App` reacts to three kinds of events: window events from the operating
//! system (keys, mouse, resize), events from terminal sessions (new output,
//! title changes, exit) and config-file changes. Nothing is drawn unless one
//! of those events asks for it, so an idle window uses no CPU.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ale_config::{Config, Palette};
use ale_core::vt::grid::Scroll;
use ale_core::vt::index::{Column, Point, Side};
use ale_core::vt::selection::{Selection, SelectionType};
use ale_core::vt::term::{self, TermMode};
use ale_core::vt::vte::ansi::{Color as AnsiColor, NamedColor, Rgb as AnsiRgb};
use ale_core::{GridSize, SpawnOptions, TermEvent, Terminal};
use ale_render::{FontSpec, Renderer};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Theme, Window, WindowId};

use crate::Options;
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
    /// Shown when the program set no title: the working folder's name.
    fallback_title: String,
    fallback_checked: Instant,
    /// Leftover trackpad scroll, in pixels, not yet amounting to a line.
    scroll_remainder: f32,
}

struct Tab {
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
    tabs: Vec<Tab>,
    active: usize,
    next_pane_id: PaneId,

    modifiers: ModifiersState,
    mouse: Mouse,
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
            next_pane_id: 1,
            modifiers: ModifiersState::empty(),
            mouse: Mouse {
                x: 0.0,
                y: 0.0,
                drag: Drag::None,
                last_click: Instant::now(),
                click_count: 0,
                last_cell: None,
            },
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
        TabBar::layout(self.tabs.len(), self.window_size().0, self.scale(), fullscreen)
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
        let id = self.next_pane_id;
        self.next_pane_id += 1;

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
        };
        match Terminal::spawn(id, options, callback) {
            Ok(terminal) => {
                let pane = Pane {
                    terminal,
                    title: None,
                    fallback_title: "shell".into(),
                    // Already "expired", so the first draw looks up the folder.
                    fallback_checked: Instant::now() - Duration::from_secs(60),
                    scroll_remainder: 0.0,
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
        if let Some(id) = self.spawn_pane(directory, self.content_rect()) {
            // Next to the current tab, like browsers do.
            let index = (self.active + 1).min(self.tabs.len());
            self.tabs.insert(index, Tab { root: Node::Leaf(id), focused: id });
            self.active = index;
            self.redraw();
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
                self.tabs.remove(index);
                if self.active > index || self.active >= self.tabs.len() {
                    self.active = self.active.saturating_sub(1);
                }
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

    fn select_tab(&mut self, index: usize) {
        if index < self.tabs.len() && index != self.active {
            self.active = index;
            self.redraw();
        }
    }

    /// Moves `offset` tabs right (or left when negative), wrapping around.
    fn cycle_tab(&mut self, offset: isize) {
        let count = self.tabs.len() as isize;
        if count > 0 {
            self.select_tab((self.active as isize + offset).rem_euclid(count) as usize);
        }
    }

    fn focus_pane(&mut self, id: PaneId) {
        if let Some(tab) = self.tabs.get_mut(self.active)
            && tab.focused != id
        {
            tab.focused = id;
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

    /// Handles app shortcuts. Returns true when the key was consumed.
    fn shortcut(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) -> bool {
        let mods = self.modifiers;
        let (cmd, shift, alt, ctrl) =
            (mods.super_key(), mods.shift_key(), mods.alt_key(), mods.control_key());

        // Ctrl+Tab and Ctrl+Shift+Tab switch tabs.
        if ctrl && !cmd && event.logical_key == Key::Named(NamedKey::Tab) {
            self.cycle_tab(if shift { -1 } else { 1 });
            return true;
        }
        // Shift+PageUp/PageDown scroll the history by a page.
        if shift && !cmd && !ctrl && !alt {
            let scroll = match event.logical_key {
                Key::Named(NamedKey::PageUp) => Some(Scroll::PageUp),
                Key::Named(NamedKey::PageDown) => Some(Scroll::PageDown),
                _ => None,
            };
            if let (Some(scroll), Some(pane)) = (scroll, self.focused_pane()) {
                pane.terminal.lock().scroll_display(scroll);
                self.redraw();
                return true;
            }
        }
        if !cmd {
            return false;
        }

        // Cmd+Option+arrows move focus between panes.
        if alt {
            let direction = match event.logical_key {
                Key::Named(NamedKey::ArrowLeft) => Some(Direction::Left),
                Key::Named(NamedKey::ArrowRight) => Some(Direction::Right),
                Key::Named(NamedKey::ArrowUp) => Some(Direction::Up),
                Key::Named(NamedKey::ArrowDown) => Some(Direction::Down),
                _ => None,
            };
            if let Some(direction) = direction {
                self.focus_direction(direction);
                return true;
            }
        }
        // Brackets are matched by position so they work on every layout.
        match event.physical_key {
            PhysicalKey::Code(KeyCode::BracketLeft) => {
                if shift { self.cycle_tab(-1) } else { self.cycle_pane(-1) }
                return true;
            }
            PhysicalKey::Code(KeyCode::BracketRight) => {
                if shift { self.cycle_tab(1) } else { self.cycle_pane(1) }
                return true;
            }
            _ => {}
        }

        let key = event.key_without_modifiers();
        let Key::Character(text) = &key else { return false };
        match text.as_str() {
            "t" => self.new_tab(None),
            "w" => {
                if let Some(id) = self.focused_pane_id()
                    && self.close_pane(id)
                {
                    event_loop.exit();
                }
            }
            "d" => self.split(if shift { Axis::Vertical } else { Axis::Horizontal }),
            "c" => self.copy(),
            "v" => self.paste(),
            "k" => self.clear_history(),
            "q" => event_loop.exit(),
            "=" | "+" => self.set_zoom(self.zoom * 1.1),
            "-" => self.set_zoom(self.zoom / 1.1),
            "0" => self.set_zoom(1.0),
            "," => open_config_file(),
            "f" if ctrl => {
                if let Some(window) = &self.window {
                    let fullscreen = window.fullscreen().is_some();
                    window.set_fullscreen(
                        (!fullscreen).then_some(winit::window::Fullscreen::Borderless(None)),
                    );
                }
            }
            digit @ ("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9") => {
                let index = digit.parse::<usize>().unwrap_or(1) - 1;
                // Cmd+9 always goes to the last tab.
                let last = self.tabs.len().saturating_sub(1);
                self.select_tab(if index == 8 { last } else { index.min(last) });
            }
            _ => return false,
        }
        true
    }

    fn key(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) {
        if event.state != ElementState::Pressed {
            return;
        }
        if self.shortcut(event, event_loop) {
            return;
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

        // Tab bar.
        if let Some(hit) = self.tab_bar().hit(x, y) {
            match (hit, mouse_button) {
                (TabBarHit::Tab(index), MouseButton::Left) => self.select_tab(index),
                (TabBarHit::Tab(index), MouseButton::Middle) => {
                    if self.close_tab(index) {
                        event_loop.exit();
                    }
                }
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

        // Show what a click would do.
        let icon = if self.tab_bar().bar.contains(x, y) {
            CursorIcon::Default
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
            TermEvent::Bell => {}
        }
    }

    // ---- Drawing ----------------------------------------------------------

    /// Refreshes the folder-name titles of panes whose program set no title.
    /// At most once a second per pane, since it asks the OS each time.
    fn refresh_fallback_titles(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let home = std::env::var_os("HOME").map(PathBuf::from);
        for id in tab.root.panes() {
            let Some(pane) = self.panes.get_mut(&id) else { continue };
            if pane.title.is_some() || pane.fallback_checked.elapsed() < Duration::from_secs(1) {
                continue;
            }
            pane.fallback_checked = Instant::now();
            if let Some(directory) = pane.terminal.working_directory() {
                pane.fallback_title = if Some(&directory) == home.as_ref() {
                    "~".into()
                } else {
                    directory
                        .file_name()
                        .map_or_else(|| "/".into(), |name| name.to_string_lossy().into_owned())
                };
            }
        }
    }

    fn tab_title(&self, tab: &Tab) -> String {
        match self.panes.get(&tab.focused) {
            Some(pane) => pane.title.clone().unwrap_or_else(|| pane.fallback_title.clone()),
            None => String::new(),
        }
    }

    /// Describes the whole window to the renderer.
    fn build_frame(&mut self) {
        self.refresh_fallback_titles();
        let layout = self.active_layout();
        let tab_bar = self.tab_bar();
        let titles: Vec<String> = self.tabs.iter().map(|tab| self.tab_title(tab)).collect();
        let focused_pane = self.focused_pane_id();
        let scale = self.scale();
        let padding = self.padding();
        let Some(renderer) = self.renderer.as_mut() else { return };
        let palette = &self.palette;
        let metrics = renderer.metrics();

        renderer.begin(color(palette.background));
        for (id, rect) in &layout.panes {
            let Some(pane) = self.panes.get(id) else { continue };
            let is_focused = Some(*id) == focused_pane;
            let style = PaneStyle { palette, metrics, padding, focused: is_focused && self.focused };
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
        tab_bar.draw(renderer, &titles, self.active, scale, palette);
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

        self.new_tab(self.options.working_directory.clone());
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
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => self.key(&event, event_loop),
            // Text composed by the system: accents typed with dead keys,
            // emoji picker, input methods.
            WindowEvent::Ime(Ime::Commit(text)) => self.send_input(text.into_bytes()),
            WindowEvent::CursorMoved { position, .. } => self.mouse_moved(position),
            WindowEvent::MouseInput { state, button, .. } => {
                self.mouse_button(state, button, event_loop)
            }
            WindowEvent::MouseWheel { delta, .. } => self.mouse_wheel(delta),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(at) = self.snapshot_at {
            if Instant::now() >= at {
                self.take_snapshot();
                event_loop.exit();
            } else {
                event_loop.set_control_flow(ControlFlow::WaitUntil(at));
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
