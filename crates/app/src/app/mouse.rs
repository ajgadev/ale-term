//! Mouse handling: clicks, drags, selection, wheel scrolling, mouse
//! reporting to programs, and links under the pointer.

use super::*;

impl App {
    /// Whether the program in `pane` asked to receive mouse events itself.
    /// Holding Shift always gives the mouse back to the terminal.
    pub(super) fn mouse_reporting(&self, pane: PaneId) -> Option<TermMode> {
        if self.modifiers.shift_key() {
            return None;
        }
        let mode = *self.panes.get(&pane)?.terminal.lock().mode();
        mode.intersects(TermMode::MOUSE_MODE).then_some(mode)
    }

    pub(super) fn report_mouse(&mut self, pane: PaneId, rect: Rect, action: MouseAction, code: u8, mode: TermMode) {
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

    pub(super) fn divider_at(&self, layout: &Layout, x: f32, y: f32) -> Option<Divider> {
        let grab = 4.0 * self.scale();
        layout.dividers.iter().copied().find(|d| d.rect.inflate(grab).contains(x, y))
    }

    pub(super) fn mouse_button(&mut self, state: ElementState, mouse_button: MouseButton, event_loop: &ActiveEventLoop) {
        self.mouse_press(state, mouse_button, event_loop);
        // What is under the pointer may have changed (a menu opened, a
        // start page became a terminal): show the right pointer at once.
        self.update_hover();
    }

    /// Ends a tab or sidebar-row drag when the button comes up: a row
    /// goes where the line in the sidebar showed it would.
    fn finish_tab_drag(&mut self) {
        if let Drag::Row { tab, moved: true, .. } = self.mouse.drag
            && let Some(spot) = self.sidebar.drop.take()
        {
            self.place_tab(tab, spot);
        }
        if matches!(self.mouse.drag, Drag::Tab { .. } | Drag::Row { .. }) {
            self.mouse.drag = Drag::None;
            self.sidebar.drop = None;
            self.redraw();
        }
    }

    fn mouse_press(&mut self, state: ElementState, mouse_button: MouseButton, event_loop: &ActiveEventLoop) {
        let (x, y) = (self.mouse.x, self.mouse.y);
        let code = match mouse_button {
            MouseButton::Left => button::LEFT,
            MouseButton::Middle => button::MIDDLE,
            MouseButton::Right => button::RIGHT,
            _ => return,
        };

        if state == ElementState::Released {
            if mouse_button == MouseButton::Left {
                self.finish_tab_drag();
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

        // A click anywhere but the tab being renamed ends the rename,
        // keeping the new name, and then does what it always does.
        if let Some((index, ..)) = self.rename_view() {
            let on_it = self.tab_bar().hit(x, y) == Some(TabBarHit::Tab(index));
            if on_it {
                return;
            }
            self.finish_rename(true);
        }

        // A dropdown menu takes every click: on a row picks it, elsewhere
        // closes the menu.
        if matches!(self.overlay, Overlay::Menu(_)) {
            self.menu_click(x, y, mouse_button);
            return;
        }

        // An open palette takes every click: on a row runs it, elsewhere
        // closes the palette.
        // The board: a click on a card goes to that agent; a click on the
        // empty part of the board does nothing; a click elsewhere (sidebar,
        // tabs) closes the board and is handled as usual.
        if matches!(self.overlay, Overlay::Board { .. }) {
            let area = self.content_rect();
            if area.contains(x, y) {
                let metrics = self.renderer.as_ref().map(|r| r.metrics());
                let target = metrics.and_then(|m| {
                    crate::board::hit(&self.board_model(), area, (m.width, m.height), self.scale(), x, y)
                });
                if let (Some(target), MouseButton::Left) = (target, mouse_button) {
                    self.close_overlay();
                    self.go_to(target);
                }
                return;
            }
            self.close_overlay();
        }

        // Settings: a click on a row selects it and, for switches and
        // choices, changes it; a click outside the panel closes the screen.
        if let Overlay::Settings { .. } = self.overlay {
            let metrics = self.renderer.as_ref().map(|r| r.metrics());
            let Some(metrics) = metrics else { return };
            let selected = match &self.overlay {
                Overlay::Settings { selected, .. } => *selected,
                _ => 0,
            };
            let layout = crate::settings::ScreenLayout::new(
                self.window_size(),
                (metrics.width, metrics.height),
                self.scale(),
                selected,
            );
            match layout.setting_at(x, y) {
                Some(index) if mouse_button == MouseButton::Left => {
                    self.overlay = Overlay::Settings { selected: index, editing: None };
                    self.settings_activate(index);
                }
                Some(_) => {}
                None if layout.frame.contains(x, y) => {}
                None => self.close_overlay(),
            }
            return;
        }

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

        // The docked board, in the board layout.
        if let Some(hit) = self.dock_hit(x, y) {
            if mouse_button == MouseButton::Left {
                self.dock_click(hit);
            }
            return;
        }

        // Sidebar.
        if let Some(hit) = self.sidebar_hit(x, y) {
            if mouse_button == MouseButton::Left {
                self.sidebar_click(hit);
                // Holding the button on a row and moving carries its tab
                // to another workspace.
                if let crate::sidebar::Hit::Row(target) = hit {
                    self.mouse.drag = Drag::Row { tab: target.tab, from: (x, y), moved: false };
                }
            }
            return;
        }
        // A click anywhere else ends a sidebar search.
        self.sidebar.searching = false;

        // Agent graph panel: clicking a node selects it.
        if self.graph_rect().is_some_and(|rect| rect.contains(x, y)) {
            if mouse_button == MouseButton::Left {
                self.graph_click(x, y);
            }
            return;
        }

        // Tab bar.
        if let Some(hit) = self.tab_bar().hit(x, y) {
            match (hit, mouse_button) {
                (TabBarHit::Tab(index), MouseButton::Left) => {
                    // A double-click on the tab already in front renames
                    // it in place.
                    let was_active = self.visible_tabs().get(index) == Some(&self.active);
                    let clicks = self.register_click();
                    if was_active && clicks == 2 {
                        self.start_rename(index);
                        return;
                    }
                    self.select_tab(index);
                    // Holding the button and moving reorders the tab.
                    let grab = self.tab_bar().tabs.get(index).map_or(0.0, |rect| x - rect.x);
                    if let Some(tab) = self.visible_tabs().get(index).and_then(|&i| self.tabs.get(i)) {
                        self.mouse.drag = Drag::Tab { id: tab.id, from: (x, y), grab, moved: false };
                    }
                }
                (TabBarHit::Close(index), MouseButton::Left) | (TabBarHit::Tab(index), MouseButton::Middle) => {
                    if let Some(&tab) = self.visible_tabs().get(index) {
                        self.close_tab(tab);
                    }
                }
                (TabBarHit::Workspace, MouseButton::Left) => self.open_list(ListKind::Workspaces),
                (TabBarHit::NewTab, MouseButton::Left) => self.new_tab(None),
                (TabBarHit::Layout(layout), MouseButton::Left) => self.set_layout(layout),
                (TabBarHit::Commands, MouseButton::Left) => self.perform(Action::CommandPalette, event_loop),
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

        // The start page handles its own clicks.
        if self.start_click(x, y) {
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
            // Remember where the press was, to tell a drag from a click.
            let (column, line, _) = self.cell_at(pane, rect, x, y);
            self.mouse.last_cell = Some((pane, column, line));
            self.mouse.program_selection = None;
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
    pub(super) fn register_click(&mut self) -> u8 {
        let now = Instant::now();
        let quick = now.duration_since(self.mouse.last_click) < Duration::from_millis(400);
        self.mouse.click_count = if quick { self.mouse.click_count % 3 + 1 } else { 1 };
        self.mouse.last_click = now;
        self.mouse.click_count
    }

    pub(super) fn mouse_moved(&mut self, position: PhysicalPosition<f64>) {
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
                    let before = self.mouse.last_cell;
                    self.report_mouse(pane, rect, MouseAction::Motion, button::LEFT, mode);
                    if self.mouse.last_cell != before {
                        self.mouse.program_selection = Some(pane);
                    }
                }
            }
            Drag::Tab { id, from, grab, moved } => {
                let far = (x - from.0).abs() > DRAG_THRESHOLD * self.scale();
                if moved || far {
                    self.mouse.drag = Drag::Tab { id, from, grab, moved: true };
                    // The tab takes the place of whichever tab the pointer
                    // is over, and is drawn following the pointer.
                    let bar = self.tab_bar();
                    if let Some(position) = bar.tabs.iter().position(|tab| x >= tab.x && x < tab.x + tab.w) {
                        self.reorder_tab(id, position);
                    }
                    self.redraw();
                }
            }
            Drag::Row { tab, from, moved } => {
                let far = (x - from.0).abs().max((y - from.1).abs()) > DRAG_THRESHOLD * self.scale();
                if moved || far {
                    self.mouse.drag = Drag::Row { tab, from, moved: true };
                    self.sidebar.drop = self.sidebar_drop_at(x, y, tab);
                    // The row's name follows the pointer.
                    self.redraw();
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
        self.update_hover();
    }

    /// Updates everything that depends on what is under the pointer: hover
    /// highlights and the pointer's shape.
    pub(super) fn update_hover(&mut self) {
        let (x, y) = (self.mouse.x, self.mouse.y);
        // Which tab the pointer is over decides where a close button shows.
        let hover_tab = if self.overlay.is_modal() {
            None
        } else {
            self.tab_bar()
                .hit(x, y)
                .filter(|hit| matches!(hit, TabBarHit::Tab(_) | TabBarHit::Close(_) | TabBarHit::Layout(_) | TabBarHit::Commands))
        };
        if hover_tab != self.mouse.hover_tab {
            // Moving between a tab and its close button is the same tab.
            let tab_of = |hit: Option<TabBarHit>| match hit {
                Some(TabBarHit::Tab(index) | TabBarHit::Close(index)) => Some(index),
                _ => None,
            };
            if tab_of(hover_tab) != tab_of(self.mouse.hover_tab) {
                self.mouse.hover_tab_since = Instant::now();
            }
            self.mouse.hover_tab = hover_tab;
            self.redraw();
        }
        let hover_dock = if self.overlay.is_modal() { None } else { self.dock_hit(x, y) };
        if hover_dock != self.dock.hovered {
            self.dock.hovered = hover_dock;
            self.redraw();
        }
        let hover_sidebar = if self.overlay.is_modal() { None } else { self.sidebar_hit(x, y) };
        if hover_sidebar != self.sidebar.hovered {
            self.sidebar.hovered = hover_sidebar;
            self.redraw();
        }
        // The start page highlights the control under the pointer; it is
        // cheap to redraw, so just do it when the pointer is over it.
        if self.start_page().is_some() && self.content_rect().contains(x, y) {
            self.redraw();
        }
        if matches!(self.overlay, Overlay::Menu(_)) {
            self.menu_hover(x, y);
        }
        // The tabs sit in the window's title bar, where the system would
        // take a drag to move the window: over a tab, and while one is
        // being dragged, the window stays put.
        let over_tab = matches!(hover_tab, Some(TabBarHit::Tab(_) | TabBarHit::Close(_)));
        let dragging = matches!(self.mouse.drag, Drag::Tab { .. } | Drag::Row { .. });
        self.set_window_movable(!over_tab && !dragging);
        self.update_cursor();
    }

    /// The pointer's shape: a hand over things that can be clicked, a text
    /// cursor over text, an arrow elsewhere.
    fn cursor_icon(&mut self) -> CursorIcon {
        let (x, y) = (self.mouse.x, self.mouse.y);
        match self.mouse.drag {
            Drag::Divider(Divider { axis: Axis::Horizontal, .. }) => return CursorIcon::ColResize,
            Drag::Divider(Divider { axis: Axis::Vertical, .. }) => return CursorIcon::RowResize,
            Drag::Select(_) => return CursorIcon::Text,
            Drag::Tab { moved: true, .. } | Drag::Row { moved: true, .. } => return CursorIcon::Grabbing,
            Drag::Reported(_) | Drag::None | Drag::Tab { .. } | Drag::Row { .. } => {}
        }
        let metrics = self.renderer.as_ref().map(|r| r.metrics());
        let cell = metrics.map_or((8.0, 16.0), |m| (m.width, m.height));
        let hand = |clickable: bool| if clickable { CursorIcon::Pointer } else { CursorIcon::Default };

        match &self.overlay {
            Overlay::Menu(_) => {
                return hand(self.menu_layout().is_some_and(|layout| layout.row_at(x, y).is_some()));
            }
            Overlay::Palette { kind, query, selected } => {
                let count = self.list_items(*kind, query).len();
                let Some(layout) = self.palette_layout(count, *selected) else { return CursorIcon::Default };
                if layout.input.contains(x, y) {
                    return CursorIcon::Text;
                }
                return hand(layout.row_at(x, y).is_some());
            }
            Overlay::Input { .. } => {
                let input = self.palette_layout(0, 0).map(|layout| layout.input);
                return if input.is_some_and(|input| input.contains(x, y)) { CursorIcon::Text } else { CursorIcon::Default };
            }
            Overlay::Settings { selected, .. } => {
                let layout = crate::settings::ScreenLayout::new(self.window_size(), cell, self.scale(), *selected);
                return hand(layout.setting_at(x, y).is_some());
            }
            Overlay::Confirm(_) => return CursorIcon::Default,
            Overlay::Board { .. } => {
                let area = self.content_rect();
                if area.contains(x, y) {
                    let card = crate::board::hit(&self.board_model(), area, cell, self.scale(), x, y);
                    return hand(card.is_some());
                }
            }
            Overlay::Search { .. } | Overlay::None => {}
        }

        if let Some(hit) = self.dock_hit(x, y) {
            use crate::dock::Hit;
            return hand(matches!(hit, Hit::Card(_) | Hit::FewerRows | Hit::MoreRows));
        }
        if let Some(hit) = self.sidebar_hit(x, y) {
            use crate::sidebar::Hit;
            return match hit {
                Hit::Search => CursorIcon::Text,
                Hit::TitleBar | Hit::Nothing => CursorIcon::Default,
                _ => CursorIcon::Pointer,
            };
        }
        if let Some(rect) = self.graph_rect()
            && rect.contains(x, y)
        {
            let scale = self.scale();
            let node = self.graph_model().and_then(|model| crate::graph::hit(&model, rect, cell, scale, x, y));
            return hand(node.is_some());
        }
        if let Some(hit) = self.tab_bar().hit(x, y) {
            return hand(!matches!(hit, TabBarHit::Empty));
        }
        if self.tab_bar().bar.contains(x, y) {
            return CursorIcon::Default;
        }
        if self.start_page().is_some() {
            return match self.start_hit(x, y) {
                Some(crate::start::Hit::Input) => CursorIcon::Text,
                Some(_) => CursorIcon::Pointer,
                None => CursorIcon::Default,
            };
        }
        if self.hover_link.is_some() {
            return CursorIcon::Pointer;
        }
        let layout = self.active_layout();
        match self.divider_at(&layout, x, y) {
            Some(Divider { axis: Axis::Horizontal, .. }) => CursorIcon::ColResize,
            Some(Divider { axis: Axis::Vertical, .. }) => CursorIcon::RowResize,
            None => match layout.pane_at(x, y) {
                // A program that takes the mouse itself (an editor, a
                // full-screen agent) gets an arrow, as in other terminals.
                Some(pane) if self.mouse_reporting(pane).is_some() => CursorIcon::Default,
                Some(_) => CursorIcon::Text,
                None => CursorIcon::Default,
            },
        }
    }

    pub(super) fn update_cursor(&mut self) {
        let icon = self.cursor_icon();
        if icon != self.mouse.cursor {
            self.mouse.cursor = icon;
            if let Some(window) = &self.window {
                window.set_cursor(icon);
            }
        }
    }

    pub(super) fn palette_layout(&self, count: usize, selected: usize) -> Option<PaletteLayout> {
        let metrics = self.renderer.as_ref()?.metrics();
        let cell = (metrics.width, metrics.height);
        Some(PaletteLayout::new(self.window_size(), cell, self.scale(), count, selected))
    }

    /// Recomputes which link (if any) is under the mouse. Links are only
    /// active while Cmd is held, so normal clicks and selection are never
    /// affected.
    pub(super) fn update_hover_link(&mut self) {
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
        self.update_hover_preview();
    }

    /// Shows a picture while the pointer rests on a mention of it: an
    /// `[Image #N]` in an agent's pane, or (with Cmd held, like any link)
    /// the path of an image file.
    fn update_hover_preview(&mut self) {
        let found = self.preview_target();
        let same = match (&found, &self.hover_preview) {
            (None, None) => return,
            (Some(a), Some(b)) => a.0 == b.source && a.1 == b.pane && a.2 == b.start,
            _ => false,
        };
        let have_image = self.renderer.as_ref().is_some_and(|r| r.has_image(crate::preview::KEY));
        if same && have_image {
            return;
        }
        self.hover_preview = None;
        let window = self.window_size();
        if let Some((source, pane, start, end)) = found
            && let Some(renderer) = self.renderer.as_mut()
        {
            // At most half the window either way, so it never hides
            // everything.
            let (max_w, max_h) = ((window.0 * 0.5) as u32, (window.1 * 0.5) as u32);
            if let Some((width, height, pixels)) = crate::preview::load(&source, max_w.max(64), max_h.max(64)) {
                renderer.upload_image(crate::preview::KEY, width, height, &pixels);
                let anchor = (self.mouse.x, self.mouse.y);
                self.hover_preview = Some(HoverPreview { source, pane, start, end, anchor, size: (width, height) });
            }
        }
        if self.hover_preview.is_none()
            && let Some(renderer) = self.renderer.as_mut()
        {
            renderer.remove_image(crate::preview::KEY);
        }
        self.redraw();
    }

    /// The picture mentioned under the pointer, if any: its file, and the
    /// pane and cells of the mention.
    fn preview_target(&self) -> Option<(PathBuf, PaneId, Point, Point)> {
        if self.overlay.is_open() || !matches!(self.mouse.drag, Drag::None) {
            return None;
        }
        if let Some(link) = &self.hover_link {
            return match &link.target {
                Target::File(path) if crate::preview::is_image(path) => {
                    Some((path.clone(), link.pane, link.start, link.end))
                }
                _ => None,
            };
        }
        let (x, y) = (self.mouse.x, self.mouse.y);
        let layout = self.active_layout();
        let pane_id = layout.pane_at(x, y)?;
        let rect = layout.rect_of(pane_id)?;
        let pane = self.panes.get(&pane_id)?;
        let (column, line, _) = self.cell_at(pane_id, rect, x, y);
        let (chars, points, at) = {
            let term = pane.terminal.lock();
            let point = term::viewport_to_point(term.grid().display_offset(), Point::new(line, Column(column)));
            logical_line(&term, point)?
        };
        let (start, end, number) = crate::preview::image_tag_at(&chars, at)?;
        // Where the agent keeps the picture. Only Claude Code is known.
        let session = pane.agent.as_ref();
        let cwd = session.and_then(|s| s.cwd.clone()).or_else(|| pane.terminal.working_directory());
        let path = crate::preview::claude_image(
            &crate::preview::claude_temp_dir(),
            session.and_then(|s| s.session_id.as_deref()),
            cwd.as_deref(),
            number,
        )?;
        Some((path, pane_id, points[start], points[end - 1]))
    }

    /// Finds a link at a window position: an explicit hyperlink set by the
    /// program, a web address, or a path to an existing file.
    pub(super) fn link_at(&self, pane_id: PaneId, rect: Rect, x: f32, y: f32) -> Option<HoverLink> {
        let pane = self.panes.get(&pane_id)?;
        let (column, line, _) = self.cell_at(pane_id, rect, x, y);
        let term = pane.terminal.lock();
        let point = term::viewport_to_point(term.grid().display_offset(), Point::new(line, Column(column)));
        let grid = term.grid();
        let last_column = term.last_column();

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
        let (chars, points, at) = logical_line(&term, point)?;
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

    pub(super) fn mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let (x, y) = (self.mouse.x, self.mouse.y);
        let Some(cell_height) = self.renderer.as_ref().map(|r| r.metrics().height) else { return };
        if self.sidebar_rect().is_some_and(|rect| rect.contains(x, y)) {
            let pixels = match delta {
                MouseScrollDelta::LineDelta(_, lines) => lines * cell_height * 3.0,
                MouseScrollDelta::PixelDelta(position) => position.y as f32,
            };
            return self.sidebar_scroll(pixels);
        }
        if let Some(hit) = self.dock_hit(x, y) {
            let column = match hit {
                crate::dock::Hit::Column(column) => Some(column),
                crate::dock::Hit::Card(target) => {
                    self.board_cards().iter().position(|cards| cards.iter().any(|card| card.target == target))
                }
                _ => None,
            };
            if let Some(column) = column {
                let pixels = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => lines * cell_height * 3.0,
                    MouseScrollDelta::PixelDelta(position) => position.y as f32,
                };
                self.dock_scroll(column, pixels);
            }
            return;
        }
        let layout = self.active_layout();
        let Some(id) = layout.pane_at(x, y) else { return };
        let Some(rect) = layout.rect_of(id) else { return };
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
}

/// The text of the logical line through `point` (a line the terminal
/// wrapped over several rows counts as one), the cell each character sits
/// in, and the index of the character at `point`.
fn logical_line<T>(term: &ale_core::vt::Term<T>, point: Point) -> Option<(Vec<char>, Vec<Point>, usize)> {
    let grid = term.grid();
    let last_column = term.last_column();
    let wraps = |line: Line| grid[line][last_column].flags.contains(Flags::WRAPLINE);
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
    Some((chars, points, at))
}
