//! Dropdown menus: a short list of choices that opens under the control
//! that asked for it, such as the sidebar's "New session" button or the
//! start page's agent picker.
//!
//! This module holds the menu's state, layout, drawing and hit-testing;
//! the app decides what picking a row does.

use ale_agents::AgentKind;
use ale_config::Palette;
use ale_render::Renderer;

use crate::draw::color;
use crate::layout::Rect;

/// What a menu is choosing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    /// What to open: an agent's start page or a plain terminal.
    NewSession,
    /// Which agent the start page will run.
    Agent,
    /// Which folder the start page's session runs in.
    Folder,
    /// Commands to run, such as the split button's menu or a pane's
    /// right-click menu.
    Commands,
}

/// One row of a menu.
pub struct MenuItem {
    pub label: String,
    /// Shown dimmed at the right, e.g. a shortcut.
    pub detail: String,
    /// Image key of an icon drawn before the label.
    pub icon: Option<u64>,
    /// Marks the current choice with a check.
    pub checked: bool,
    /// The agent this row picks, in the agent menu.
    pub agent: Option<AgentKind>,
    /// The folder this row picks, in the folder menu. `None` on the row
    /// that asks for one.
    pub folder: Option<std::path::PathBuf>,
    /// The command this row runs, in a menu of commands.
    pub action: Option<crate::actions::Action>,
}

pub struct Menu {
    pub kind: MenuKind,
    /// The control the menu belongs to; the menu opens below it (or above
    /// when there is no room below).
    pub anchor: Rect,
    pub items: Vec<MenuItem>,
    /// The highlighted row: follows the pointer and the arrow keys.
    pub selected: usize,
}

/// The two kinds of new session, in menu order.
pub const NEW_SESSION_AGENT: usize = 0;
pub const NEW_SESSION_TERMINAL: usize = 1;

impl Menu {
    pub fn new_session(anchor: Rect) -> Menu {
        let item = |label: &str, detail: &str| MenuItem {
            label: label.into(),
            detail: detail.into(),
            icon: None,
            checked: false,
            agent: None,
            folder: None,
            action: None,
        };
        Menu {
            kind: MenuKind::NewSession,
            anchor,
            items: vec![item("Agent", "⇧⌘T"), item("Terminal", "⌘T")],
            selected: NEW_SESSION_AGENT,
        }
    }

    /// The agents to choose from, each with where it is installed (shown
    /// dimmed; empty when not known).
    pub fn agents(anchor: Rect, current: AgentKind, choices: &[(AgentKind, String)]) -> Menu {
        let items = choices
            .iter()
            .map(|(kind, location)| MenuItem {
                label: kind.name().into(),
                detail: location.clone(),
                icon: Some(crate::icons::key(*kind)),
                checked: *kind == current,
                agent: Some(*kind),
                folder: None,
                action: None,
            })
            .collect();
        let selected = choices.iter().position(|(kind, _)| *kind == current).unwrap_or(0);
        Menu { kind: MenuKind::Agent, anchor, items, selected }
    }

    /// Folders to run a session in, `current` checked, and a last row that
    /// asks for any other folder.
    pub fn folders(anchor: Rect, current: &std::path::Path, choices: &[(String, std::path::PathBuf)]) -> Menu {
        let mut items: Vec<MenuItem> = choices
            .iter()
            .map(|(label, path)| MenuItem {
                label: label.clone(),
                detail: String::new(),
                icon: None,
                checked: path == current,
                agent: None,
                folder: Some(path.clone()),
                action: None,
            })
            .collect();
        items.push(MenuItem {
            label: "Choose folder…".into(),
            detail: String::new(),
            icon: None,
            checked: false,
            agent: None,
            folder: None,
            action: None,
        });
        let selected = items.iter().position(|item| item.checked).unwrap_or(0);
        Menu { kind: MenuKind::Folder, anchor, items, selected }
    }

    /// A menu of commands, each shown with its name from the command
    /// palette (or `label`, when given) and its shortcut.
    pub fn commands(anchor: Rect, rows: &[(crate::actions::Action, Option<&str>)]) -> Menu {
        let items = rows
            .iter()
            .map(|(action, label)| {
                let (name, keys) = crate::actions::COMMANDS
                    .iter()
                    .find(|(known, ..)| known == action)
                    .map_or(("", ""), |(_, name, keys)| (*name, *keys));
                MenuItem {
                    label: label.unwrap_or(name).to_owned(),
                    detail: keys.to_owned(),
                    icon: None,
                    checked: false,
                    agent: None,
                    folder: None,
                    action: Some(*action),
                }
            })
            .collect();
        Menu { kind: MenuKind::Commands, anchor, items, selected: 0 }
    }

    pub fn step(&mut self, offset: isize) {
        let count = self.items.len().max(1) as isize;
        self.selected = (self.selected as isize + offset).rem_euclid(count) as usize;
    }
}

/// Where the menu and its rows are on screen.
pub struct MenuLayout {
    pub frame: Rect,
    pub rows: Vec<Rect>,
}

impl MenuLayout {
    pub fn new(menu: &Menu, window: (f32, f32), cell: (f32, f32), scale: f32) -> MenuLayout {
        let pad = (10.0 * scale).round();
        let gap = (4.0 * scale).round();
        let row_height = (cell.1 + 10.0 * scale).round();
        // Room for the check, an icon, the label and the detail, matching
        // what `draw` puts in a row.
        let widest = menu
            .items
            .iter()
            .map(|item| {
                let detail = item.detail.chars().count();
                item.label.chars().count() + if detail > 0 { detail + 4 } else { 0 }
            })
            .max()
            .unwrap_or(0) as f32;
        let check = if menu.items.iter().any(|item| item.checked) { cell.0 * 2.0 } else { 0.0 };
        let icon = if menu.items.iter().any(|item| item.icon.is_some()) { cell.1.round() + cell.0 } else { 0.0 };
        let content = gap * 2.0 + pad * 2.0 + check + icon + (widest + 1.0) * cell.0;
        let width = content.max(menu.anchor.w).min(window.0 - gap * 2.0).round();
        let height = row_height * menu.items.len() as f32 + gap * 2.0;

        let x = menu.anchor.x.min(window.0 - width - gap).max(gap);
        let below = menu.anchor.y + menu.anchor.h + gap;
        let y = if below + height <= window.1 - gap || menu.anchor.y - gap - height < 0.0 {
            below
        } else {
            menu.anchor.y - gap - height
        };
        let frame = Rect::new(x, y, width, height);
        let rows = (0..menu.items.len())
            .map(|index| Rect::new(x + gap, y + gap + row_height * index as f32, width - gap * 2.0, row_height))
            .collect();
        MenuLayout { frame, rows }
    }

    /// The row under a window position.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        self.rows.iter().position(|row| row.contains(x, y))
    }
}

pub fn draw(r: &mut Renderer, menu: &Menu, window: (f32, f32), scale: f32, palette: &Palette) {
    let metrics = r.metrics();
    let cell = (metrics.width, metrics.height);
    let layout = MenuLayout::new(menu, window, cell, scale);
    let line = scale.round().max(1.0);
    let pad = (10.0 * scale).round();
    let fg = color(palette.foreground);
    let dim = color(palette.chrome_text_dim);

    // A soft shadow, then the panel with a hairline border.
    let frame = layout.frame;
    let [red, green, blue, _] = color(palette.background);
    let shadow = (6.0 * scale).round();
    r.rect(frame.x - shadow / 2.0, frame.y + shadow / 2.0, frame.w + shadow, frame.h + shadow / 2.0, [red / 2, green / 2, blue / 2, 60]);
    let outer = frame.inflate(line);
    r.rect(outer.x, outer.y, outer.w, outer.h, color(palette.border));
    r.rect(frame.x, frame.y, frame.w, frame.h, color(palette.chrome));

    let icon_size = cell.1.round();
    for (index, (row, item)) in layout.rows.iter().zip(&menu.items).enumerate() {
        if index == menu.selected {
            r.rect(row.x, row.y, row.w, row.h, color(palette.selection));
        }
        let text_y = row.y + ((row.h - cell.1) / 2.0).round();
        let mut x = row.x + pad;
        if item.checked {
            r.text("✓", false, x, text_y, cell.0 * 2.0, color(palette.accent));
        }
        if menu.items.iter().any(|item| item.checked) {
            x += cell.0 * 2.0;
        }
        if let Some(icon) = item.icon {
            r.image(icon, x, text_y, icon_size, icon_size, [0.0, 0.0, 1.0, 1.0]);
            x += icon_size + cell.0;
        }
        let detail_width = r.text_width(&item.detail);
        let room = row.x + row.w - pad - detail_width - x;
        r.text(&item.label, false, x, text_y, room.max(0.0), fg);
        if !item.detail.is_empty() {
            r.text(&item.detail, false, row.x + row.w - pad - detail_width, text_y, detail_width, dim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: (f32, f32) = (1000.0, 700.0);
    const CELL: (f32, f32) = (8.0, 16.0);

    #[test]
    fn opens_below_its_control_and_rows_are_clickable() {
        let anchor = Rect::new(12.0, 60.0, 236.0, 28.0);
        let menu = Menu::new_session(anchor);
        let layout = MenuLayout::new(&menu, WINDOW, CELL, 1.0);
        assert!(layout.frame.y >= anchor.y + anchor.h);
        assert_eq!(layout.frame.x, anchor.x);
        assert!(layout.frame.w >= anchor.w);
        let (x, y) = layout.rows[NEW_SESSION_TERMINAL].center();
        assert_eq!(layout.row_at(x, y), Some(NEW_SESSION_TERMINAL));
        assert_eq!(layout.row_at(0.0, 0.0), None);
    }

    #[test]
    fn flips_above_when_there_is_no_room_below() {
        let anchor = Rect::new(900.0, 660.0, 80.0, 28.0);
        let all: Vec<(AgentKind, String)> = AgentKind::ALL.iter().map(|kind| (*kind, String::new())).collect();
        let menu = Menu::agents(anchor, AgentKind::Codex, &all);
        let layout = MenuLayout::new(&menu, WINDOW, CELL, 1.0);
        assert!(layout.frame.y + layout.frame.h <= anchor.y);
        // And stays inside the window horizontally.
        assert!(layout.frame.x + layout.frame.w <= WINDOW.0);
        // The current agent starts highlighted and checked.
        assert_eq!(menu.selected, 1);
        assert!(menu.items[1].checked && !menu.items[0].checked);
    }

    #[test]
    fn the_agent_menu_lists_only_the_agents_given() {
        let installed = [(AgentKind::Claude, "~/.local/bin/claude".to_owned()), (AgentKind::Kiro, "alias".to_owned())];
        let menu = Menu::agents(Rect::default(), AgentKind::Kiro, &installed);
        let agents: Vec<Option<AgentKind>> = menu.items.iter().map(|item| item.agent).collect();
        assert_eq!(agents, [Some(AgentKind::Claude), Some(AgentKind::Kiro)]);
        assert_eq!(menu.items[0].detail, "~/.local/bin/claude");
        assert_eq!(menu.selected, 1);
        // An agent that is not listed is not selected.
        assert_eq!(Menu::agents(Rect::default(), AgentKind::Codex, &installed).selected, 0);
    }

    #[test]
    fn command_menus_show_each_commands_name_and_shortcut() {
        use crate::actions::Action;
        let menu = Menu::commands(Rect::default(), &[(Action::SplitRight, None), (Action::RenameTab, Some("Rename…"))]);
        assert_eq!(menu.kind, MenuKind::Commands);
        assert_eq!((menu.items[0].label.as_str(), menu.items[0].detail.as_str()), ("Split Right", "⌘D"));
        assert_eq!((menu.items[1].label.as_str(), menu.items[1].detail.as_str()), ("Rename…", ""));
        assert_eq!(menu.items[1].action, Some(Action::RenameTab));
    }

    #[test]
    fn arrow_keys_wrap_around() {
        let mut menu = Menu::new_session(Rect::default());
        menu.step(-1);
        assert_eq!(menu.selected, NEW_SESSION_TERMINAL);
        menu.step(1);
        assert_eq!(menu.selected, NEW_SESSION_AGENT);
    }
}
