//! Colors: the built-in dark and light palettes and user overrides.

use serde::{Deserialize, Deserializer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Builds a color from `0xRRGGBB`.
    pub const fn hex(v: u32) -> Self {
        Self { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }

    /// Parses `#rrggbb` (the `#` is optional).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 || !s.is_ascii() {
            return None;
        }
        u32::from_str_radix(s, 16).ok().map(Self::hex)
    }

    /// Linear blend towards `other`; `t` = 0 keeps `self`, 1 gives `other`.
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let ch = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb::new(ch(self.r, other.r), ch(self.g, other.g), ch(self.b, other.b))
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgb::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!("invalid color {s:?}, expected \"#rrggbb\""))
        })
    }
}

/// Every color the app draws with.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    /// Highlight color for the active tab marker and focused pane.
    pub accent: Rgb,
    /// The 16 ANSI colors: 8 normal followed by 8 bright.
    pub ansi: [Rgb; 16],
    /// Background of the tab bar and other chrome.
    pub chrome: Rgb,
    /// Background of the selected tab.
    pub chrome_active: Rgb,
    /// Secondary text in the chrome.
    pub chrome_text_dim: Rgb,
    /// Lines between panes and under the tab bar.
    pub border: Rgb,
}

/// User-supplied colors. Anything left out keeps the built-in value.
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct PaletteOverride {
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub selection: Option<Rgb>,
    pub accent: Option<Rgb>,
    pub ansi: Option<[Rgb; 16]>,
}

impl Palette {
    pub fn dark() -> Self {
        Self::derive(
            Rgb::hex(0x0e0e10),
            Rgb::hex(0xe4e4e7),
            Rgb::hex(0x2f4370),
            Rgb::hex(0xd97757),
            [
                0x1c1c1f, 0xf0616d, 0x8cc570, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xc8ccd4,
                0x5c6370, 0xff7b86, 0xa5e075, 0xf0d58a, 0x7cc0ff, 0xd88ef0, 0x6fd3df, 0xffffff,
            ],
        )
    }

    pub fn light() -> Self {
        Self::derive(
            Rgb::hex(0xfcfcfc),
            Rgb::hex(0x24242a),
            Rgb::hex(0xbcd3f7),
            Rgb::hex(0xc45f3c),
            [
                0x24242a, 0xc7323f, 0x3f8f2f, 0xa87400, 0x2a6fd1, 0xa035b8, 0x13808f, 0xb4b6bd,
                0x70747f, 0xdf4250, 0x4fa53c, 0xbf8600, 0x3d83e8, 0xb648cf, 0x1795a6, 0x3a3a42,
            ],
        )
    }

    /// Fills in the chrome colors from the background and foreground so that
    /// overriding just those two still gives a coherent look.
    fn derive(background: Rgb, foreground: Rgb, selection: Rgb, accent: Rgb, ansi: [u32; 16]) -> Self {
        let mut palette = Self {
            background,
            foreground,
            cursor: foreground,
            selection,
            accent,
            ansi: ansi.map(Rgb::hex),
            chrome: background,
            chrome_active: background,
            chrome_text_dim: foreground,
            border: background,
        };
        palette.derive_chrome();
        palette
    }

    fn derive_chrome(&mut self) {
        let (bg, fg) = (self.background, self.foreground);
        self.chrome = bg.mix(fg, 0.05);
        self.chrome_active = bg;
        self.chrome_text_dim = fg.mix(bg, 0.45);
        self.border = bg.mix(fg, 0.13);
    }

    /// Returns this palette with the user's overrides applied.
    pub fn with(mut self, o: &PaletteOverride) -> Self {
        if let Some(c) = o.background {
            self.background = c;
        }
        if let Some(c) = o.foreground {
            self.foreground = c;
            self.cursor = c;
        }
        if let Some(c) = o.cursor {
            self.cursor = c;
        }
        if let Some(c) = o.selection {
            self.selection = c;
        }
        if let Some(c) = o.accent {
            self.accent = c;
        }
        if let Some(c) = o.ansi {
            self.ansi = c;
        }
        self.derive_chrome();
        self
    }

    /// Looks up one of the 256 indexed terminal colors: 0-15 are the ANSI
    /// colors, 16-231 a 6x6x6 color cube, 232-255 a gray ramp.
    pub fn indexed(&self, index: u8) -> Rgb {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let i = index - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb::new(level(i / 36), level((i / 6) % 6), level(i % 6))
            }
            _ => {
                let v = 8 + (index - 232) * 10;
                Rgb::new(v, v, v)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colors() {
        assert_eq!(Rgb::parse("#ff8000"), Some(Rgb::new(255, 128, 0)));
        assert_eq!(Rgb::parse("ff8000"), Some(Rgb::new(255, 128, 0)));
        assert_eq!(Rgb::parse("#fff"), None);
        assert_eq!(Rgb::parse("#gggggg"), None);
    }

    #[test]
    fn indexed_colors_match_xterm() {
        let p = Palette::dark();
        assert_eq!(p.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(p.indexed(196), Rgb::new(255, 0, 0));
        assert_eq!(p.indexed(231), Rgb::new(255, 255, 255));
        assert_eq!(p.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(p.indexed(255), Rgb::new(238, 238, 238));
    }
}
