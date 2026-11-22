//! Dropdown menus in the app: opening them, following the pointer, and
//! doing what the chosen row says.

use super::*;
use crate::menu::{self, Menu, MenuKind, MenuLayout};

impl App {
    /// Opens the "New session" menu under the sidebar's button.
    pub(super) fn open_new_session_menu(&mut self) {
        let Some((rect, cell, scale)) = self.sidebar_geometry() else {
            // No sidebar to hang it from: just start an agent session.
            return self.new_agent_session();
        };
        let anchor = crate::sidebar::new_session_button(rect, cell, scale);
        self.overlay = Overlay::Menu(Box::new(Menu::new_session(anchor)));
        self.redraw();
    }

    /// Opens the split button's menu under it.
    pub(super) fn open_split_menu(&mut self) {
        let anchor = self.tab_bar().split;
        let rows = [(Action::SplitRight, None), (Action::SplitDown, None)];
        self.overlay = Overlay::Menu(Box::new(Menu::commands(anchor, &rows)));
        self.redraw();
    }

    /// Opens a pane's right-click menu at the pointer: splitting, the
    /// clipboard, and the pane and its tab.
    pub(super) fn open_pane_menu(&mut self, x: f32, y: f32) {
        let has_selection = self.focused_pane().is_some_and(|pane| pane.terminal.lock().selection.is_some());
        let mut rows = vec![(Action::SplitRight, None), (Action::SplitDown, None)];
        if has_selection {
            rows.push((Action::Copy, None));
        }
        rows.extend([
            (Action::Paste, None),
            (Action::ClearHistory, None),
            (Action::RenameTab, Some("Rename Tab…")),
            (Action::ClosePane, None),
        ]);
        let anchor = Rect::new(x, y, 0.0, 0.0);
        self.overlay = Overlay::Menu(Box::new(Menu::commands(anchor, &rows)));
        self.redraw();
    }

    pub(super) fn menu_layout(&self) -> Option<MenuLayout> {
        let Overlay::Menu(menu) = &self.overlay else { return None };
        let metrics = self.renderer.as_ref()?.metrics();
        Some(MenuLayout::new(menu, self.window_size(), (metrics.width, metrics.height), self.scale()))
    }

    /// Closes the open menu and does what row `index` says.
    pub(super) fn menu_choose(&mut self, index: usize, event_loop: &ActiveEventLoop) {
        let Overlay::Menu(menu) = std::mem::replace(&mut self.overlay, Overlay::None) else { return };
        match menu.kind {
            MenuKind::NewSession if index == menu::NEW_SESSION_AGENT => self.new_agent_session(),
            MenuKind::NewSession if index == menu::NEW_SESSION_TERMINAL => self.new_tab(None),
            MenuKind::NewSession => {}
            MenuKind::Agent => {
                if let Some(agent) = menu.items.get(index).and_then(|item| item.agent) {
                    self.start_set_agent(agent);
                }
            }
            MenuKind::Model | MenuKind::Effort => {
                if let Some(item) = menu.items.get(index) {
                    self.start_set_option(menu.kind, item.value);
                }
            }
            MenuKind::Folder => match menu.items.get(index).map(|item| item.folder.clone()) {
                Some(Some(folder)) => {
                    if let Some(tab) = self.tabs.get(self.active).map(|tab| tab.id) {
                        self.start_set_folder(tab, folder);
                    }
                }
                Some(None) => self.choose_folder(),
                None => {}
            },
            MenuKind::Commands => {
                if let Some(action) = menu.items.get(index).and_then(|item| item.action) {
                    self.perform(action, event_loop);
                }
            }
        }
        self.redraw();
    }

    /// A click while a menu is open: on a row picks it; anywhere else
    /// closes the menu, and the click does nothing more (as with the
    /// system's menus).
    pub(super) fn menu_click(&mut self, x: f32, y: f32, button: MouseButton, event_loop: &ActiveEventLoop) {
        match self.menu_layout().and_then(|layout| layout.row_at(x, y)) {
            Some(index) if button == MouseButton::Left => self.menu_choose(index, event_loop),
            Some(_) => {}
            None => self.close_overlay(),
        }
    }

    /// Highlights the row under the pointer. Returns whether the pointer
    /// is over a row.
    pub(super) fn menu_hover(&mut self, x: f32, y: f32) -> bool {
        let Some(index) = self.menu_layout().and_then(|layout| layout.row_at(x, y)) else { return false };
        if let Overlay::Menu(menu) = &mut self.overlay
            && menu.selected != index
        {
            menu.selected = index;
            self.redraw();
        }
        true
    }
}
