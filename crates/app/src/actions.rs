//! Everything the user can ask the app to do, in one list.
//!
//! Keyboard shortcuts and the command palette both go through [`Action`], so
//! a command added here shows up in the palette automatically.

use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

use crate::layout::Direction;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    NewTab,
    ClosePane,
    CloseTab,
    RenameTab,
    SwitchWorkspace,
    NextWorkspace,
    PreviousWorkspace,
    NewWorkspace,
    RenameWorkspace,
    MoveTab,
    SetUpWorkspace,
    ToggleSidebar,
    ToggleBoard,
    /// Between the sidebar and board layouts.
    SwitchLayout,
    ToggleGraph,
    NewAgentSession,
    ShowArchive,
    ClearArchive,
    /// The screen of usage statistics.
    ShowUsage,
    NextAttention,
    SetUpHooks,
    RemoveHooks,
    SetUpKiroHooks,
    RemoveKiroHooks,
    SplitRight,
    SplitDown,
    NextTab,
    PreviousTab,
    /// Zero-based; 8 means "last tab".
    SelectTab(u8),
    NextPane,
    PreviousPane,
    FocusPane(Direction),
    Copy,
    Paste,
    Find,
    FindNext,
    FindPrevious,
    PreviousPrompt,
    NextPrompt,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToTop,
    ScrollToBottom,
    ClearHistory,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleFullscreen,
    CommandPalette,
    Settings,
    OpenConfig,
    Quit,
}

/// The commands listed in the palette: action, name, shortcut shown.
pub const COMMANDS: &[(Action, &str, &str)] = &[
    (Action::NewTab, "New Tab", "⌘T"),
    (Action::ClosePane, "Close Pane", "⌘W"),
    (Action::CloseTab, "Close Tab", "⇧⌘W"),
    (Action::RenameTab, "Rename Tab", ""),
    (Action::SwitchWorkspace, "Switch Workspace", "⌘P"),
    (Action::NextWorkspace, "Next Workspace", "⌃⌘→"),
    (Action::PreviousWorkspace, "Previous Workspace", "⌃⌘←"),
    (Action::NewWorkspace, "New Workspace", ""),
    (Action::RenameWorkspace, "Rename Workspace", ""),
    (Action::MoveTab, "Move Tab to Workspace", ""),
    (Action::SetUpWorkspace, "Set Up Workspace Tabs from Template", ""),
    (Action::ToggleSidebar, "Toggle Sidebar or Docked Board", "⌘B"),
    (Action::SwitchLayout, "Switch Layout: Sidebar / Board", "⌥⌘B"),
    (Action::ToggleBoard, "Toggle Agent Board", "⇧⌘B"),
    (Action::ToggleGraph, "Toggle Agent Graph", "⌥⌘G"),
    (Action::NewAgentSession, "New Agent Session", "⇧⌘T"),
    (Action::ShowArchive, "Resume Archived Session", ""),
    (Action::ClearArchive, "Clear Session Archive", ""),
    (Action::ShowUsage, "Agent Usage", "⌥⌘U"),
    (Action::NextAttention, "Go to Next Agent That Needs You", "⇧⌘A"),
    (Action::SetUpHooks, "Set Up Claude Code Status Hooks", ""),
    (Action::RemoveHooks, "Remove Claude Code Status Hooks", ""),
    (Action::SetUpKiroHooks, "Set Up Kiro Status Hooks", ""),
    (Action::RemoveKiroHooks, "Remove Kiro Status Hooks", ""),
    (Action::SplitRight, "Split Right", "⌘D"),
    (Action::SplitDown, "Split Down", "⇧⌘D"),
    (Action::NextTab, "Next Tab", "⌃Tab"),
    (Action::PreviousTab, "Previous Tab", "⌃⇧Tab"),
    (Action::NextPane, "Next Pane", "⌘]"),
    (Action::PreviousPane, "Previous Pane", "⌘["),
    (Action::FocusPane(Direction::Left), "Focus Pane Left", "⌥⌘←"),
    (Action::FocusPane(Direction::Right), "Focus Pane Right", "⌥⌘→"),
    (Action::FocusPane(Direction::Up), "Focus Pane Above", "⌥⌘↑"),
    (Action::FocusPane(Direction::Down), "Focus Pane Below", "⌥⌘↓"),
    (Action::Copy, "Copy", "⌘C"),
    (Action::Paste, "Paste", "⌘V"),
    (Action::Find, "Find in Scrollback", "⌘F"),
    (Action::FindNext, "Find Next", "⌘G"),
    (Action::FindPrevious, "Find Previous", "⇧⌘G"),
    (Action::PreviousPrompt, "Jump to Previous Prompt", "⌘↑"),
    (Action::NextPrompt, "Jump to Next Prompt", "⌘↓"),
    (Action::ScrollPageUp, "Scroll Page Up", "⇧PgUp"),
    (Action::ScrollPageDown, "Scroll Page Down", "⇧PgDn"),
    (Action::ScrollToTop, "Scroll to Top", "⌘Home"),
    (Action::ScrollToBottom, "Scroll to Bottom", "⌘End"),
    (Action::ClearHistory, "Clear Scrollback", "⌘K"),
    (Action::ZoomIn, "Increase Font Size", "⌘+"),
    (Action::ZoomOut, "Decrease Font Size", "⌘-"),
    (Action::ZoomReset, "Reset Font Size", "⌘0"),
    (Action::ToggleFullscreen, "Toggle Full Screen", "⌃⌘F"),
    (Action::CommandPalette, "Command Palette", "⇧⌘P"),
    (Action::Settings, "Settings", "⌘,"),
    (Action::OpenConfig, "Open Settings File", ""),
    (Action::Quit, "Quit ale-term", "⌘Q"),
];

/// Maps a key press to an action.
///
/// `key` is the key ignoring modifiers (so Shift+Cmd+D still reports `d`);
/// `physical` is its position on the keyboard, used for the bracket keys so
/// they work on every layout.
pub fn from_key(key: &Key, physical: PhysicalKey, mods: ModifiersState) -> Option<Action> {
    let (cmd, shift, alt, ctrl) =
        (mods.super_key(), mods.shift_key(), mods.alt_key(), mods.control_key());

    if let Key::Named(named) = key {
        return Some(match named {
            NamedKey::Tab if ctrl && !cmd => {
                if shift { Action::PreviousTab } else { Action::NextTab }
            }
            NamedKey::PageUp if shift && !cmd && !ctrl && !alt => Action::ScrollPageUp,
            NamedKey::PageDown if shift && !cmd && !ctrl && !alt => Action::ScrollPageDown,
            NamedKey::ArrowLeft if cmd && ctrl => Action::PreviousWorkspace,
            NamedKey::ArrowRight if cmd && ctrl => Action::NextWorkspace,
            NamedKey::ArrowLeft if cmd && alt => Action::FocusPane(Direction::Left),
            NamedKey::ArrowRight if cmd && alt => Action::FocusPane(Direction::Right),
            NamedKey::ArrowUp if cmd && alt => Action::FocusPane(Direction::Up),
            NamedKey::ArrowDown if cmd && alt => Action::FocusPane(Direction::Down),
            NamedKey::ArrowUp if cmd => Action::PreviousPrompt,
            NamedKey::ArrowDown if cmd => Action::NextPrompt,
            NamedKey::Home if cmd => Action::ScrollToTop,
            NamedKey::End if cmd => Action::ScrollToBottom,
            _ => return None,
        });
    }
    if !cmd {
        return None;
    }
    match physical {
        PhysicalKey::Code(KeyCode::BracketLeft) => {
            return Some(if shift { Action::PreviousTab } else { Action::PreviousPane });
        }
        PhysicalKey::Code(KeyCode::BracketRight) => {
            return Some(if shift { Action::NextTab } else { Action::NextPane });
        }
        _ => {}
    }
    let Key::Character(text) = key else { return None };
    Some(match text.as_str() {
        "t" if shift => Action::NewAgentSession,
        "t" => Action::NewTab,
        "b" if alt => Action::SwitchLayout,
        "b" if shift => Action::ToggleBoard,
        "b" => Action::ToggleSidebar,
        "a" if shift => Action::NextAttention,
        "w" if shift => Action::CloseTab,
        "w" => Action::ClosePane,
        "d" if shift => Action::SplitDown,
        "d" => Action::SplitRight,
        "c" => Action::Copy,
        "v" => Action::Paste,
        "f" if ctrl => Action::ToggleFullscreen,
        "f" => Action::Find,
        "g" if alt => Action::ToggleGraph,
        "u" if alt => Action::ShowUsage,
        "g" if shift => Action::FindPrevious,
        "g" => Action::FindNext,
        "p" if shift => Action::CommandPalette,
        "p" => Action::SwitchWorkspace,
        "k" => Action::ClearHistory,
        "q" => Action::Quit,
        "=" | "+" => Action::ZoomIn,
        "-" => Action::ZoomOut,
        "0" => Action::ZoomReset,
        "," => Action::Settings,
        digit if digit.len() == 1 && ("1"..="9").contains(&digit) => {
            Action::SelectTab(digit.as_bytes()[0] - b'1')
        }
        _ => return None,
    })
}

/// Scores how well `query` matches `text` as a fuzzy (in-order, not
/// necessarily adjacent) match, ignoring case. Higher is better; `None`
/// means no match. Matches at word starts and runs of adjacent letters
/// score more, so "st" ranks "Split Right"... below "Scroll to Top".
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0;
    let mut position = 0;
    let mut previous_match: Option<usize> = None;
    for wanted in query.chars().flat_map(char::to_lowercase).filter(|c| !c.is_whitespace()) {
        let found = text[position..].iter().position(|c| *c == wanted)? + position;
        score += 1;
        if found == 0 || text[found - 1] == ' ' {
            score += 8;
        }
        if previous_match.is_some_and(|previous| previous + 1 == found) {
            score += 5;
        }
        // Prefer matches near the start.
        score -= (found - position).min(4) as i32;
        previous_match = Some(found);
        position = found + 1;
    }
    Some(score)
}

/// The commands matching `query`, best first. An empty query lists all.
pub fn search(query: &str) -> Vec<&'static (Action, &'static str, &'static str)> {
    let mut matches: Vec<_> = COMMANDS
        .iter()
        .filter_map(|command| fuzzy_score(query, command.1).map(|score| (score, command)))
        .collect();
    // Stable sort keeps the list order for equal scores.
    matches.sort_by_key(|(score, _)| -score);
    matches.into_iter().map(|(_, command)| command).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_listed_in_the_readme_with_its_shortcut() {
        let readme = include_str!("../../../README.md");
        let table = readme
            .split("<!-- commands:start -->")
            .nth(1)
            .and_then(|rest| rest.split("<!-- commands:end -->").next())
            .expect("README.md has a commands section");
        for (_, name, keys) in COMMANDS {
            let shortcut = if keys.is_empty() { String::new() } else { format!("`{keys}`") };
            let row = format!("| {name} | {shortcut} |");
            assert!(
                table.lines().any(|line| line == row),
                "README.md does not list {name:?} with {keys:?}: run python3 scripts/readme-commands.py"
            );
        }
    }

    fn key(text: &str, mods: ModifiersState) -> Option<Action> {
        from_key(&Key::Character(text.into()), PhysicalKey::Code(KeyCode::KeyA), mods)
    }

    #[test]
    fn command_shortcuts() {
        let cmd = ModifiersState::SUPER;
        assert_eq!(key("t", cmd), Some(Action::NewTab));
        assert_eq!(key("d", cmd), Some(Action::SplitRight));
        assert_eq!(key("d", cmd | ModifiersState::SHIFT), Some(Action::SplitDown));
        assert_eq!(key("p", cmd | ModifiersState::SHIFT), Some(Action::CommandPalette));
        assert_eq!(key("f", cmd), Some(Action::Find));
        assert_eq!(key("p", cmd), Some(Action::SwitchWorkspace));
        assert_eq!(key("w", cmd | ModifiersState::SHIFT), Some(Action::CloseTab));
        assert_eq!(key("f", cmd | ModifiersState::CONTROL), Some(Action::ToggleFullscreen));
        assert_eq!(key("u", cmd | ModifiersState::ALT), Some(Action::ShowUsage));
        assert_eq!(key("1", cmd), Some(Action::SelectTab(0)));
        assert_eq!(key("9", cmd), Some(Action::SelectTab(8)));
        // Without Cmd, letters are ordinary typing.
        assert_eq!(key("t", ModifiersState::empty()), None);
        assert_eq!(key("x", cmd), None);
    }

    #[test]
    fn named_key_shortcuts() {
        let named = |key, mods| from_key(&Key::Named(key), PhysicalKey::Code(KeyCode::KeyA), mods);
        assert_eq!(named(NamedKey::Tab, ModifiersState::CONTROL), Some(Action::NextTab));
        assert_eq!(named(NamedKey::ArrowUp, ModifiersState::SUPER), Some(Action::PreviousPrompt));
        assert_eq!(
            named(NamedKey::ArrowUp, ModifiersState::SUPER | ModifiersState::ALT),
            Some(Action::FocusPane(Direction::Up))
        );
        assert_eq!(named(NamedKey::ArrowUp, ModifiersState::empty()), None);
        assert_eq!(named(NamedKey::PageUp, ModifiersState::SHIFT), Some(Action::ScrollPageUp));
    }

    #[test]
    fn brackets_are_matched_by_position() {
        let bracket = |mods| from_key(&Key::Character("´".into()), PhysicalKey::Code(KeyCode::BracketLeft), mods);
        assert_eq!(bracket(ModifiersState::SUPER), Some(Action::PreviousPane));
        assert_eq!(bracket(ModifiersState::SUPER | ModifiersState::SHIFT), Some(Action::PreviousTab));
    }

    #[test]
    fn every_command_has_a_unique_name() {
        let mut names: Vec<_> = COMMANDS.iter().map(|c| c.1).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), COMMANDS.len());
    }

    #[test]
    fn fuzzy_search_ranks_sensibly() {
        assert_eq!(search("").len(), COMMANDS.len());
        assert_eq!(search("new tab")[0].0, Action::NewTab);
        assert_eq!(search("nt")[0].0, Action::NewTab);
        assert_eq!(search("split d")[0].0, Action::SplitDown);
        assert_eq!(search("switch")[0].0, Action::SwitchWorkspace);
        assert_eq!(search("quit")[0].0, Action::Quit);
        assert_eq!(search("font")[0].0, Action::ZoomIn);
        assert!(search("zzzz").is_empty());
        assert_eq!(fuzzy_score("TAB", "New Tab"), fuzzy_score("tab", "New Tab"));
    }
}
