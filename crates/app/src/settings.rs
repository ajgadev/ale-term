//! The settings screen: a list of the options people change most, each
//! adjustable from the keyboard or with a click.
//!
//! It is a view onto the settings file, not a separate store. Changing a
//! value here rewrites that one line of the file (see
//! `ale_config::set_value`); everything else in the file is kept, and the
//! file remains the place for the rest (colors, workspace templates).

use ale_config::{Config, Palette, RendererChoice, SettingValue, ThemeMode};
use ale_render::Renderer;

use crate::draw::color;
use crate::layout::Rect;

pub enum Kind {
    Toggle,
    Number { step: f64, min: f64, max: f64, integer: bool },
    /// One of a fixed set of words.
    Choice(&'static [&'static str]),
    Text,
}

pub struct Setting {
    /// Table and key in the settings file.
    pub section: &'static str,
    pub key: &'static str,
    /// Heading it is listed under.
    pub group: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    /// One line shown while the setting is selected.
    pub help: &'static str,
}

const fn number(step: f64, min: f64, max: f64) -> Kind {
    Kind::Number { step, min, max, integer: false }
}

const fn whole(step: f64, min: f64, max: f64) -> Kind {
    Kind::Number { step, min, max, integer: true }
}

pub const SETTINGS: &[Setting] = &[
    Setting { section: "theme", key: "mode", group: "Appearance", label: "Theme", kind: Kind::Choice(&["auto", "dark", "light"]), help: "\"auto\" follows the macOS appearance." },
    Setting { section: "font", key: "family", group: "Appearance", label: "Font", kind: Kind::Text, help: "Font family name, as shown in Font Book." },
    Setting { section: "font", key: "size", group: "Appearance", label: "Font size", kind: number(0.5, 6.0, 72.0), help: "In points." },
    Setting { section: "font", key: "line_height", group: "Appearance", label: "Line height", kind: number(0.05, 0.8, 3.0), help: "As a multiple of the font size." },
    Setting { section: "font", key: "ligatures", group: "Appearance", label: "Ligatures", kind: Kind::Toggle, help: "Join pairs such as -> and != when the font supports it." },
    Setting { section: "window", key: "padding", group: "Appearance", label: "Padding", kind: number(1.0, 0.0, 64.0), help: "Space between a pane's edge and its text, in points." },
    Setting { section: "terminal", key: "scrollback", group: "Terminal", label: "Scrollback lines", kind: whole(1000.0, 0.0, 1_000_000.0), help: "Lines of history kept per pane. Applies to new panes." },
    Setting { section: "terminal", key: "scroll_multiplier", group: "Terminal", label: "Scroll speed", kind: number(0.5, 0.5, 20.0), help: "Lines scrolled per mouse-wheel notch." },
    Setting { section: "terminal", key: "option_as_meta", group: "Terminal", label: "Option as Meta", kind: Kind::Toggle, help: "Send ESC + key instead of composing characters with Option." },
    Setting { section: "terminal", key: "shell_integration", group: "Terminal", label: "Shell integration", kind: Kind::Toggle, help: "Let zsh report its folder, prompts and commands. Applies to new panes." },
    Setting { section: "terminal", key: "image_memory_mb", group: "Terminal", label: "Image memory (MB)", kind: whole(32.0, 0.0, 4096.0), help: "Budget for inline images; the oldest are dropped beyond it." },
    Setting { section: "agents", key: "sidebar", group: "Agents", label: "Sidebar at launch", kind: Kind::Toggle, help: "Show the sidebar when the app starts. Cmd+B toggles it any time." },
    Setting { section: "agents", key: "sidebar_width", group: "Agents", label: "Sidebar width", kind: number(10.0, 160.0, 600.0), help: "In points." },
    Setting { section: "agents", key: "sound", group: "Agents", label: "Sound", kind: Kind::Toggle, help: "Play a sound when an agent needs you or finishes." },
    Setting { section: "agents", key: "notifications", group: "Agents", label: "Notifications", kind: Kind::Toggle, help: "Notify when that happens and the window is not in front." },
    Setting { section: "agents", key: "new_session_command", group: "Agents", label: "New session runs", kind: Kind::Text, help: "The command for the New session button (Shift+Cmd+T)." },
    Setting { section: "window", key: "restore", group: "Window", label: "Restore at launch", kind: Kind::Toggle, help: "Reopen the previous workspaces, tabs and splits." },
    Setting { section: "window", key: "renderer", group: "Window", label: "Renderer", kind: Kind::Choice(&["cpu", "gpu"]), help: "\"cpu\" uses the least memory. Takes effect at the next launch." },
];

/// The value a setting currently has.
pub fn current(config: &Config, setting: &Setting) -> SettingValue {
    use SettingValue::{Bool, Integer, Text};
    // Settings are stored as 32-bit numbers; round away the noise that
    // widening them adds (1.3 would otherwise read back as 1.2999999523).
    #[allow(non_snake_case)]
    let Float = |value: f64| SettingValue::Float((value * 1000.0).round() / 1000.0);
    match (setting.section, setting.key) {
        ("theme", "mode") => Text(
            match config.theme.mode {
                ThemeMode::Auto => "auto",
                ThemeMode::Dark => "dark",
                ThemeMode::Light => "light",
            }
            .into(),
        ),
        ("font", "family") => Text(config.font.family.clone()),
        ("font", "size") => Float(config.font.size as f64),
        ("font", "line_height") => Float(config.font.line_height as f64),
        ("font", "ligatures") => Bool(config.font.ligatures),
        ("window", "padding") => Float(config.window.padding as f64),
        ("window", "restore") => Bool(config.window.restore),
        ("window", "renderer") => Text(
            match config.window.renderer {
                RendererChoice::Cpu => "cpu",
                RendererChoice::Gpu => "gpu",
            }
            .into(),
        ),
        ("terminal", "scrollback") => Integer(config.terminal.scrollback as i64),
        ("terminal", "scroll_multiplier") => Float(config.terminal.scroll_multiplier as f64),
        ("terminal", "option_as_meta") => Bool(config.terminal.option_as_meta),
        ("terminal", "shell_integration") => Bool(config.terminal.shell_integration),
        ("terminal", "image_memory_mb") => Integer(config.terminal.image_memory_mb as i64),
        ("agents", "sidebar") => Bool(config.agents.sidebar),
        ("agents", "sidebar_width") => Float(config.agents.sidebar_width as f64),
        ("agents", "sound") => Bool(config.agents.sound),
        ("agents", "notifications") => Bool(config.agents.notifications),
        ("agents", "new_session_command") => Text(config.agents.new_session_command.clone()),
        _ => Text(String::new()),
    }
}

/// The value after one step up (`direction` = 1) or down (-1). Toggles
/// flip and choices cycle either way. `None` for text, which is typed.
pub fn stepped(setting: &Setting, value: &SettingValue, direction: i32) -> Option<SettingValue> {
    match (&setting.kind, value) {
        (Kind::Toggle, SettingValue::Bool(on)) => Some(SettingValue::Bool(!on)),
        (Kind::Choice(options), SettingValue::Text(now)) => {
            let position = options.iter().position(|option| option == now).unwrap_or(0) as i32;
            let next = (position + direction).rem_euclid(options.len() as i32) as usize;
            Some(SettingValue::Text(options[next].to_owned()))
        }
        (Kind::Number { step, min, max, integer }, SettingValue::Float(now)) if !integer => {
            // Rounded to the step's precision so 13.0 + 0.5 is not 13.499999.
            let next = ((now + step * direction as f64) / step).round() * step;
            Some(SettingValue::Float((next.clamp(*min, *max) * 1000.0).round() / 1000.0))
        }
        (Kind::Number { step, min, max, .. }, SettingValue::Integer(now)) => {
            let next = (*now as f64 + step * direction as f64).clamp(*min, *max);
            Some(SettingValue::Integer(next as i64))
        }
        _ => None,
    }
}

/// How a value is shown in the list.
pub fn display(value: &SettingValue) -> String {
    match value {
        SettingValue::Bool(true) => "On".into(),
        SettingValue::Bool(false) => "Off".into(),
        SettingValue::Integer(n) => n.to_string(),
        // Up to two decimals, without trailing zeros.
        SettingValue::Float(n) => {
            let text = format!("{n:.2}");
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
        SettingValue::Text(text) => text.clone(),
    }
}

/// One line of the screen: a group heading or a setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    Heading(&'static str),
    Setting(usize),
}

fn lines() -> Vec<Line> {
    let mut out = Vec::new();
    let mut group = "";
    for (index, setting) in SETTINGS.iter().enumerate() {
        if setting.group != group {
            group = setting.group;
            out.push(Line::Heading(group));
        }
        out.push(Line::Setting(index));
    }
    out
}

pub struct ScreenLayout {
    pub frame: Rect,
    /// The visible lines with their rectangles.
    rows: Vec<(Rect, Line)>,
    /// Where the help text and key hints go.
    footer: Rect,
}

impl ScreenLayout {
    /// Lays the screen out, scrolled so the selected setting is visible.
    pub fn new(window: (f32, f32), cell: (f32, f32), scale: f32, selected: usize) -> Self {
        let row_height = (cell.1 + 10.0 * scale).round();
        let width = (620.0 * scale).min(window.0 - 32.0 * scale).max(cell.0 * 20.0).round();
        let x = ((window.0 - width) / 2.0).round();
        let top = (window.1 * 0.08).round();
        let footer_height = row_height * 2.0 + (8.0 * scale).round();
        let title_height = row_height + (6.0 * scale).round();
        let available = window.1 - top * 2.0 - footer_height - title_height;
        let all = lines();
        let capacity = ((available / row_height).floor() as usize).clamp(3, all.len());

        // Scroll just enough to keep the selection (and, at the top of a
        // group, its heading) in view.
        let selected_line = all.iter().position(|line| *line == Line::Setting(selected)).unwrap_or(0);
        let mut first = selected_line.saturating_sub(capacity - 1);
        if selected_line < first + 1 {
            first = selected_line.saturating_sub(1);
        }
        first = first.min(all.len() - capacity);

        let rows_top = top + title_height;
        let rows = all
            .iter()
            .skip(first)
            .take(capacity)
            .enumerate()
            .map(|(i, line)| (Rect::new(x, rows_top + i as f32 * row_height, width, row_height), *line))
            .collect();
        let footer = Rect::new(x, rows_top + capacity as f32 * row_height, width, footer_height);
        let frame = Rect::new(x, top, width, title_height + capacity as f32 * row_height + footer_height);
        Self { frame, rows, footer }
    }

    /// The setting under a window position.
    pub fn setting_at(&self, x: f32, y: f32) -> Option<usize> {
        self.rows.iter().find_map(|(rect, line)| match line {
            Line::Setting(index) if rect.contains(x, y) => Some(*index),
            _ => None,
        })
    }
}

/// Draws the screen. `editing` is the text being typed for the selected
/// setting, when it is a text setting in edit mode.
pub fn draw(
    r: &mut Renderer,
    config: &Config,
    selected: usize,
    editing: Option<&str>,
    window: (f32, f32),
    scale: f32,
    palette: &Palette,
) {
    let metrics = r.metrics();
    let layout = ScreenLayout::new(window, (metrics.width, metrics.height), scale, selected);
    let line = scale.round().max(1.0);
    let pad = (16.0 * scale).round();
    let dim = color(palette.chrome_text_dim);
    let fg = color(palette.foreground);
    let border = color(palette.border);

    let [br, bg, bb, _] = color(palette.background);
    r.rect(0.0, 0.0, window.0, window.1, [br, bg, bb, 130]);
    let frame = layout.frame;
    r.rect(frame.x, frame.y, frame.w, frame.h, color(palette.chrome));
    for (x, y, w, h) in [
        (frame.x - line, frame.y - line, frame.w + line * 2.0, line),
        (frame.x - line, frame.y + frame.h, frame.w + line * 2.0, line),
        (frame.x - line, frame.y, line, frame.h),
        (frame.x + frame.w, frame.y, line, frame.h),
    ] {
        r.rect(x, y, w, h, border);
    }
    let text_width = frame.w - pad * 2.0;
    r.text("Settings", true, frame.x + pad, frame.y + (10.0 * scale).round(), text_width, fg);

    for (rect, row) in &layout.rows {
        let text_y = rect.y + ((rect.h - metrics.height) / 2.0).round();
        match row {
            Line::Heading(title) => {
                r.text(title, true, rect.x + pad, text_y, text_width, color(palette.accent));
            }
            Line::Setting(index) => {
                let setting = &SETTINGS[*index];
                let is_selected = *index == selected;
                if is_selected {
                    r.rect(rect.x, rect.y, rect.w, rect.h, color(palette.selection));
                }
                r.text(setting.label, false, rect.x + pad * 2.0, text_y, text_width, fg);

                let value = match (is_selected, editing) {
                    (true, Some(text)) => text.to_owned(),
                    _ => display(&current(config, setting)),
                };
                // Arrows hint that left and right change the value.
                let adjustable = !matches!(setting.kind, Kind::Text);
                let shown = if is_selected && adjustable { format!("‹ {value} ›") } else { value };
                let width = r.text_width(&shown).min(rect.w * 0.55);
                let value_x = rect.x + rect.w - pad - width;
                let value_color = if is_selected { fg } else { dim };
                let used = r.text(&shown, false, value_x, text_y, width, value_color);
                if is_selected && editing.is_some() {
                    r.rect(value_x + used, text_y, line * 2.0, metrics.height, color(palette.accent));
                }
            }
        }
    }

    let footer = layout.footer;
    r.rect(footer.x, footer.y, footer.w, line, border);
    let help = SETTINGS.get(selected).map_or("", |setting| setting.help);
    let first = footer.y + (8.0 * scale).round();
    r.text(help, false, footer.x + pad, first, text_width, fg);
    let hints = if editing.is_some() {
        "Enter: save  ·  Esc: cancel"
    } else {
        "↑↓ select   ←→ change   Enter edit   ⌘E open file   Esc close"
    };
    r.text(hints, false, footer.x + pad, first + metrics.height + (6.0 * scale).round(), text_width, dim);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_reads_and_writes_back_through_the_file() {
        // Stepping each setting and writing the result must produce a file
        // that still parses, and that reads back as the new value.
        let config = Config::default();
        for setting in SETTINGS {
            let value = current(&config, setting);
            let new = match &setting.kind {
                Kind::Text => SettingValue::Text("changed".into()),
                _ => stepped(setting, &value, 1).unwrap_or_else(|| panic!("{} cannot step", setting.label)),
            };
            let text = ale_config::set_in_text(ale_config::DEFAULT_CONFIG, setting.section, setting.key, &new)
                .unwrap_or_else(|e| panic!("{}: {e}", setting.label));
            let reparsed = Config::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", setting.label));
            assert_eq!(current(&reparsed, setting), new, "{}", setting.label);
            // Keys are unique, so no two rows edit the same thing.
            let same = SETTINGS.iter().filter(|s| (s.section, s.key) == (setting.section, setting.key)).count();
            assert_eq!(same, 1, "{}", setting.label);
        }
    }

    #[test]
    fn stepping_respects_kinds_and_limits() {
        let find = |key: &str| SETTINGS.iter().find(|s| s.key == key).unwrap();
        assert_eq!(stepped(find("size"), &SettingValue::Float(13.0), 1), Some(SettingValue::Float(13.5)));
        assert_eq!(stepped(find("size"), &SettingValue::Float(72.0), 1), Some(SettingValue::Float(72.0)));
        assert_eq!(stepped(find("line_height"), &SettingValue::Float(1.25), -1), Some(SettingValue::Float(1.2)));
        assert_eq!(stepped(find("scrollback"), &SettingValue::Integer(10_000), -1), Some(SettingValue::Integer(9000)));
        assert_eq!(stepped(find("scrollback"), &SettingValue::Integer(0), -1), Some(SettingValue::Integer(0)));
        assert_eq!(stepped(find("sound"), &SettingValue::Bool(true), -1), Some(SettingValue::Bool(false)));
        let mode = find("mode");
        assert_eq!(stepped(mode, &SettingValue::Text("auto".into()), 1), Some(SettingValue::Text("dark".into())));
        assert_eq!(stepped(mode, &SettingValue::Text("auto".into()), -1), Some(SettingValue::Text("light".into())));
        assert_eq!(stepped(find("family"), &SettingValue::Text("Menlo".into()), 1), None);
    }

    #[test]
    fn values_are_shown_plainly() {
        assert_eq!(display(&SettingValue::Float(13.0)), "13");
        assert_eq!(display(&SettingValue::Float(1.25)), "1.25");
        assert_eq!(display(&SettingValue::Float(13.5)), "13.5");
        assert_eq!(display(&SettingValue::Bool(true)), "On");
        assert_eq!(display(&SettingValue::Integer(10_000)), "10000");
    }

    #[test]
    fn the_list_scrolls_to_keep_the_selection_visible_and_clickable() {
        let cell = (8.0, 16.0);
        // A short window: not everything fits.
        for selected in [0, SETTINGS.len() / 2, SETTINGS.len() - 1] {
            let layout = ScreenLayout::new((1000.0, 420.0), cell, 1.0, selected);
            let row = layout.rows.iter().find(|(_, line)| *line == Line::Setting(selected));
            let (rect, _) = row.unwrap_or_else(|| panic!("setting {selected} is not visible"));
            assert_eq!(layout.setting_at(rect.x + 5.0, rect.y + 5.0), Some(selected));
            assert!(rect.y + rect.h <= layout.footer.y + 0.5);
            assert!(layout.frame.y + layout.frame.h <= 420.0);
        }
        // Headings are not settings.
        let layout = ScreenLayout::new((1000.0, 900.0), cell, 1.0, 0);
        let (heading, _) = layout.rows[0];
        assert_eq!(layout.rows[0].1, Line::Heading("Appearance"));
        assert_eq!(layout.setting_at(heading.x + 5.0, heading.y + 5.0), None);
        assert_eq!(layout.rows.len(), lines().len());
    }
}
