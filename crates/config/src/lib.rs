//! Configuration for ale-term.
//!
//! The config lives in one TOML file (`~/.config/ale-term/config.toml`). Every
//! field is optional: anything missing falls back to the built-in default, so
//! an empty file is a valid config.
//!
//! This crate has no knowledge of windows or rendering. It only parses the
//! file and hands out plain data.

mod theme;

use std::path::PathBuf;
use std::time::SystemTime;

use serde::Deserialize;

pub use theme::{Palette, PaletteOverride, Rgb};

/// Template written to disk the first time the app runs.
pub const DEFAULT_CONFIG: &str = include_str!("default.toml");

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub font: FontConfig,
    pub window: WindowConfig,
    pub terminal: TerminalConfig,
    pub theme: ThemeConfig,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct FontConfig {
    /// Font family name as shown in Font Book.
    pub family: String,
    /// Size in points.
    pub size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self { family: "Menlo".into(), size: 13.0, line_height: 1.25 }
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WindowConfig {
    /// Initial window size in points.
    pub width: f32,
    pub height: f32,
    /// Space between a pane's edge and its text, in points.
    pub padding: f32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self { width: 1100.0, height: 700.0, padding: 8.0 }
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalConfig {
    /// Lines of history kept per pane.
    pub scrollback: usize,
    /// Program to run instead of the login shell.
    pub shell: Option<String>,
    pub shell_args: Vec<String>,
    /// Treat the Option key as Meta (sends ESC + key) instead of letting macOS
    /// compose characters with it. Off by default because many keyboard
    /// layouts need Option to type characters such as `@`, `[` or `\`.
    pub option_as_meta: bool,
    /// Lines scrolled per mouse-wheel notch.
    pub scroll_multiplier: f32,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            scrollback: 10_000,
            shell: None,
            shell_args: Vec::new(),
            option_as_meta: false,
            scroll_multiplier: 3.0,
        }
    }
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Follow the macOS appearance.
    #[default]
    Auto,
    Dark,
    Light,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    pub mode: ThemeMode,
    /// Color overrides applied on top of the built-in dark theme.
    pub dark: PaletteOverride,
    /// Color overrides applied on top of the built-in light theme.
    pub light: PaletteOverride,
}

impl Config {
    /// Resolves the palette to draw with. `system_is_dark` is only consulted
    /// when the mode is `auto`.
    pub fn palette(&self, system_is_dark: bool) -> Palette {
        let dark = match self.theme.mode {
            ThemeMode::Auto => system_is_dark,
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
        };
        if dark {
            Palette::dark().with(&self.theme.dark)
        } else {
            Palette::light().with(&self.theme.light)
        }
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let mut config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        config.sanitize();
        Ok(config)
    }

    /// Clamps values that would otherwise break layout or rendering.
    fn sanitize(&mut self) {
        self.font.size = self.font.size.clamp(6.0, 72.0);
        self.font.line_height = self.font.line_height.clamp(0.8, 3.0);
        self.window.padding = self.window.padding.clamp(0.0, 64.0);
        self.window.width = self.window.width.max(300.0);
        self.window.height = self.window.height.max(200.0);
        self.terminal.scroll_multiplier = self.terminal.scroll_multiplier.clamp(0.1, 20.0);
    }
}

/// Where the config file lives: `$XDG_CONFIG_HOME/ale-term/config.toml`, or
/// `~/.config/ale-term/config.toml`.
pub fn config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("ale-term").join("config.toml"))
}

/// Loads the config, creating the file from the template if it is missing.
///
/// Never fails: on any error the defaults are returned together with a
/// message describing what went wrong, so the app always starts.
pub fn load() -> (Config, Option<String>) {
    let Some(path) = config_path() else {
        return (Config::default(), Some("cannot locate the home directory".into()));
    };
    if !path.exists() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, DEFAULT_CONFIG);
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => match Config::parse(&text) {
            Ok(config) => (config, None),
            Err(e) => (Config::default(), Some(format!("{}: {e}", path.display()))),
        },
        Err(e) => (Config::default(), Some(format!("{}: {e}", path.display()))),
    }
}

/// Reads the config again. Unlike [`load`], an invalid file is an error so
/// the caller can keep using the config it already has.
pub fn reload() -> Result<Config, String> {
    let path = config_path().ok_or("cannot locate the home directory")?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Config::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Watches the config file and calls `on_change` whenever it is modified.
///
/// This checks the file's modification time once a second on a background
/// thread. One `stat` call per second is far cheaper than pulling in a
/// file-watching library, and editors that replace the file on save (which
/// breaks naive watchers) are handled for free.
pub fn watch(on_change: impl Fn() + Send + 'static) {
    let Some(path) = config_path() else { return };
    let modified = move || -> Option<SystemTime> { std::fs::metadata(&path).ok()?.modified().ok() };
    let _ = std::thread::Builder::new().name("config-watch".into()).spawn(move || {
        let mut last = modified();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let now = modified();
            if now != last {
                last = now;
                on_change();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn default_template_is_valid_and_equals_defaults() {
        assert_eq!(Config::parse(DEFAULT_CONFIG).unwrap(), Config::default());
    }

    #[test]
    fn overrides_apply_on_top_of_builtin_theme() {
        let config = Config::parse("[theme.dark]\nbackground = \"#102030\"").unwrap();
        let palette = config.palette(true);
        assert_eq!(palette.background, Rgb::new(0x10, 0x20, 0x30));
        assert_eq!(palette.foreground, Palette::dark().foreground);
        // The light theme is untouched.
        assert_eq!(config.palette(false), Palette::light());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("[font]\nsizee = 12").is_err());
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let config = Config::parse("[font]\nsize = 500").unwrap();
        assert_eq!(config.font.size, 72.0);
    }

    #[test]
    fn bad_color_is_an_error() {
        assert!(Config::parse("[theme.dark]\nbackground = \"blue\"").is_err());
    }
}
