//! The start page shown in a new session tab: a place to say what you want
//! done and by which agent, or to ask for a plain terminal instead.
//!
//! Nothing runs until the user chooses. Layout, drawing and hit-testing
//! live here; the app handles the keys and starts things.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ale_agents::AgentKind;
use ale_config::Palette;
use ale_render::Renderer;

use crate::draw::color;
use crate::graph::wrap;
use crate::layout::Rect;
use crate::mention;

/// The state of one start page.
pub struct StartPage {
    /// What the user has typed so far.
    pub text: String,
    /// The agent that will be started.
    pub agent: AgentKind,
    /// Where it will run.
    pub directory: PathBuf,
    /// The folder's files, for `@` references; `None` while being listed.
    pub files: Option<Arc<Vec<String>>>,
    /// The highlighted file while an `@` reference is being typed.
    pub suggestion: usize,
    /// Esc closed the file list for the reference being typed.
    pub suggestions_closed: bool,
}

/// At most this many files are offered for a reference.
pub const SUGGESTIONS: usize = 8;

impl StartPage {
    pub fn new(agent: AgentKind, directory: PathBuf) -> Self {
        StartPage { text: String::new(), agent, directory, files: None, suggestion: 0, suggestions_closed: false }
    }

    /// Whether an `@` reference is being typed and its list is open.
    pub fn referencing(&self) -> bool {
        !self.suggestions_closed && mention::query(&self.text).is_some()
    }

    /// The files offered for the `@` reference being typed, best first.
    pub fn suggestions(&self) -> Vec<String> {
        let (Some(query), Some(files), true) = (mention::query(&self.text), &self.files, self.referencing()) else {
            return Vec::new();
        };
        mention::suggestions(files, query, SUGGESTIONS).into_iter().map(str::to_owned).collect()
    }

    /// The text changed: the file list opens again and starts at the top.
    pub fn text_changed(&mut self) {
        self.suggestion = 0;
        self.suggestions_closed = false;
    }
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
    /// The paperclip: attach files as `@` references.
    Attach,
    /// The folder below the box: choose where the session runs.
    Folder,
    /// A file offered for the `@` reference being typed.
    Suggestion(usize),
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
    attach: Rect,
    folder: Rect,
    /// Rows of the `@` file list, which opens below the box.
    suggestions: Vec<Rect>,
    footer_y: f32,
}

/// Lays the page out. `suggestion_rows` is how many rows the `@` file
/// list shows (0 when closed).
fn layout(area: Rect, cell: (f32, f32), scale: f32, labels: (usize, usize), suggestion_rows: usize) -> Layout {
    let (agent_label_cells, folder_label_cells) = labels;
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
    let attach = Rect::new(input.x + pad - gap, row_y - gap, send_size, send_size);
    let footer_y = input.y + input.h + gap * 2.0;
    let folder_width = (folder_label_cells as f32 * cell.0 + gap * 2.0).min(input.w * 0.6);
    let folder = Rect::new(input.x - gap, footer_y - gap, folder_width, cell.1 + gap * 2.0);
    let row_height = (cell.1 + gap * 1.5).round();
    let list_width = (input.w - pad * 2.0).min(cell.0 * 72.0);
    let suggestions = (0..suggestion_rows)
        .map(|row| Rect::new(input.x + pad, input.y + input.h + line_gap(gap) + row as f32 * row_height, list_width, row_height))
        .collect();
    Layout { heading_y, terminal, input, agent, send, attach, folder, suggestions, footer_y }
}

/// Space between the box and the file list below it.
fn line_gap(gap: f32) -> f32 {
    (gap / 2.0).round()
}

/// The folder button's text.
fn folder_label(page: &StartPage) -> String {
    format!("📁 {} ▾", display_folder(&page.directory))
}

/// How many rows the file list takes: the files, or one line saying why
/// there are none.
fn suggestion_rows(page: &StartPage) -> usize {
    if !page.referencing() { 0 } else { page.suggestions().len().max(1) }
}

fn page_layout(page: &StartPage, area: Rect, cell: (f32, f32), scale: f32) -> Layout {
    let labels = (agent_label(page.agent).chars().count(), folder_label(page).chars().count() + 1);
    layout(area, cell, scale, labels, suggestion_rows(page))
}

/// The picker's text: icon space, name and a marker that it can change.
fn agent_label(agent: AgentKind) -> String {
    format!("   {} ▾", agent.name())
}

pub fn hit(page: &StartPage, area: Rect, cell: (f32, f32), scale: f32, x: f32, y: f32) -> Option<Hit> {
    let layout = page_layout(page, area, cell, scale);
    // The file list lies over the footer, so it comes first.
    let offered = page.suggestions().len();
    if let Some(row) = layout.suggestions.iter().position(|row| row.contains(x, y)) {
        return (row < offered).then_some(Hit::Suggestion(row));
    }
    // Generous targets: a little slack around the link and buttons.
    let slack = 4.0 * scale;
    if layout.terminal.inflate(slack).contains(x, y) {
        Some(Hit::Terminal)
    } else if layout.send.contains(x, y) {
        Some(Hit::Send)
    } else if layout.agent.contains(x, y) {
        Some(Hit::Agent)
    } else if layout.attach.contains(x, y) {
        Some(Hit::Attach)
    } else if layout.folder.contains(x, y) {
        Some(Hit::Folder)
    } else if layout.input.contains(x, y) {
        Some(Hit::Input)
    } else {
        None
    }
}

/// Where the agent picker is; its menu opens below it.
pub fn agent_button(page: &StartPage, area: Rect, cell: (f32, f32), scale: f32) -> Rect {
    page_layout(page, area, cell, scale).agent
}

/// Where the folder button is; its menu opens below it.
pub fn folder_button(page: &StartPage, area: Rect, cell: (f32, f32), scale: f32) -> Rect {
    page_layout(page, area, cell, scale).folder
}

/// How the page looks right now, beyond its own state.
#[derive(Clone, Copy, Default)]
pub struct View {
    /// The control under the pointer.
    pub hovered: Option<Hit>,
    /// The agent menu is open.
    pub picking: bool,
    /// The folder menu is open.
    pub picking_folder: bool,
}

pub fn draw(r: &mut Renderer, page: &StartPage, view: View, area: Rect, scale: f32, palette: &Palette) {
    let metrics = r.metrics();
    let cell = (metrics.width, metrics.height);
    let line = scale.round().max(1.0);
    let pad = (14.0 * scale).round();
    let gap = (6.0 * scale).round();
    let dim = color(palette.chrome_text_dim);
    let fg = color(palette.foreground);
    let label = agent_label(page.agent);
    let layout = page_layout(page, area, cell, scale);
    // The project the session will run in: its repository's name, or the
    // folder's.
    let root = crate::workspace::project_root(&page.directory).unwrap_or_else(|| page.directory.clone());
    let project = root.file_name().map_or_else(|| display_folder(&root), |name| name.to_string_lossy().into_owned());
    let project = project.as_str();

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

    // The paperclip, at the other end of the row.
    let attach = layout.attach;
    if view.hovered == Some(Hit::Attach) {
        r.rect(attach.x, attach.y, attach.w, attach.h, color(palette.selection));
    }
    let clip_x = attach.x + ((attach.w - cell.0 * 2.0) / 2.0).round();
    r.text("📎", false, clip_x, attach.y + ((attach.h - cell.1) / 2.0).round(), cell.0 * 2.0, fg);

    // Where it will run (a button to choose), and how to drive the page.
    let folder = layout.folder;
    let folder_active = view.picking_folder || view.hovered == Some(Hit::Folder);
    if folder_active {
        r.rect(folder.x, folder.y, folder.w, folder.h, color(palette.selection));
    }
    let folder_text = folder_label(page);
    r.text(&folder_text, false, folder.x + gap, layout.footer_y, folder.w - gap, if folder_active { fg } else { dim });
    if let Some(branch) = crate::workspace::project_root(&page.directory).as_deref().and_then(branch) {
        let text = format!("⎇ {branch}");
        let width = r.text_width(&text);
        r.text(&text, false, input.x + input.w - width, layout.footer_y, width, dim);
    }
    let hints = if page.referencing() {
        "↑↓ choose file   Enter or Tab insert   Esc close list"
    } else {
        "Enter start   ⇧Enter new line   @ file   Tab choose agent   ⌘Enter terminal"
    };
    let hints_width = r.text_width(hints).min(input.w);
    let hints_x = area.x + ((area.w - hints_width) / 2.0).round();
    let hints_y = layout.footer_y + cell.1 + gap * 3.0;
    let below_list = layout.suggestions.last().map_or(0.0, |row| row.y + row.h + gap * 2.0);
    r.text(hints, false, hints_x, hints_y.max(below_list), hints_width, dim);

    // The files for the `@` reference being typed, over the footer.
    if let (Some(first), Some(last)) = (layout.suggestions.first(), layout.suggestions.last()) {
        let frame = Rect::new(first.x, first.y, first.w, last.y + last.h - first.y);
        r.rect(frame.x, frame.y, frame.w, frame.h, color(palette.chrome));
        for (bx, by, bw, bh) in [
            (frame.x, frame.y, frame.w, line),
            (frame.x, frame.y + frame.h - line, frame.w, line),
            (frame.x, frame.y, line, frame.h),
            (frame.x + frame.w - line, frame.y, line, frame.h),
        ] {
            r.rect(bx, by, bw, bh, color(palette.border));
        }
        let files = page.suggestions();
        let text_room = first.w - gap * 4.0;
        if files.is_empty() {
            let why = if page.files.is_none() { "Listing files…" } else { "No files match" };
            r.text(why, false, first.x + gap * 2.0, first.y + ((first.h - cell.1) / 2.0).round(), text_room, dim);
        }
        for (index, (row, file)) in layout.suggestions.iter().zip(&files).enumerate() {
            let selected = index == page.suggestion.min(files.len().saturating_sub(1));
            let hovered = view.hovered == Some(Hit::Suggestion(index));
            if selected || hovered {
                r.rect(row.x + line, row.y, row.w - line * 2.0, row.h, color(palette.selection));
            }
            // The folder dimmed, the file's own name bright.
            let text_y = row.y + ((row.h - cell.1) / 2.0).round();
            let (folder, name) = match file.rfind('/') {
                Some(at) => (&file[..=at], &file[at + 1..]),
                None => ("", file.as_str()),
            };
            let shown_folder = fit_start(folder, (text_room - r.text_width(name)).max(0.0), cell.0);
            let used = r.text(&shown_folder, false, row.x + gap * 2.0, text_y, text_room, dim);
            r.text(name, selected, row.x + gap * 2.0 + used, text_y, text_room - used, fg);
        }
    }
}

/// Shortens `text` from the left ("…/src/") to fit `width`.
fn fit_start(text: &str, width: f32, cell_width: f32) -> String {
    let room = (width / cell_width).floor() as usize;
    let count = text.chars().count();
    if count <= room {
        return text.to_owned();
    }
    if room <= 1 {
        return String::new();
    }
    let tail: String = text.chars().skip(count - (room - 1)).collect();
    format!("…{tail}")
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
        StartPage::new(AgentKind::Claude, "/repo".into())
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
        let layout = page_layout(&page, AREA, CELL, 1.0);
        let at = |rect: Rect| hit(&page, AREA, CELL, 1.0, rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        assert_eq!(at(layout.terminal), Some(Hit::Terminal));
        assert_eq!(at(layout.send), Some(Hit::Send));
        assert_eq!(at(layout.agent), Some(Hit::Agent));
        assert_eq!(hit(&page, AREA, CELL, 1.0, layout.input.x + 20.0, layout.input.y + 20.0), Some(Hit::Input));
        assert_eq!(hit(&page, AREA, CELL, 1.0, AREA.x + 2.0, AREA.y + 2.0), None);
        assert_eq!(agent_button(&page, AREA, CELL, 1.0), layout.agent);
        assert_eq!(at(layout.attach), Some(Hit::Attach));
        assert_eq!(at(layout.folder), Some(Hit::Folder));
        assert_eq!(folder_button(&page, AREA, CELL, 1.0), layout.folder);
        // The paperclip sits at the left of the row, clear of the picker.
        assert!(layout.attach.x >= layout.input.x && layout.attach.x + layout.attach.w <= layout.agent.x);
        // The picker and button sit inside the input box, side by side.
        assert!(layout.agent.x >= layout.input.x && layout.agent.x + layout.agent.w <= layout.send.x);
        assert!(layout.send.x + layout.send.w <= layout.input.x + layout.input.w);
        assert!(layout.input.x >= AREA.x && layout.input.x + layout.input.w <= AREA.x + AREA.w);
        // In a very small area nothing goes above it.
        let tiny = Rect::new(0.0, 0.0, 300.0, 120.0);
        assert!(super::layout(tiny, CELL, 1.0, (10, 10), 0).heading_y >= 0.0);
    }

    #[test]
    fn typing_an_at_sign_offers_the_folders_files() {
        let mut page = page();
        page.files = Some(Arc::new(vec!["README.md".into(), "src/main.rs".into(), "src/start.rs".into()]));
        assert!(!page.referencing() && page.suggestions().is_empty());
        page.text = "fix @sta".into();
        assert_eq!(page.suggestions(), ["src/start.rs"]);
        // The list is clickable where it is drawn, over the footer.
        let layout = page_layout(&page, AREA, CELL, 1.0);
        assert_eq!(layout.suggestions.len(), 1);
        let row = layout.suggestions[0];
        assert_eq!(hit(&page, AREA, CELL, 1.0, row.x + 5.0, row.y + 5.0), Some(Hit::Suggestion(0)));
        // Esc closes it until the text changes.
        page.suggestions_closed = true;
        assert!(page.suggestions().is_empty() && page_layout(&page, AREA, CELL, 1.0).suggestions.is_empty());
        page.text_changed();
        assert_eq!(page.suggestions().len(), 1);
        // While the files are being listed, one row says so.
        page.files = None;
        assert_eq!(page_layout(&page, AREA, CELL, 1.0).suggestions.len(), 1);
        assert_eq!(hit(&page, AREA, CELL, 1.0, row.x + 5.0, row.y + 5.0), None);
    }

    #[test]
    fn long_folders_are_shortened_from_the_left() {
        assert_eq!(fit_start("crates/app/src/", 80.0, 8.0), "…/app/src/");
        assert_eq!(fit_start("src/", 80.0, 8.0), "src/");
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
