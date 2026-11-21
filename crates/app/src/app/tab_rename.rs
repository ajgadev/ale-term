//! Renaming a tab in place: double-click its title, type, press Enter.
//!
//! While a rename is under way the tab's title is a small text field. Enter
//! or a click elsewhere keeps the new name, Esc keeps the old one, and an
//! empty name gives the tab its automatic title back (as with the "Rename
//! Tab" command).

use super::*;

/// A tab whose title is being edited.
pub struct Renaming {
    pub tab: TabId,
    pub text: String,
    /// The old name is still selected: typing replaces it.
    pub selected: bool,
}

impl App {
    /// Starts editing the title of the tab at position `index` of the tab
    /// bar, beginning with what it shows now.
    pub(super) fn start_rename(&mut self, index: usize) {
        let Some(&tab_index) = self.visible_tabs().get(index) else { return };
        let Some(tab) = self.tabs.get(tab_index) else { return };
        if tab.start.is_some() {
            // A start page's tab is named when its session starts.
            return;
        }
        let text = tab.custom_title.clone().unwrap_or_else(|| self.tab_title(tab));
        self.renaming = Some(Renaming { tab: tab.id, text, selected: true });
        self.mouse.drag = Drag::None;
        self.redraw();
    }

    /// Ends the rename, keeping the typed name when `keep`.
    pub(super) fn finish_rename(&mut self, keep: bool) {
        let Some(renaming) = self.renaming.take() else { return };
        if keep && let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == renaming.tab) {
            let text = renaming.text.trim();
            tab.custom_title = (!text.is_empty()).then(|| text.to_owned());
            self.mark_dirty();
        }
        self.redraw();
    }

    /// The tab bar position of the tab being renamed, its text so far, and
    /// whether that text is selected.
    pub(super) fn rename_view(&self) -> Option<(usize, &str, bool)> {
        let renaming = self.renaming.as_ref()?;
        let index = self.visible_tabs().iter().position(|&i| self.tabs.get(i).is_some_and(|t| t.id == renaming.tab))?;
        Some((index, renaming.text.as_str(), renaming.selected))
    }

    /// Keyboard input while a tab is being renamed. Returns true when the
    /// key was used. A shortcut ends the rename (keeping the name) and then
    /// does what it always does, except paste, which pastes into the name.
    pub(super) fn rename_key(&mut self, event: &KeyEvent, action: Option<Action>) -> bool {
        if self.renaming.is_none() {
            return false;
        }
        match (&event.logical_key, action) {
            (_, Some(Action::Paste)) => {
                let pasted = get_clipboard().unwrap_or_default();
                self.rename_type(&pasted.replace(['\n', '\r'], " "));
            }
            (_, Some(_)) => {
                self.finish_rename(true);
                return false;
            }
            (Key::Named(NamedKey::Enter), _) => self.finish_rename(true),
            (Key::Named(NamedKey::Escape), _) => self.finish_rename(false),
            (Key::Named(NamedKey::Backspace), _) => {
                if let Some(renaming) = self.renaming.as_mut() {
                    if self.modifiers.super_key() || renaming.selected {
                        renaming.text.clear();
                    } else {
                        renaming.text.pop();
                    }
                    renaming.selected = false;
                }
            }
            // The arrows drop the selection, to add to the old name.
            (Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::End), _) => {
                if let Some(renaming) = self.renaming.as_mut() {
                    renaming.selected = false;
                }
            }
            _ => {
                let plain = !self.modifiers.super_key() && !self.modifiers.control_key();
                if let (true, Some(text)) = (plain, event.text.as_deref()) {
                    self.rename_type(text);
                }
            }
        }
        self.redraw();
        true
    }

    /// Adds typed text to the name. Returns false when no tab is being
    /// renamed.
    pub(super) fn rename_type(&mut self, text: &str) -> bool {
        let Some(renaming) = self.renaming.as_mut() else { return false };
        if renaming.selected {
            renaming.text.clear();
            renaming.selected = false;
        }
        renaming.text.extend(text.chars().filter(|c| !c.is_control()));
        self.redraw();
        true
    }
}
