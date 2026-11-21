//! The start page shown in a new session tab: a place to say what you want
//! done and by which agent, or to ask for a plain terminal instead.
//!
//! Nothing runs until the user chooses. Layout, drawing and hit-testing
//! live here; the app handles the keys and starts things.

use std::path::{Path, PathBuf};

use ale_agents::AgentKind;
use ale_config::Palette;
use ale_render::Renderer;

use crate::draw::color;
use crate::graph::wrap;
use crate::layout::Rect;

/// The state of one start page.
pub struct StartPage {
    /// What the user has typed so far.
    pub text: String,
    /// The agent that will be started.
    pub agent: AgentKind,
    /// Where it will run.
    pub directory: PathBuf,
}

/// What a point on the page is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// "or open a terminal".
    Terminal,
    /// The agent's name: opens the list of agents.
    Agent,
    /// The send button.
    Send,
    /// The text box (shows a text cursor).
    Input,
}

/// The name of the git branch checked out in `root`, read from the
/// repository itself (no `git` process is run).
pub fn branch(root: &Path) -> Option<String> {
    let head = std::fs::read_to_string(root.join(".git").join("HEAD")).ok()?;
    let name = head.trim().strip_prefix("ref: refs/heads/")?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// Quotes `text` so a shell passes it on as one argument, unchanged.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The command line that starts `agent` on `prompt`.
///
/// `configured` is the user's own command for new sessions; it is used when
/// it is for the same agent, so any options in it are kept.
pub fn launch_command(agent: AgentKind, configured: &str, prompt: &str) -> String {
    let base = match AgentKind::from_command(configured) {
        Some(kind) if kind == agent => configured.trim(),
        _ => ale_agents::archive::start_command(agent),
    };
    let prompt = prompt.trim();
    if prompt.is_empty() { base.to_owned() } else { format!("{base} {}", shell_quote(prompt)) }
}

struct Layout {
    heading_y: f32,
    terminal: Rect,
    input: Rect,
    agent: Rect,
    send: Rect,
    footer_y: f32,
}

fn layout(area: Rect, cell: (f32, f32), scale: f32, agent_label_cells: usize) -> Layout {
    let gap = (6.0 * scale).round();
    let pad = (14.0 * scale).round();
    let width = (820.0 * scale).min(area.w - 48.0 * scale).max(cell.0 * 24.0).round();
    let x = area.x + ((area.w - width) / 2.0).round();
    // Four lines of text, plus the row holding the picker and send button.
    let input_height = cell.1 * 5.0 + pad * 2.0 + gap * 2.0;
    let total = cell.1 * 2.0 + gap * 5.0 + input_height + gap * 3.0 + cell.1;
    let top = area.y + ((area.h - total) * 0.42).round().max(gap);

    let heading_y = top;
    let link = "or open a terminal";
    let link_width = link.chars().count() as f32 * cell.0;
    let terminal_y = heading_y + cell.1 + gap * 2.0;
    let terminal = Rect::new(area.x + ((area.w - link_width) / 2.0).round(), terminal_y, link_width, cell.1);

    let input = Rect::new(x, terminal_y + cell.1 + gap * 4.0, width, input_height);
    let row_y = input.y + input.h - pad - cell.1;
    let send_size = cell.1 + gap * 2.0;
    let send = Rect::new(input.x + input.w - pad - send_size, row_y - gap, send_size, send_size);
    let agent_width = agent_label_cells as f32 * cell.0 + gap * 2.0;
    let agent = Rect::new(send.x - gap * 2.0 - agent_width, row_y - gap, agent_width, send_size);
    Layout { heading_y, terminal, input, agent, send, footer_y: input.y + input.h + gap * 2.0 }
}

/// The picker's text: icon space, name and a marker that it can change.
fn agent_label(agent: AgentKind) -> String {
    format!("   {} ▾", agent.name())
}

pub fn hit(page: &StartPage, area: Rect, cell: (f32, f32), scale: f32, x: f32, y: f32) -> Option<Hit> {
    let layout = layout(area, cell, scale, agent_label(page.agent).chars().count());
    // Generous targets: a little slack around the link and buttons.
    let slack = 4.0 * scale;
    if layout.terminal.inflate(slack).contains(x, y) {
        Some(Hit::Terminal)
    } else if layout.send.contains(x, y) {
        Some(Hit::Send)
    } else if layout.agent.contains(x, y) {
        Some(Hit::Agent)
    } else if layout.input.contains(x, y) {
        Some(Hit::Input)
    } else {
        None
    }
}

/// Where the agent picker is; its menu opens below it.
pub fn agent_button(page: &StartPage, area: Rect, cell: (f32, f32), scale: f32) -> Rect {
    layout(area, cell, scale, agent_label(page.agent).chars().count()).agent
}

/// How the page looks right now, beyond its own state.
#[derive(Clone, Copy, Default)]
pub struct View {
    /// The control under the pointer.
    pub hovered: Option<Hit>,
    /// The agent menu is open.
    pub picking: bool,
}

pub fn draw(r: &mut Renderer, page: &StartPage, view: View, project: &str, area: Rect, scale: f32, palette: &Palette) {
    let metrics = r.metrics();
    let cell = (metrics.width, metrics.height);
    let line = scale.round().max(1.0);
    let pad = (14.0 * scale).round();
    let gap = (6.0 * scale).round();
    let dim = color(palette.chrome_text_dim);
    let fg = color(palette.foreground);
    let label = agent_label(page.agent);
    let layout = layout(area, cell, scale, label.chars().count());

    r.rect(area.x, area.y, area.w, area.h, color(palette.background));

    // "What should we build in <project>?", with the project emphasized.
    let (before, after) = ("What should we build in ", "?");
    let heading_width = r.text_width(before) + r.text_width(project) + r.text_width(after);
    let mut x = area.x + ((area.w - heading_width) / 2.0).round().max(0.0);
    x += r.text(before, false, x, layout.heading_y, area.w, fg);
    x += r.text(project, true, x, layout.heading_y, area.w, fg);
    r.text(after, false, x, layout.heading_y, area.w, fg);
    let link_hovered = view.hovered == Some(Hit::Terminal);
    let link_color = if link_hovered { fg } else { dim };
    r.text("or open a terminal", false, layout.terminal.x, layout.terminal.y, layout.terminal.w + cell.0, link_color);
    let underline = if link_hovered { palette.accent } else { palette.border };
    r.rect(layout.terminal.x, layout.terminal.y + cell.1, layout.terminal.w, line, color(underline));

    // The input box.
    let input = layout.input;
    r.rect(input.x, input.y, input.w, input.h, color(palette.chrome));
    for (bx, by, bw, bh) in [
        (input.x, input.y, input.w, line),
        (input.x, input.y + input.h - line, input.w, line),
        (input.x, input.y, line, input.h),
        (input.x + input.w - line, input.y, line, input.h),
    ] {
        r.rect(bx, by, bw, bh, color(palette.border));
    }
    let text_x = input.x + pad;
    let text_width = input.w - pad * 2.0;
    let columns = (text_width / cell.0).floor() as usize;
    let row_height = cell.1 + (gap / 3.0).round();
    if page.text.is_empty() {
        r.text("Ask anything", false, text_x, input.y + pad, text_width, dim);
        r.rect(text_x, input.y + pad, line * 2.0, cell.1, color(palette.accent));
    } else {
        // Wrapped, showing the last rows that fit, with a caret at the end.
        let rows: Vec<String> = page.text.split('\n').flat_map(|paragraph| wrap(paragraph, columns)).collect();
        let visible = 4;
        let skip = rows.len().saturating_sub(visible);
        let mut caret = (text_x, input.y + pad);
        for (index, row) in rows.iter().skip(skip).enumerate() {
            let y = input.y + pad + index as f32 * row_height;
            let used = r.text(row, false, text_x, y, text_width, fg);
            caret = (text_x + used, y);
        }
        r.rect(caret.0, caret.1, line * 2.0, cell.1, color(palette.accent));
    }

    // Agent picker and send button.
    let agent = layout.agent;
    if view.picking || view.hovered == Some(Hit::Agent) {
        r.rect(agent.x, agent.y, agent.w, agent.h, color(palette.selection));
    }
    let label_y = agent.y + ((agent.h - cell.1) / 2.0).round();
    let icon = cell.1.min(cell.0 * 2.5).round();
    r.image(crate::icons::key(page.agent), agent.x + gap, label_y, icon, icon, [0.0, 0.0, 1.0, 1.0]);
    r.text(&label, false, agent.x + gap, label_y, agent.w, fg);
    let send = layout.send;
    let ready = !page.text.trim().is_empty();
    let send_color = match (ready, view.hovered == Some(Hit::Send)) {
        (true, true) => lighten(palette.accent),
        (true, false) => palette.accent,
        (false, true) => palette.selection,
        (false, false) => palette.border,
    };
    r.rect(send.x, send.y, send.w, send.h, color(send_color));
    let arrow_x = send.x + ((send.w - cell.0) / 2.0).round();
    r.text("↑", true, arrow_x, send.y + ((send.h - cell.1) / 2.0).round(), send.w, color(palette.background));

    // Where it will run, and how to drive the page.
    let folder = display_folder(&page.directory);
    r.text(&folder, false, input.x, layout.footer_y, input.w * 0.6, dim);
    if let Some(branch) = crate::workspace::project_root(&page.directory).as_deref().and_then(branch) {
        let text = format!("⎇ {branch}");
        let width = r.text_width(&text);
        r.text(&text, false, input.x + input.w - width, layout.footer_y, width, dim);
    }
    let hints = "Enter start   ⇧Enter new line   Tab choose agent   ⌘Enter terminal";
    let hints_width = r.text_width(hints).min(input.w);
    let hints_x = area.x + ((area.w - hints_width) / 2.0).round();
    r.text(hints, false, hints_x, layout.footer_y + cell.1 + gap * 3.0, hints_width, dim);
}

/// A color a little closer to white, for a hovered button.
fn lighten(rgb: ale_config::Rgb) -> ale_config::Rgb {
    let up = |c: u8| c + (255 - c) / 6;
    ale_config::Rgb { r: up(rgb.r), g: up(rgb.g), b: up(rgb.b) }
}

/// A folder with the home folder shortened to `~`.
fn display_folder(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(260.0, 38.0, 1000.0, 700.0);
    const CELL: (f32, f32) = (8.0, 16.0);

    fn page() -> StartPage {
        StartPage { text: String::new(), agent: AgentKind::Claude, directory: "/repo".into() }
    }

    #[test]
    fn prompts_are_quoted_so_the_shell_cannot_misread_them() {
        assert_eq!(shell_quote("fix the bug"), "'fix the bug'");
        assert_eq!(shell_quote("it's broken"), r"'it'\''s broken'");
        // Everything a shell would otherwise act on stays inert in quotes.
        assert_eq!(shell_quote("$(rm -rf ~); `x` && y | z > f"), "'$(rm -rf ~); `x` && y | z > f'");

        // And a shell really does hand it over unchanged.
        for text in ["it's \"here\"", "$(echo no) `no` $HOME \\n", "two\nlines; rm x", "'''"] {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf %s {}", shell_quote(text)))
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&output.stdout), text);
        }
    }

    #[test]
    fn launch_commands() {
        let claude = AgentKind::Claude;
        assert_eq!(launch_command(claude, "claude", ""), "claude");
        assert_eq!(launch_command(claude, "claude", "  add tests "), "claude 'add tests'");
        // The user's own options are kept when they are for this agent...
        assert_eq!(launch_command(claude, "claude --model opus", "hi"), "claude --model opus 'hi'");
        // ...and not when a different agent was picked.
        assert_eq!(launch_command(AgentKind::Codex, "claude --model opus", "hi"), "codex 'hi'");
        assert_eq!(launch_command(AgentKind::Kiro, "claude", ""), "kiro-cli");
    }

    #[test]
    fn controls_are_inside_the_page_and_clickable() {
        let page = page();
        let layout = layout(AREA, CELL, 1.0, agent_label(page.agent).chars().count());
        let at = |rect: Rect| hit(&page, AREA, CELL, 1.0, rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        assert_eq!(at(layout.terminal), Some(Hit::Terminal));
        assert_eq!(at(layout.send), Some(Hit::Send));
        assert_eq!(at(layout.agent), Some(Hit::Agent));
        assert_eq!(hit(&page, AREA, CELL, 1.0, layout.input.x + 20.0, layout.input.y + 20.0), Some(Hit::Input));
        assert_eq!(hit(&page, AREA, CELL, 1.0, AREA.x + 2.0, AREA.y + 2.0), None);
        assert_eq!(agent_button(&page, AREA, CELL, 1.0), layout.agent);
        // The picker and button sit inside the input box, side by side.
        assert!(layout.agent.x >= layout.input.x && layout.agent.x + layout.agent.w <= layout.send.x);
        assert!(layout.send.x + layout.send.w <= layout.input.x + layout.input.w);
        assert!(layout.input.x >= AREA.x && layout.input.x + layout.input.w <= AREA.x + AREA.w);
        // In a very small area nothing goes above it.
        let tiny = Rect::new(0.0, 0.0, 300.0, 120.0);
        assert!(super::layout(tiny, CELL, 1.0, 10).heading_y >= 0.0);
    }

    #[test]
    fn reads_the_branch_from_the_repository() {
        let dir = std::env::temp_dir().join(format!("ale-term-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/feature/login\n").unwrap();
        assert_eq!(branch(&dir).as_deref(), Some("feature/login"));
        // A detached HEAD (a bare commit id) has no branch name.
        std::fs::write(dir.join(".git/HEAD"), "3f2a91bc7d104e55a001\n").unwrap();
        assert_eq!(branch(&dir), None);
        assert_eq!(branch(&dir.join("missing")), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
