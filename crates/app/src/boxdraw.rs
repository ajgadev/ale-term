//! Box-drawing and block characters drawn as rectangles.
//!
//! Font glyphs for these rarely line up exactly with the cell grid, which
//! leaves hairline gaps in borders drawn by terminal UIs. Drawing them
//! ourselves makes lines join perfectly at any font size or line height.

use ale_render::{Color, Renderer};

/// Which arms of a line character are present, and how thick they are.
struct Arms {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    heavy: bool,
}

fn arms(c: char) -> Option<Arms> {
    let light = |up, down, left, right| Some(Arms { up, down, left, right, heavy: false });
    let heavy = |up, down, left, right| Some(Arms { up, down, left, right, heavy: true });
    match c {
        '─' => light(false, false, true, true),
        '│' => light(true, true, false, false),
        // Rounded corners are drawn square for now.
        '┌' | '╭' => light(false, true, false, true),
        '┐' | '╮' => light(false, true, true, false),
        '└' | '╰' => light(true, false, false, true),
        '┘' | '╯' => light(true, false, true, false),
        '├' => light(true, true, false, true),
        '┤' => light(true, true, true, false),
        '┬' => light(false, true, true, true),
        '┴' => light(true, false, true, true),
        '┼' => light(true, true, true, true),
        '╴' => light(false, false, true, false),
        '╵' => light(true, false, false, false),
        '╶' => light(false, false, false, true),
        '╷' => light(false, true, false, false),
        '━' => heavy(false, false, true, true),
        '┃' => heavy(true, true, false, false),
        '┏' => heavy(false, true, false, true),
        '┓' => heavy(false, true, true, false),
        '┗' => heavy(true, false, false, true),
        '┛' => heavy(true, false, true, false),
        '┣' => heavy(true, true, false, true),
        '┫' => heavy(true, true, true, false),
        '┳' => heavy(false, true, true, true),
        '┻' => heavy(true, false, true, true),
        '╋' => heavy(true, true, true, true),
        _ => None,
    }
}

/// Draws `c` into the cell at (`x`, `y`) if it is a character we draw by
/// hand. Returns false when the font should draw it instead.
pub fn draw(r: &mut Renderer, c: char, x: f32, y: f32, w: f32, h: f32, color: Color) -> bool {
    if !('\u{2500}'..='\u{259f}').contains(&c) {
        return false;
    }

    if let Some(arms) = arms(c) {
        let light = (w / 8.0).round().max(1.0);
        let t = if arms.heavy { light * 2.0 } else { light };
        // Top-left corner of the square where the arms meet.
        let cx = x + ((w - t) / 2.0).round();
        let cy = y + ((h - t) / 2.0).round();
        if arms.left {
            r.rect(x, cy, cx + t - x, t, color);
        }
        if arms.right {
            r.rect(cx, cy, x + w - cx, t, color);
        }
        if arms.up {
            r.rect(cx, y, t, cy + t - y, color);
        }
        if arms.down {
            r.rect(cx, cy, t, y + h - cy, color);
        }
        return true;
    }

    let alpha = |a: f32| [color[0], color[1], color[2], (color[3] as f32 * a) as u8];
    match c {
        '█' => r.rect(x, y, w, h, color),
        '▀' => r.rect(x, y, w, (h / 2.0).round(), color),
        '▌' => r.rect(x, y, (w / 2.0).round(), h, color),
        '▐' => {
            let half = (w / 2.0).round();
            r.rect(x + half, y, w - half, h, color);
        }
        // Lower one-eighth to seven-eighths blocks.
        '▁'..='▇' => {
            let eighths = (c as u32 - '▁' as u32 + 1) as f32;
            let height = (h * eighths / 8.0).round();
            r.rect(x, y + h - height, w, height, color);
        }
        // Left seven-eighths to one-eighth blocks.
        '▉'..='▏' => {
            let eighths = 7.0 - (c as u32 - '▉' as u32) as f32;
            r.rect(x, y, (w * eighths / 8.0).round(), h, color);
        }
        '░' => r.rect(x, y, w, h, alpha(0.25)),
        '▒' => r.rect(x, y, w, h, alpha(0.5)),
        '▓' => r.rect(x, y, w, h, alpha(0.75)),
        _ => return false,
    }
    true
}
