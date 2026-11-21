//! The command palette, list and text prompts, confirmations, scrollback
//! search and prompt jumping: everything that happens while an overlay is
//! open, plus opening and closing them.

use super::*;

impl App {
    /// Keyboard handling while the palette or search bar is open: keys edit
    /// the query instead of going to the shell.
    pub(super) fn overlay_key(&mut self, event: &KeyEvent, action: Option<Action>, event_loop: &ActiveEventLoop) {
        let shift = self.modifiers.shift_key();
        let command = self.modifiers.super_key() || self.modifiers.control_key();
        if matches!(self.overlay, Overlay::Settings { .. }) && self.settings_key(event) {
            return;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => return self.close_overlay(),
            Key::Named(NamedKey::Enter) => return self.overlay_confirm(shift, event_loop),
            Key::Named(NamedKey::ArrowUp) => return self.overlay_move(-1),
            Key::Named(NamedKey::ArrowDown) => return self.overlay_move(1),
            Key::Named(NamedKey::ArrowLeft) if matches!(self.overlay, Overlay::Board { .. }) => {
                return self.board_move(-1, 0);
            }
            Key::Named(NamedKey::ArrowRight) if matches!(self.overlay, Overlay::Board { .. }) => {
                return self.board_move(1, 0);
            }
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
            Some(Action::CommandPalette | Action::Find | Action::SwitchWorkspace | Action::ToggleBoard) => {
                self.close_overlay()
            }
            Some(_) => {}
            None if !command => {
                if let Some(text) = event.text.as_deref() {
                    self.overlay_type(text);
                }
            }
            None => {}
        }
    }

    pub(super) fn overlay_type(&mut self, text: &str) {
        if let Some(query) = self.overlay.query_mut() {
            query.extend(text.chars().filter(|c| !c.is_control()));
            self.overlay_changed();
        }
    }

    /// The query was edited.
    pub(super) fn overlay_changed(&mut self) {
        match &mut self.overlay {
            Overlay::Palette { selected, .. } => *selected = 0,
            Overlay::Search { query, .. } => {
                self.last_search = query.clone();
                self.search_step(SearchDirection::Left, false);
            }
            Overlay::None
            | Overlay::Input { .. }
            | Overlay::Confirm(_)
            | Overlay::Board { .. }
            | Overlay::Settings { .. } => {}
        }
        self.redraw();
    }

    pub(super) fn overlay_move(&mut self, offset: isize) {
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
            Overlay::Board { .. } => self.board_move(0, offset),
            Overlay::Settings { .. } | Overlay::None | Overlay::Input { .. } | Overlay::Confirm(_) => {}
        }
    }

    pub(super) fn overlay_confirm(&mut self, shift: bool, event_loop: &ActiveEventLoop) {
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
            Overlay::Board { .. } => {
                let target = self.board_model().selected_card().map(|card| card.target);
                self.close_overlay();
                if let Some(target) = target {
                    self.go_to(target);
                }
            }
            Overlay::Search { .. } => {
                let direction = if shift { SearchDirection::Right } else { SearchDirection::Left };
                self.search_step(direction, true);
            }
            Overlay::Settings { .. } | Overlay::None => {}
        }
    }

    pub(super) fn run_choice(&mut self, choice: Option<Choice>, event_loop: &ActiveEventLoop) {
        match choice {
            Some(Choice::Run(action)) => self.perform(action, event_loop),
            Some(choice) => self.choose(choice),
            None => {}
        }
    }

    pub(super) fn close_overlay(&mut self) {
        // Dismissing the hook offer is an answer: do not ask again.
        if matches!(&self.overlay, Overlay::Confirm(confirm) if confirm.install_hooks) {
            self.decline_hooks();
        }
        self.overlay = Overlay::None;
        self.redraw();
    }

    pub(super) fn open_search(&mut self) {
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
    pub(super) fn search_step(&mut self, direction: SearchDirection, advance: bool) {
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
    pub(super) fn jump_to_prompt(&mut self, previous: bool) {
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
    pub(super) fn send_input(&mut self, bytes: Vec<u8>) {
        if let Some(id) = self.focused_pane_id()
            && let Some(session) = self.panes.get_mut(&id).and_then(|pane| pane.agent.as_mut())
        {
            // So the echo of this typing is not taken for the agent working.
            session.on_input(Instant::now());
        }
        let Some(pane) = self.focused_pane() else { return };
        {
            let mut term = pane.terminal.lock();
            term.scroll_display(Scroll::Bottom);
            term.selection = None;
        }
        pane.terminal.write(bytes);
        self.redraw();
    }

    pub(super) fn copy(&mut self) {
        let Some(pane) = self.focused_pane() else { return };
        let text = pane.terminal.lock().selection_to_string();
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            set_clipboard(text);
        }
    }

    pub(super) fn paste(&mut self) {
        let Some(pane) = self.focused_pane() else { return };
        let Some(text) = get_clipboard() else { return };
        let mode = *pane.terminal.lock().mode();
        self.send_input(input::encode_paste(&text, mode));
    }

    /// Cmd+K: forget the scrollback and ask the shell to redraw its prompt.
    pub(super) fn clear_history(&mut self) {
        let Some(pane) = self.focused_pane() else { return };
        pane.terminal.lock().grid_mut().clear_history();
        pane.terminal.write(&b"\x0c"[..]);
        self.redraw();
    }

    // ---- Settings screen ----------------------------------------------------

    /// Keyboard handling for the settings screen. Returns true when the
    /// key was used (Esc while not editing is left to close the screen).
    fn settings_key(&mut self, event: &KeyEvent) -> bool {
        let Overlay::Settings { selected, editing } = &mut self.overlay else { return false };
        let count = crate::settings::SETTINGS.len();
        let index = (*selected).min(count - 1);

        // Typing a text value.
        if let Some(text) = editing {
            match &event.logical_key {
                Key::Named(NamedKey::Enter) => {
                    let value = ale_config::SettingValue::Text(text.trim().to_owned());
                    *editing = None;
                    self.settings_write(index, value);
                }
                Key::Named(NamedKey::Escape) => *editing = None,
                Key::Named(NamedKey::Backspace) => {
                    text.pop();
                }
                _ => {
                    let plain = !self.modifiers.super_key() && !self.modifiers.control_key();
                    if let (true, Some(typed)) = (plain, event.text.as_deref()) {
                        text.extend(typed.chars().filter(|c| !c.is_control()));
                    }
                }
            }
            self.redraw();
            return true;
        }

        let step = |app: &mut Self, direction: i32| {
            let setting = &crate::settings::SETTINGS[index];
            let value = crate::settings::current(&app.config, setting);
            if let Some(new) = crate::settings::stepped(setting, &value, direction) {
                app.settings_write(index, new);
            }
        };
        match &event.logical_key {
            Key::Named(NamedKey::ArrowUp) => *selected = index.saturating_sub(1),
            Key::Named(NamedKey::ArrowDown) => *selected = (index + 1).min(count - 1),
            Key::Named(NamedKey::ArrowLeft) => step(self, -1),
            Key::Named(NamedKey::ArrowRight) => step(self, 1),
            Key::Named(NamedKey::Enter | NamedKey::Space) => self.settings_activate(index),
            // Cmd+E: the file itself, for everything not listed here.
            Key::Character(key) if key.as_str() == "e" && self.modifiers.super_key() => open_config_file(),
            _ => return false,
        }
        self.redraw();
        true
    }

    /// Enter or a click on a setting: toggles and choices change, text
    /// starts being edited, numbers are left to the arrow keys.
    pub(super) fn settings_activate(&mut self, index: usize) {
        let setting = &crate::settings::SETTINGS[index];
        let value = crate::settings::current(&self.config, setting);
        match setting.kind {
            crate::settings::Kind::Text => {
                if let Overlay::Settings { editing, .. } = &mut self.overlay {
                    *editing = Some(crate::settings::display(&value));
                }
            }
            crate::settings::Kind::Toggle | crate::settings::Kind::Choice(_) => {
                if let Some(new) = crate::settings::stepped(setting, &value, 1) {
                    self.settings_write(index, new);
                }
            }
            crate::settings::Kind::Number { .. } => {}
        }
        self.redraw();
    }

    /// Test hook: steps the setting named `section.key` up ("+") or down.
    pub(super) fn script_setting(&mut self, change: &str) {
        let mut parts = change.rsplitn(2, ':');
        let (direction, name) = (parts.next().unwrap_or("+"), parts.next().unwrap_or(""));
        let found = crate::settings::SETTINGS
            .iter()
            .position(|setting| format!("{}.{}", setting.section, setting.key) == name);
        let Some(index) = found else { return eprintln!("ale-term: unknown setting {name:?}") };
        let setting = &crate::settings::SETTINGS[index];
        let value = crate::settings::current(&self.config, setting);
        if let Some(new) = crate::settings::stepped(setting, &value, if direction == "-" { -1 } else { 1 }) {
            self.settings_write(index, new);
        }
        if let Overlay::Settings { selected, .. } = &mut self.overlay {
            *selected = index;
        }
    }

    /// Writes a setting to the file and applies it at once.
    fn settings_write(&mut self, index: usize, value: ale_config::SettingValue) {
        let setting = &crate::settings::SETTINGS[index];
        match ale_config::set_value(setting.section, setting.key, &value) {
            Ok(()) => self.reload_config(),
            Err(error) => {
                let lines = error.lines().take(8).map(str::to_owned).collect();
                self.show_message("The setting could not be saved", lines);
            }
        }
    }
}
