//! Small marks that tell the agents apart at a glance, on tabs and in the
//! sidebar.
//!
//! These are simple shapes in each agent's usual color, drawn here from
//! formulas at whatever size the text is. They are stand-ins, not the
//! vendors' logos: those are artwork we do not ship.

use ale_agents::AgentKind;

/// Renderer image keys for the icons. Pane ids start at 1 and occupy the
/// upper half of an image key, so keys with an empty upper half are ours.
pub fn key(kind: AgentKind) -> u64 {
    0xA1E0_0000 + kind as u64
}

fn color(kind: AgentKind) -> [f32; 3] {
    match kind {
        AgentKind::Claude => [217.0, 119.0, 87.0],
        AgentKind::Codex => [222.0, 222.0, 228.0],
        AgentKind::OpenCode => [150.0, 200.0, 255.0],
        AgentKind::Gemini => [86.0, 140.0, 247.0],
        AgentKind::Kiro => [150.0, 110.0, 246.0],
    }
}

/// Distance from (x, y) to the segment a-b.
fn segment(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (px, py, vx, vy) = (x - a.0, y - a.1, b.0 - a.0, b.1 - a.1);
    let t = ((px * vx + py * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
    ((px - vx * t).powi(2) + (py - vy * t).powi(2)).sqrt()
}

/// Signed distance to a box centred on the origin (negative inside).
fn boxed(x: f32, y: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let (dx, dy) = (x.abs() - (half_w - radius), y.abs() - (half_h - radius));
    (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - radius
}

/// Signed distance to each agent's shape, in a square from -1 to 1.
fn shape(kind: AgentKind, x: f32, y: f32) -> f32 {
    match kind {
        // A burst of eight rays.
        AgentKind::Claude => (0..4)
            .map(|i| {
                let angle = i as f32 * std::f32::consts::FRAC_PI_4;
                let (s, c) = angle.sin_cos();
                segment(x, y, (-c * 0.82, -s * 0.82), (c * 0.82, s * 0.82)) - 0.12
            })
            .fold(f32::MAX, f32::min),
        // A hexagonal ring.
        AgentKind::Codex => {
            let (ax, ay) = (x.abs(), y.abs());
            let hexagon = (ax * 0.866 + ay * 0.5).max(ay) - 0.74;
            hexagon.abs() - 0.13
        }
        // A four-pointed star.
        AgentKind::Gemini => (x.abs().powf(0.6) + y.abs().powf(0.6)).powf(1.0 / 0.6) - 0.86,
        // A rounded figure with two eyes.
        AgentKind::Kiro => {
            let body = boxed(x, y - 0.05, 0.66, 0.8, 0.6).max(-(y + 0.9));
            let eyes = boxed(x.abs() - 0.24, y + 0.12, 0.1, 0.16, 0.1);
            body.max(-eyes)
        }
        // A square frame with a block inside.
        AgentKind::OpenCode => {
            let frame = boxed(x, y, 0.78, 0.78, 0.12).abs() - 0.1;
            frame.min(boxed(x, y, 0.3, 0.3, 0.04))
        }
    }
}

/// Draws an agent's icon as `size` x `size` RGBA pixels, with soft edges.
pub fn render(kind: AgentKind, size: u32) -> Vec<u8> {
    let [r, g, b] = color(kind);
    let half = size as f32 / 2.0;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let x = (px as f32 + 0.5 - half) / half;
            let y = (py as f32 + 0.5 - half) / half;
            // Distance in pixels; half a pixel either side of the edge
            // fades, which is what smooths it.
            let distance = shape(kind, x, y) * half;
            let alpha = (0.5 - distance).clamp(0.0, 1.0);
            pixels.extend_from_slice(&[r as u8, g as u8, b as u8, (alpha * 255.0) as u8]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agent_has_a_distinct_icon_that_fills_its_square_sensibly() {
        let mut seen = Vec::new();
        for kind in AgentKind::ALL {
            let pixels = render(kind, 32);
            assert_eq!(pixels.len(), 32 * 32 * 4);
            let covered = pixels.chunks(4).filter(|p| p[3] > 128).count();
            // Neither empty nor a solid block.
            assert!((60..800).contains(&covered), "{kind:?} covers {covered} of 1024 pixels");
            // The corners stay clear.
            assert_eq!(pixels[3], 0, "{kind:?}");
            assert!(!seen.contains(&pixels), "{kind:?} looks like another agent");
            seen.push(pixels);
        }
        let keys: std::collections::HashSet<u64> = AgentKind::ALL.iter().map(|kind| key(*kind)).collect();
        assert_eq!(keys.len(), AgentKind::ALL.len());
        // Icon keys can never collide with a pane's images.
        assert!(keys.iter().all(|key| key >> 32 == 0));
    }
}
