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

        // Sidebar.
        if let Some(hit) = self.sidebar_hit(x, y) {
            if mouse_button == MouseButton::Left {
                self.sidebar_click(hit);
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
        let over_sidebar = self.sidebar_rect().is_some_and(|rect| rect.contains(x, y));
        let over_graph = self.graph_rect().is_some_and(|rect| rect.contains(x, y));
        let icon = if over_sidebar || over_graph || self.tab_bar().bar.contains(x, y) || self.overlay.is_modal() {
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
