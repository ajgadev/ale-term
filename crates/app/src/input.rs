//! Turning key presses and mouse actions into the bytes a terminal program
//! expects.
//!
//! Everything here is a pure function of its arguments, so the tables can be
//! unit-tested without a window.

use ale_core::vt::term::TermMode;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Encodes a key press, or returns `None` when the key sends nothing.
///
/// * `text` is what the key would type with the current modifiers.
/// * `base` is the character on the key ignoring modifiers (so Ctrl+Shift+C
///   still knows it is the `c` key).
/// * `option_as_meta` makes Option behave like the Meta key (ESC prefix).
pub fn encode_key(
    key: &Key,
    text: Option<&str>,
    base: Option<char>,
    mods: ModifiersState,
    mode: TermMode,
    option_as_meta: bool,
) -> Option<Vec<u8>> {
    let shift = mods.shift_key();
    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let cmd = mods.super_key();

    // xterm's modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
    let modifier = 1 + shift as u8 + (alt as u8) * 2 + (ctrl as u8) * 4;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    // Cursor-style keys: `ESC [ A`, or `ESC O A` in application mode, or
    // `ESC [ 1 ; m A` with modifiers.
    let cursor_key = |letter: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    // Editing and function keys: `ESC [ n ~`, or `ESC [ n ; m ~`.
    let tilde_key = |number: u8| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[{number};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{number}~").into_bytes()
        }
    };

    if let Key::Named(named) = key {
        let bytes: Vec<u8> = match named {
            // Shift+Enter and Option+Enter insert a newline in agent prompts
            // (they read ESC + Enter as "new line, don't submit").
            NamedKey::Enter if shift || alt => b"\x1b\r".to_vec(),
            NamedKey::Enter => b"\r".to_vec(),
            NamedKey::Tab if shift => b"\x1b[Z".to_vec(),
            NamedKey::Tab => b"\t".to_vec(),
            // macOS editing habits: Cmd deletes the line, Option the word.
            NamedKey::Backspace if cmd => b"\x15".to_vec(),
            NamedKey::Backspace if alt => b"\x17".to_vec(),
            NamedKey::Backspace if ctrl => b"\x08".to_vec(),
            NamedKey::Backspace => b"\x7f".to_vec(),
            NamedKey::Escape => b"\x1b".to_vec(),
            NamedKey::Space if ctrl => b"\0".to_vec(),
            NamedKey::Space if alt && option_as_meta => b"\x1b ".to_vec(),
            NamedKey::Space => b" ".to_vec(),

            // Cmd+arrows jump to the line edges, Option+arrows move by word.
            NamedKey::ArrowLeft if cmd => b"\x01".to_vec(),
            NamedKey::ArrowRight if cmd => b"\x05".to_vec(),
            NamedKey::ArrowUp | NamedKey::ArrowDown if cmd => return None,
            NamedKey::ArrowLeft if alt && !shift && !ctrl => b"\x1bb".to_vec(),
            NamedKey::ArrowRight if alt && !shift && !ctrl => b"\x1bf".to_vec(),
            NamedKey::ArrowUp => cursor_key('A'),
            NamedKey::ArrowDown => cursor_key('B'),
            NamedKey::ArrowRight => cursor_key('C'),
            NamedKey::ArrowLeft => cursor_key('D'),
            NamedKey::Home => cursor_key('H'),
            NamedKey::End => cursor_key('F'),

            NamedKey::Insert => tilde_key(2),
            NamedKey::Delete => tilde_key(3),
            NamedKey::PageUp => tilde_key(5),
            NamedKey::PageDown => tilde_key(6),

            NamedKey::F1 => cursor_key_fn('P', modifier),
            NamedKey::F2 => cursor_key_fn('Q', modifier),
            NamedKey::F3 => cursor_key_fn('R', modifier),
            NamedKey::F4 => cursor_key_fn('S', modifier),
            NamedKey::F5 => tilde_key(15),
            NamedKey::F6 => tilde_key(17),
            NamedKey::F7 => tilde_key(18),
            NamedKey::F8 => tilde_key(19),
            NamedKey::F9 => tilde_key(20),
            NamedKey::F10 => tilde_key(21),
            NamedKey::F11 => tilde_key(23),
            NamedKey::F12 => tilde_key(24),
            _ => return None,
        };
        return Some(bytes);
    }

    // Anything else with Cmd held is an app shortcut, never terminal input.
    if cmd {
        return None;
    }

    if ctrl {
        let code = control_code(base?)?;
        return Some(if alt { vec![0x1b, code] } else { vec![code] });
    }

    let text = text.filter(|t| !t.is_empty())?;
    if alt && option_as_meta {
        let mut bytes = vec![0x1b];
        bytes.extend_from_slice(text.as_bytes());
        return Some(bytes);
    }
    Some(text.as_bytes().to_vec())
}

/// F1-F4 use `ESC O P`..`S`, or `ESC [ 1 ; m P` with modifiers.
fn cursor_key_fn(letter: char, modifier: u8) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[1;{modifier}{letter}").into_bytes()
    } else {
        format!("\x1bO{letter}").into_bytes()
    }
}

/// The control character produced by Ctrl + `c`, e.g. Ctrl+C is 0x03.
fn control_code(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' | ' ' => 0,
        '[' | '3' => 27,
        '\\' | '4' => 28,
        ']' | '5' => 29,
        '^' | '6' => 30,
        '_' | '-' | '7' | '/' => 31,
        '8' | '?' => 127,
        _ => return None,
    })
}

/// Wraps pasted text for the terminal.
///
/// In bracketed-paste mode the text is surrounded by markers so the program
/// can tell it apart from typing (and not run each line as a command).
/// Escape characters are removed so pasted text cannot end the paste early
/// and smuggle in commands.
pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let clean: String = text.chars().filter(|c| *c != '\x1b' && *c != '\x03').collect();
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        // Terminals send carriage return for Enter, so do the same for
        // pasted line breaks.
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// What the mouse did, for [`encode_mouse`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    /// Movement, with a button held or not.
    Motion,
}

/// Mouse buttons as terminals number them.
pub mod button {
    pub const LEFT: u8 = 0;
    pub const MIDDLE: u8 = 1;
    pub const RIGHT: u8 = 2;
    /// Used for motion with no button held.
    pub const NONE: u8 = 3;
    pub const WHEEL_UP: u8 = 64;
    pub const WHEEL_DOWN: u8 = 65;
}

/// Encodes a mouse event for programs that asked for mouse reporting
/// (vim, htop, ...). `column` and `line` are zero-based cell coordinates.
pub fn encode_mouse(
    action: MouseAction,
    button: u8,
    column: usize,
    line: usize,
    mods: ModifiersState,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let mut code = button;
    if mods.shift_key() {
        code += 4;
    }
    if mods.alt_key() {
        code += 8;
    }
    if mods.control_key() {
        code += 16;
    }
    if action == MouseAction::Motion {
        code += 32;
    }

    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if action == MouseAction::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{end}", column + 1, line + 1).into_bytes());
    }

    // Legacy encoding: one byte per value, which cannot express positions
    // past column/line 223, and reports every release as "button 3".
    if column >= 223 || line >= 223 {
        return None;
    }
    if action == MouseAction::Release {
        code = (code & !0b11) | 3;
    }
    Some(vec![0x1b, b'[', b'M', 32 + code, 32 + 1 + column as u8, 32 + 1 + line as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: ModifiersState = ModifiersState::empty();

    fn named(key: NamedKey, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
        encode_key(&Key::Named(key), None, None, mods, mode, false)
    }

    fn typed(text: &str, base: char, mods: ModifiersState, meta: bool) -> Option<Vec<u8>> {
        encode_key(&Key::Character(text.into()), Some(text), Some(base), mods, TermMode::NONE, meta)
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(typed("a", 'a', NONE, false), Some(b"a".to_vec()));
        assert_eq!(typed("ñ", 'ñ', NONE, false), Some("ñ".as_bytes().to_vec()));
        assert_eq!(typed("A", 'a', ModifiersState::SHIFT, false), Some(b"A".to_vec()));
    }

    #[test]
    fn control_combinations() {
        assert_eq!(typed("c", 'c', ModifiersState::CONTROL, false), Some(vec![3]));
        assert_eq!(typed("[", '[', ModifiersState::CONTROL, false), Some(vec![27]));
        assert_eq!(named(NamedKey::Space, ModifiersState::CONTROL, TermMode::NONE), Some(vec![0]));
        assert_eq!(typed("1", '1', ModifiersState::CONTROL, false), None);
    }

    #[test]
    fn option_composes_by_default_and_is_meta_when_enabled() {
        // On a Spanish layout Option+2 types "@"; it must arrive as "@".
        assert_eq!(typed("@", '2', ModifiersState::ALT, false), Some(b"@".to_vec()));
        assert_eq!(typed("b", 'b', ModifiersState::ALT, true), Some(b"\x1bb".to_vec()));
    }

    #[test]
    fn command_shortcuts_never_reach_the_shell() {
        assert_eq!(typed("t", 't', ModifiersState::SUPER, false), None);
    }

    #[test]
    fn arrows_follow_cursor_mode_and_modifiers() {
        assert_eq!(named(NamedKey::ArrowUp, NONE, TermMode::NONE), Some(b"\x1b[A".to_vec()));
        assert_eq!(named(NamedKey::ArrowUp, NONE, TermMode::APP_CURSOR), Some(b"\x1bOA".to_vec()));
        assert_eq!(
            named(NamedKey::ArrowRight, ModifiersState::SHIFT, TermMode::NONE),
            Some(b"\x1b[1;2C".to_vec())
        );
        assert_eq!(
            named(NamedKey::ArrowLeft, ModifiersState::CONTROL, TermMode::APP_CURSOR),
            Some(b"\x1b[1;5D".to_vec())
        );
    }

    #[test]
    fn mac_editing_shortcuts() {
        assert_eq!(named(NamedKey::ArrowLeft, ModifiersState::SUPER, TermMode::NONE), Some(vec![1]));
        assert_eq!(named(NamedKey::ArrowRight, ModifiersState::SUPER, TermMode::NONE), Some(vec![5]));
        assert_eq!(named(NamedKey::ArrowLeft, ModifiersState::ALT, TermMode::NONE), Some(b"\x1bb".to_vec()));
        assert_eq!(named(NamedKey::Backspace, ModifiersState::SUPER, TermMode::NONE), Some(vec![0x15]));
        assert_eq!(named(NamedKey::Backspace, ModifiersState::ALT, TermMode::NONE), Some(vec![0x17]));
        assert_eq!(named(NamedKey::Backspace, NONE, TermMode::NONE), Some(vec![0x7f]));
    }

    #[test]
    fn enter_variants() {
        assert_eq!(named(NamedKey::Enter, NONE, TermMode::NONE), Some(b"\r".to_vec()));
        assert_eq!(named(NamedKey::Enter, ModifiersState::SHIFT, TermMode::NONE), Some(b"\x1b\r".to_vec()));
    }

    #[test]
    fn function_and_editing_keys() {
        assert_eq!(named(NamedKey::F1, NONE, TermMode::NONE), Some(b"\x1bOP".to_vec()));
        assert_eq!(named(NamedKey::F5, NONE, TermMode::NONE), Some(b"\x1b[15~".to_vec()));
        assert_eq!(named(NamedKey::Delete, NONE, TermMode::NONE), Some(b"\x1b[3~".to_vec()));
        assert_eq!(named(NamedKey::PageUp, ModifiersState::CONTROL, TermMode::NONE), Some(b"\x1b[5;5~".to_vec()));
    }

    #[test]
    fn paste_is_bracketed_and_sanitized() {
        assert_eq!(encode_paste("a\nb", TermMode::NONE), b"a\rb".to_vec());
        assert_eq!(
            encode_paste("ls\x1b[201~rm", TermMode::BRACKETED_PASTE),
            b"\x1b[200~ls[201~rm\x1b[201~".to_vec()
        );
    }

    #[test]
    fn mouse_reports() {
        let sgr = TermMode::SGR_MOUSE;
        assert_eq!(
            encode_mouse(MouseAction::Press, button::LEFT, 4, 9, NONE, sgr),
            Some(b"\x1b[<0;5;10M".to_vec())
        );
        assert_eq!(
            encode_mouse(MouseAction::Release, button::LEFT, 4, 9, NONE, sgr),
            Some(b"\x1b[<0;5;10m".to_vec())
        );
        assert_eq!(
            encode_mouse(MouseAction::Motion, button::LEFT, 0, 0, ModifiersState::CONTROL, sgr),
            Some(b"\x1b[<48;1;1M".to_vec())
        );
        assert_eq!(
            encode_mouse(MouseAction::Press, button::WHEEL_UP, 0, 0, NONE, sgr),
            Some(b"\x1b[<64;1;1M".to_vec())
        );
        // Legacy encoding.
        assert_eq!(
            encode_mouse(MouseAction::Press, button::LEFT, 0, 0, NONE, TermMode::NONE),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        assert_eq!(
            encode_mouse(MouseAction::Release, button::LEFT, 0, 0, NONE, TermMode::NONE),
            Some(vec![0x1b, b'[', b'M', 35, 33, 33])
        );
        assert_eq!(encode_mouse(MouseAction::Press, button::LEFT, 300, 0, NONE, TermMode::NONE), None);
    }
}
