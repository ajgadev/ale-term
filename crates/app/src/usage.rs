//! The usage screen: how much the agents have been used, over the whole
//! window. Totals, tokens by day, when the agents work, and a breakdown by
//! model and by agent.
//!
//! The app supplies the numbers (a [`View`]); this module lays them out,
//! draws them and says what is under the pointer.

use ale_agents::AgentKind;
use ale_agents::usage::{Report, Totals, date_from_days};
use ale_config::{Palette, Rgb};
use ale_render::{Color, Renderer};

use crate::draw::{color, dot};
use crate::layout::Rect;

/// The periods offered: label and number of days (`None` for everything).
pub const RANGES: [(&str, Option<u32>); 4] =
    [("7 days", Some(7)), ("30 days", Some(30)), ("90 days", Some(90)), ("All", None)];

/// A row of the workspace menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuRow {
    pub label: String,
    /// The project's folder.
    pub detail: String,
    /// It is the workspace on show.
    pub checked: bool,
}

/// What the screen shows.
pub struct View<'a> {
    /// The numbers; `None` until they have been read for the first time.
    pub report: Option<&'a Report>,
    /// The agents' records are being read again.
    pub scanning: bool,
    /// Index into [`RANGES`].
    pub range: usize,
    /// Name of the workspace on show.
    pub workspace: &'a str,
    pub rows: &'a [MenuRow],
    /// The open workspace menu: its highlighted row and the first row shown.
    pub menu: Option<(usize, usize)>,
    /// Short name of the only agent on show, if one was picked.
    pub agent: Option<&'a str>,
    pub pointer: (f32, f32),
}

/// Something under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Close,
    Range(usize),
    /// The button that opens the workspace menu.
    Workspace,
    MenuRow(usize),
    /// A row of the agents table.
    Agent(usize),
    /// A bar of the tokens chart.
    Bar(usize),
    /// A square of the heat map: weekday (Monday first) and hour.
    Hour(usize, usize),
}

impl Hit {
    /// Clicking it does something.
    pub fn clickable(self) -> bool {
        !matches!(self, Hit::Bar(_) | Hit::Hour(..))
    }
}

/// Widest workspace name shown on the button, in characters.
const WORKSPACE_CHARS: usize = 28;
/// Width of the workspace menu, in characters.
const MENU_CHARS: f32 = 44.0;
/// Width of a column of numbers in the tables, in characters.
const NUMBER_CHARS: f32 = 9.0;

/// Where everything is on screen.
pub struct Layout {
    cell: (f32, f32),
    scale: f32,
    /// The whole screen's content area.
    frame: Rect,
    title: (f32, f32),
    close: Rect,
    ranges: [Rect; 4],
    workspace: Rect,
    menu: Option<(Rect, Vec<(usize, Rect)>)>,
    tiles: [Rect; 4],
    /// Each section: its heading line and its body.
    chart: (Rect, Rect),
    heat: (Rect, Rect),
    models: (Rect, Rect),
    agents: (Rect, Rect),
    /// The tokens chart's bars: the area they fill and how many there are.
    plot: Rect,
    bars: usize,
    /// The heat map's squares: the first one's corner and the step between.
    heat_origin: (f32, f32),
    heat_step: (f32, f32),
    model_rows: Vec<Rect>,
    agent_rows: Vec<Rect>,
}

/// What a layout depends on besides the window.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sizes {
    /// Days covered by the report.
    pub days: usize,
    pub models: usize,
    pub agents: usize,
    /// Characters in the workspace's name.
    pub workspace_chars: usize,
    /// The open workspace menu: number of rows and the first one shown.
    pub menu: Option<(usize, usize)>,
}

impl Sizes {
    pub fn of(view: &View) -> Sizes {
        Sizes {
            days: view.report.map_or(0, |report| report.days.len()),
            models: view.report.map_or(0, |report| report.models.len()),
            agents: view.report.map_or(0, |report| report.agents.len()),
            workspace_chars: view.workspace.chars().count(),
            menu: view.menu.map(|(_, first)| (view.rows.len(), first)),
        }
    }
}

impl Layout {
    /// `top` is the height of the window's title bar, which the screen
    /// leaves clear.
    pub fn new(window: (f32, f32), cell: (f32, f32), scale: f32, top: f32, sizes: Sizes) -> Layout {
        let px = |points: f32| (points * scale).round();
        let pad = px(24.0);
        let gap = px(16.0);
        let width = (window.0 - pad * 2.0).max(cell.0 * 40.0);
        let right = pad + width;
        let button = (cell.1 + px(10.0)).round();
        let row = (cell.1 + px(8.0)).round();
        let heading = (cell.1 + px(12.0)).round();

        // The header: title on the left, controls on the right.
        let mut y = top + px(10.0);
        let header = cell.1 * 2.0 + px(4.0);
        let title = (pad, y);
        let button_y = y + ((header - button) / 2.0).round();
        let wide = |chars: usize| (chars as f32 * cell.0 + px(20.0)).round();
        let close = Rect::new(right - wide(3), button_y, wide(3), button);
        let mut ranges = [Rect::default(); 4];
        let mut x = close.x - gap;
        for (index, (label, _)) in RANGES.iter().enumerate().rev() {
            let w = wide(label.chars().count());
            x -= w;
            ranges[index] = Rect::new(x, button_y, w, button);
        }
        let w = wide(sizes.workspace_chars.min(WORKSPACE_CHARS) + 2);
        let workspace = Rect::new(ranges[0].x - gap - w, button_y, w, button);
        y += header + gap;

        // The four totals.
        let inner = px(14.0);
        let tile_h = (inner * 2.0 + cell.1 * 4.5 + px(14.0)).round();
        let tile_w = ((width - gap * 3.0) / 4.0).floor();
        let tiles = std::array::from_fn(|i| Rect::new(pad + (tile_w + gap) * i as f32, y, tile_w, tile_h));
        y += tile_h + (gap * 1.5).round();

        // Two rows of two sections share what is left.
        let rest = (window.1 - pad - y).max(cell.1 * 12.0);
        let between = (gap * 1.5).round();
        let middle_h = (rest * 0.42).clamp(cell.1 * 7.0, cell.1 * 16.0).round();
        let bottom_h = (rest - middle_h - between).max(cell.1 * 4.0);
        let column_gap = gap * 2.0;
        let chart_w = ((width - column_gap) * 0.58).round();
        let heat_x = pad + chart_w + column_gap;
        let half = ((width - column_gap) / 2.0).floor();
        let section = |x: f32, y: f32, w: f32, h: f32| {
            (Rect::new(x, y, w, heading), Rect::new(x, y + heading, w, (h - heading).max(0.0)))
        };
        let chart = section(pad, y, chart_w, middle_h);
        let heat = section(heat_x, y, right - heat_x, middle_h);
        let bottom = y + middle_h + between;
        let models = section(pad, bottom, half, bottom_h);
        let agents = section(right - half, bottom, half, bottom_h);

        // The chart: amounts on the left, dates below.
        let labels = cell.0 * 9.0;
        let plot = Rect::new(
            chart.1.x + labels,
            chart.1.y,
            (chart.1.w - labels).max(1.0),
            (chart.1.h - cell.1 - px(6.0)).max(1.0),
        );
        let bars = bar_count(sizes.days, plot.w, scale);

        // The heat map: weekdays on the left, hours below.
        let weekday = (cell.0 * 2.5).round();
        let step_x = ((heat.1.w - weekday) / 24.0).floor().max(2.0);
        let grid_h = heat.1.h - cell.1 - px(6.0) - if heat.1.h >= cell.1 * 10.0 { cell.1 } else { 0.0 };
        let step_y = (grid_h / 7.0).floor().clamp(2.0, step_x.min(cell.1 * 1.4));
        let heat_origin = (heat.1.x + weekday, heat.1.y);

        // The tables. Models have a share bar and its legend above theirs.
        let models_top = models.1.y + px(6.0) + px(10.0) + cell.1 + px(8.0) + cell.1 + px(4.0);
        let fit = |top: f32, body: Rect| (((body.y + body.h - top) / row).floor().max(1.0)) as usize;
        let rows = |top: f32, body: Rect, count: usize| {
            (0..count.min(fit(top, body))).map(|i| Rect::new(body.x, top + row * i as f32, body.w, row)).collect()
        };
        let model_rows: Vec<Rect> = rows(models_top, models.1, sizes.models);
        let agents_top = agents.1.y + cell.1 + px(4.0);
        let agent_rows: Vec<Rect> = rows(agents_top, agents.1, sizes.agents);

        // The workspace menu hangs below its button, inside the window.
        let menu = sizes.menu.map(|(count, first)| {
            let w = (MENU_CHARS * cell.0).round().min(width).max(workspace.w);
            let x = workspace.x.min(right - w).max(pad);
            let top = workspace.y + workspace.h + px(4.0);
            let shown = count.saturating_sub(first).min(menu_capacity(window, cell, scale, top));
            let frame = Rect::new(x, top, w, shown as f32 * row + px(8.0));
            let rows = (0..shown).map(|i| (first + i, Rect::new(x, top + px(4.0) + row * i as f32, w, row))).collect();
            (frame, rows)
        });

        Layout {
            cell,
            scale,
            frame: Rect::new(pad, top, width, window.1 - top),
            title,
            close,
            ranges,
            workspace,
            menu,
            tiles,
            chart,
            heat,
            models,
            agents,
            plot,
            bars,
            heat_origin,
            heat_step: (step_x, step_y),
            model_rows,
            agent_rows,
        }
    }

    /// What is at a window position. An open menu takes everything.
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if let Some((_, rows)) = &self.menu {
            return rows.iter().find(|(_, rect)| rect.contains(x, y)).map(|(index, _)| Hit::MenuRow(*index));
        }
        if self.close.contains(x, y) {
            return Some(Hit::Close);
        }
        if self.workspace.contains(x, y) {
            return Some(Hit::Workspace);
        }
        if let Some(index) = self.ranges.iter().position(|rect| rect.contains(x, y)) {
            return Some(Hit::Range(index));
        }
        if let Some(index) = self.agent_rows.iter().position(|rect| rect.contains(x, y)) {
            return Some(Hit::Agent(index));
        }
        if self.plot.contains(x, y) && self.bars > 0 {
            let bar = ((x - self.plot.x) / self.plot.w * self.bars as f32) as usize;
            return Some(Hit::Bar(bar.min(self.bars - 1)));
        }
        let (column, line) = ((x - self.heat_origin.0) / self.heat_step.0, (y - self.heat_origin.1) / self.heat_step.1);
        if (0.0..24.0).contains(&column) && (0.0..7.0).contains(&line) {
            return Some(Hit::Hour(line as usize, column as usize));
        }
        None
    }

    /// How many rows the workspace menu shows at once.
    pub fn menu_room(&self, window: (f32, f32)) -> usize {
        let top = self.workspace.y + self.workspace.h + (4.0 * self.scale).round();
        menu_capacity(window, self.cell, self.scale, top)
    }

    /// The square of the heat map for a weekday and hour.
    fn hour_rect(&self, weekday: usize, hour: usize) -> Rect {
        let space = (2.0 * self.scale).round();
        Rect::new(
            self.heat_origin.0 + self.heat_step.0 * hour as f32,
            self.heat_origin.1 + self.heat_step.1 * weekday as f32,
            self.heat_step.0 - space,
            self.heat_step.1 - space,
        )
    }
}

/// How many rows of the workspace menu fit below `top`.
pub fn menu_capacity(window: (f32, f32), cell: (f32, f32), scale: f32, top: f32) -> usize {
    let row = (cell.1 + (8.0 * scale).round()).round();
    (((window.1 - top - 24.0 * scale) / row).floor() as usize).clamp(3, 16)
}

/// Where a menu should start so that its highlighted row shows.
pub fn menu_scroll(selected: usize, first: usize, capacity: usize) -> usize {
    if selected < first {
        selected
    } else if selected >= first + capacity {
        selected + 1 - capacity
    } else {
        first
    }
}

/// How many bars show `days` days in `width` pixels: one a day while they
/// fit, otherwise several days to a bar.
fn bar_count(days: usize, width: f32, scale: f32) -> usize {
    days.min((width / (3.0 * scale)).floor() as usize).max(1)
}

/// The days bar `bar` of `bars` stands for.
fn bar_days(bar: usize, bars: usize, days: usize) -> std::ops::Range<usize> {
    bar * days / bars..((bar + 1) * days / bars).max(bar * days / bars + 1).min(days)
}

/// `days` added up into `bars` bars.
fn summed(days: &[Totals], bars: usize, value: impl Fn(&Totals) -> u64) -> Vec<u64> {
    if days.is_empty() {
        return Vec::new();
    }
    (0..bars).map(|bar| days[bar_days(bar, bars, days.len())].iter().map(&value).sum()).collect()
}

// ---- Words and numbers ------------------------------------------------------

/// A count in at most three digits and a unit: 22.9K, 963K, 61.3M, 10B.
pub fn compact(count: u64) -> String {
    const UNITS: [(f64, &str); 4] = [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")];
    if count < 1000 {
        return count.to_string();
    }
    let count = count as f64;
    for (index, (unit, name)) in UNITS.iter().enumerate().rev() {
        let value = count / unit;
        // Rounding 999.96K up belongs to the next unit.
        if value >= 999.5 && index > 0 {
            continue;
        }
        let text = if value < 99.95 { format!("{value:.1}") } else { format!("{value:.0}") };
        return format!("{}{name}", text.strip_suffix(".0").unwrap_or(&text));
    }
    unreachable!("the largest unit takes whatever is left")
}

/// A count with thousands separated: 2,674.
pub fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut text = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// A length of time: 347 h, 9.2 h, 23 min.
pub fn duration(millis: u64) -> String {
    let minutes = millis as f64 / 60_000.0;
    if millis == 0 {
        "—".into()
    } else if minutes < 1.0 {
        "<1 min".into()
    } else if minutes < 59.5 {
        format!("{minutes:.0} min")
    } else if minutes < 599.7 {
        format!("{:.1} h", minutes / 60.0)
    } else {
        format!("{:.0} h", minutes / 60.0)
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// A day counted from 1970, as "Oct 23"; with the year when `year` is set.
pub fn day_name(day: i64, year: bool) -> String {
    let (y, month, date) = date_from_days(day);
    let month = MONTHS[(month as usize - 1).min(11)];
    if year { format!("{month} {date}, {y}") } else { format!("{month} {date}") }
}

/// How a total changed against the one before, as "+12%"; nothing when
/// there is nothing to compare with.
pub fn change(now: u64, before: u64) -> Option<(String, bool)> {
    if before == 0 {
        return None;
    }
    let percent = ((now as f64 - before as f64) / before as f64 * 100.0).round();
    let shown = percent.abs().min(999.0);
    Some((format!("{}{shown:.0}%", if percent < 0.0 { "−" } else { "+" }), percent >= 0.0))
}

/// A model's name as people say it: "claude-opus-5-5" is "Opus 5.5".
/// Names of other makers' models are shown as they are.
pub fn model_name(model: &str) -> String {
    let Some(rest) = model.strip_prefix("claude-") else { return model.to_owned() };
    let mut family = String::new();
    let mut version = Vec::new();
    for part in rest.split('-') {
        if part.chars().all(|c| c.is_ascii_digit()) {
            // A longer number is a release date.
            if part.len() <= 2 {
                version.push(part);
            }
        } else if family.is_empty() {
            let mut letters = part.chars();
            family = letters.next().map(|first| first.to_uppercase().chain(letters).collect()).unwrap_or_default();
        }
    }
    if family.is_empty() {
        return model.to_owned();
    }
    format!("{family} {}", version.join(".")).trim_end().to_owned()
}

/// Who makes a model, going by its name.
pub fn maker(model: &str) -> &'static str {
    let model = model.to_ascii_lowercase();
    let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| model.starts_with(prefix));
    if starts(&["claude"]) {
        "Anthropic"
    } else if starts(&["gpt", "o1", "o3", "o4", "codex", "chatgpt"]) {
        "OpenAI"
    } else if starts(&["gemini"]) {
        "Google"
    } else if starts(&["grok"]) {
        "xAI"
    } else {
        "Other"
    }
}

fn maker_color(maker: &str, palette: &Palette) -> Rgb {
    match maker {
        "Anthropic" => palette.accent,
        "OpenAI" => palette.ansi[6],
        "Google" => palette.ansi[4],
        "xAI" => palette.ansi[5],
        _ => palette.chrome_text_dim,
    }
}

// ---- Drawing ----------------------------------------------------------------

struct Pen<'a> {
    r: &'a mut Renderer,
    cell: (f32, f32),
    scale: f32,
    line: f32,
    fg: Color,
    dim: Color,
    border: Color,
    palette: &'a Palette,
}

impl Pen<'_> {
    fn px(&self, points: f32) -> f32 {
        (points * self.scale).round()
    }

    fn fill(&mut self, rect: Rect, fill: Color) {
        self.r.rect(rect.x, rect.y, rect.w, rect.h, fill);
    }

    /// The foreground color, seen through: for marks that should sit back.
    fn faint(&self, alpha: u8) -> Color {
        let [r, g, b, _] = self.fg;
        [r, g, b, alpha]
    }

    fn outline(&mut self, rect: Rect, fill: Color) {
        let line = self.line;
        self.r.rect(rect.x - line, rect.y - line, rect.w + line * 2.0, rect.h + line * 2.0, self.border);
        self.fill(rect, fill);
    }

    fn left(&mut self, text: &str, bold: bool, x: f32, y: f32, width: f32, fill: Color) -> f32 {
        self.r.text(text, bold, x, y, width.max(0.0), fill)
    }

    /// Text ending at `right`.
    fn right(&mut self, text: &str, bold: bool, right: f32, y: f32, fill: Color) {
        let width = self.r.text_width(text);
        self.r.text(text, bold, right - width, y, width, fill);
    }

    /// Text centered in a rectangle.
    fn centered(&mut self, text: &str, rect: Rect, fill: Color) {
        let width = self.r.text_width(text).min(rect.w);
        let y = rect.y + ((rect.h - self.cell.1) / 2.0).round();
        self.r.text(text, false, rect.x + ((rect.w - width) / 2.0).round(), y, width, fill);
    }

    /// A section's heading: its name, a rule, and a note at the right.
    fn heading(&mut self, rect: Rect, name: &str, note: &str) {
        let used = self.left(name, false, rect.x, rect.y, rect.w, self.dim);
        let note_width = self.r.text_width(note).min((rect.w - used - self.cell.0 * 2.0).max(0.0));
        let note_x = rect.x + rect.w - note_width;
        self.left(note, false, note_x, rect.y, note_width, self.dim);
        let from = rect.x + used + self.cell.0;
        let to = if note.is_empty() { rect.x + rect.w } else { note_x - self.cell.0 };
        if to > from {
            self.r.rect(from, rect.y + (self.cell.1 / 2.0).round(), to - from, self.line, self.border);
        }
    }

    /// A horizontal bar with rounded ends; a dot when it is very short.
    fn pill(&mut self, x: f32, middle: f32, width: f32, thickness: f32, fill: Color) {
        let radius = thickness / 2.0;
        if width <= thickness {
            dot(self.r, x + radius, middle, radius, fill);
            return;
        }
        dot(self.r, x + radius, middle, radius, fill);
        dot(self.r, x + width - radius, middle, radius, fill);
        self.r.rect(x + radius, (middle - radius).round(), width - thickness, thickness, fill);
    }
}

/// Draws the screen over the whole window.
pub fn draw(r: &mut Renderer, view: &View, window: (f32, f32), scale: f32, top: f32, palette: &Palette) {
    let metrics = r.metrics();
    let cell = (metrics.width, metrics.height);
    let layout = Layout::new(window, cell, scale, top, Sizes::of(view));
    let hover = layout.hit(view.pointer.0, view.pointer.1);
    let mut pen = Pen {
        r,
        cell,
        scale,
        line: scale.round().max(1.0),
        fg: color(palette.foreground),
        dim: color(palette.chrome_text_dim),
        border: color(palette.border),
        palette,
    };
    pen.fill(Rect::new(0.0, 0.0, window.0, window.1), color(palette.background));

    header(&mut pen, view, &layout, hover);
    let Some(report) = view.report else {
        let text = if view.scanning { "Reading the agents' records…" } else { "No usage has been recorded yet" };
        pen.centered(text, layout.frame, pen.dim);
        return;
    };
    tiles(&mut pen, report, &layout);
    chart(&mut pen, report, &layout, hover);
    heat(&mut pen, report, &layout, hover);
    models(&mut pen, report, &layout);
    agents(&mut pen, view, report, &layout, hover);
    menu(&mut pen, view, &layout, hover);
}

fn header(pen: &mut Pen, view: &View, layout: &Layout, hover: Option<Hit>) {
    let (x, y) = layout.title;
    let room = layout.workspace.x - x - pen.cell.0;
    pen.left("Agent usage", true, x, y, room, pen.fg);
    let mut dates = match view.report {
        Some(report) if report.first_day < report.today => {
            // Across a year's end, say which year it started in.
            let years = date_from_days(report.first_day).0 != date_from_days(report.today).0;
            format!("{} – today", day_name(report.first_day, years))
        }
        Some(_) => "Today".to_owned(),
        None => String::new(),
    };
    if let Some(agent) = view.agent {
        let name = AgentKind::from_slug(agent).map_or(agent, |kind| kind.name());
        dates.push_str(&format!(" · {name} only"));
    }
    if view.scanning && view.report.is_some() {
        dates.push_str(" · updating…");
    }
    pen.left(&dates, false, x, y + pen.cell.1 + pen.px(4.0), room, pen.dim);

    let chrome = color(pen.palette.chrome);
    let active = color(pen.palette.chrome_active);
    // The periods, as one control with the chosen one raised.
    let first = layout.ranges[0];
    let last = layout.ranges[3];
    pen.outline(Rect::new(first.x, first.y, last.x + last.w - first.x, first.h), chrome);
    for (index, (label, _)) in RANGES.iter().enumerate() {
        let rect = layout.ranges[index];
        let chosen = index == view.range;
        if chosen || hover == Some(Hit::Range(index)) {
            pen.fill(rect, if chosen { color(pen.palette.selection) } else { active });
        }
        pen.centered(label, rect, if chosen { pen.fg } else { pen.dim });
    }
    let open = view.menu.is_some();
    let over = open || hover == Some(Hit::Workspace);
    pen.outline(layout.workspace, if over { active } else { chrome });
    let name: String = view.workspace.chars().take(WORKSPACE_CHARS).collect();
    pen.centered(&format!("{name} ▾"), layout.workspace, pen.fg);
    pen.outline(layout.close, if hover == Some(Hit::Close) { active } else { chrome });
    pen.centered("esc", layout.close, pen.dim);
}

fn tiles(pen: &mut Pen, report: &Report, layout: &Layout) {
    let total = report.total;
    let before = report.previous.unwrap_or_default();
    let cached = if total.input == 0 { 0 } else { (total.cache_read as f64 / total.input as f64 * 100.0).round() as u64 };
    let per_turn = total.output.checked_div(total.turns).unwrap_or(0);
    type Pick = fn(&Totals) -> u64;
    let content: [(&str, String, String, u64, u64, Pick); 4] = [
        (
            "INPUT TOKENS",
            compact(total.input),
            format!("{cached}% from cache"),
            total.input,
            before.input,
            |day| day.input,
        ),
        (
            "OUTPUT TOKENS",
            compact(total.output),
            format!("{} per turn", compact(per_turn)),
            total.output,
            before.output,
            |day| day.output,
        ),
        (
            "TASKS",
            grouped(total.tasks),
            format!("{} turns", grouped(total.turns)),
            total.tasks,
            before.tasks,
            |day| day.tasks,
        ),
        (
            "AGENT TIME",
            duration(total.millis),
            format!("up to {} in one hour", report.busiest_hour),
            total.millis,
            before.millis,
            |day| day.millis,
        ),
    ];
    let inner = pen.px(14.0);
    for (rect, (label, value, detail, now, earlier, pick)) in layout.tiles.iter().zip(content) {
        pen.outline(*rect, color(pen.palette.chrome));
        let (x, width) = (rect.x + inner, rect.w - inner * 2.0);
        let mut y = rect.y + inner;
        let mut room = width;
        if let Some((text, up)) = change(now, earlier) {
            let fill = color(if up { pen.palette.ansi[2] } else { pen.palette.ansi[1] });
            let text_width = pen.r.text_width(&text);
            if text_width + pen.cell.0 * 8.0 < width {
                pen.right(&text, false, rect.x + rect.w - inner, y, fill);
                room -= text_width + pen.cell.0;
            }
        }
        pen.left(label, false, x, y, room, pen.dim);
        y += pen.cell.1 + pen.px(4.0);
        pen.left(&value, true, x, y, width, pen.fg);
        y += pen.cell.1 + pen.px(2.0);
        pen.left(&detail, false, x, y, width, pen.dim);
        y += pen.cell.1 + pen.px(8.0);

        // The same total day by day, as a strip of small bars.
        let height = rect.y + rect.h - inner - y;
        let bars = bar_count(report.days.len(), width, pen.scale * 1.5);
        let values = summed(&report.days, bars, pick);
        let most = values.iter().copied().max().unwrap_or(0).max(1);
        let step = width / bars as f32;
        let bar = (step * 0.7).clamp(pen.line, pen.px(10.0));
        let fill = pen.faint(70);
        for (index, value) in values.iter().enumerate() {
            let tall = (*value as f32 / most as f32 * height).round().max(pen.line);
            let left = (x + step * index as f32 + (step - bar) / 2.0).round();
            pen.r.rect(left, y + height - tall, bar.round().max(1.0), tall, fill);
        }
    }
}

fn chart(pen: &mut Pen, report: &Report, layout: &Layout, hover: Option<Hit>) {
    let (heading, body) = layout.chart;
    let plot = layout.plot;
    let bars = layout.bars;
    let days = report.days.len();
    let input = summed(&report.days, bars, |day| day.input);
    let output = summed(&report.days, bars, |day| day.output);
    let hovered = match hover {
        Some(Hit::Bar(bar)) if bar < bars => Some(bar),
        _ => None,
    };
    let note = match hovered {
        Some(bar) => {
            let span = bar_days(bar, bars, days);
            let first = day_name(report.first_day + span.start as i64, false);
            let when = if span.len() > 1 {
                format!("{first} – {}", day_name(report.first_day + span.end as i64 - 1, false))
            } else {
                first
            };
            format!("{when} · {} in · {} out", compact(input[bar]), compact(output[bar]))
        }
        None => {
            let active = report.days.iter().filter(|day| day.input + day.output > 0).count();
            let each = (report.total.input + report.total.output) / days.max(1) as u64;
            format!("{active} of {days} days with agents · {}/day", compact(each))
        }
    };
    pen.heading(heading, "TOKENS BY DAY", &note);

    // Input grows up from the line and output down, each to its own scale.
    let most_in = input.iter().copied().max().unwrap_or(0);
    let most_out = output.iter().copied().max().unwrap_or(0);
    let up = (plot.h * 0.66).round();
    let down = plot.h - up - pen.line;
    let base = plot.y + up;
    let blue = color(pen.palette.ansi[4]);
    pen.left(&compact(most_in), false, body.x, plot.y, plot.x - body.x, pen.fg);
    pen.left("input", false, body.x, plot.y + pen.cell.1, plot.x - body.x, pen.dim);
    if down >= pen.cell.1 * 2.0 {
        let y = base + pen.px(4.0);
        pen.left(&compact(most_out), false, body.x, y, plot.x - body.x, pen.fg);
        pen.left("output", false, body.x, y + pen.cell.1, plot.x - body.x, blue);
    }
    pen.r.rect(plot.x, base, plot.w, pen.line, pen.border);
    let step = plot.w / bars as f32;
    let bar = (step * 0.7).clamp(pen.line, pen.px(22.0)).round().max(1.0);
    for index in 0..bars {
        let left = (plot.x + step * index as f32 + (step - bar) / 2.0).round();
        let lit = hovered == Some(index);
        if input[index] > 0 {
            let tall = (input[index] as f32 / most_in.max(1) as f32 * up).round().max(pen.line);
            let fill = pen.faint(if lit { 150 } else { 80 });
            pen.r.rect(left, base - tall, bar, tall, fill);
        }
        if output[index] > 0 {
            let tall = (output[index] as f32 / most_out.max(1) as f32 * down).round().max(pen.line);
            let [r, g, b, _] = blue;
            pen.r.rect(left, base + pen.line, bar, tall, [r, g, b, if lit { 255 } else { 215 }]);
        }
    }
    // Dates along the bottom.
    let y = plot.y + plot.h + pen.px(6.0);
    if days > 1 {
        pen.left(&day_name(report.first_day, false), false, plot.x, y, plot.w / 3.0, pen.dim);
        if plot.w > pen.cell.0 * 40.0 {
            let middle = day_name(report.first_day + days as i64 / 2, false);
            let width = pen.r.text_width(&middle);
            pen.left(&middle, false, plot.x + ((plot.w - width) / 2.0).round(), y, width, pen.dim);
        }
    }
    pen.right("today", false, plot.x + plot.w, y, pen.dim);
}

fn heat(pen: &mut Pen, report: &Report, layout: &Layout, hover: Option<Hit>) {
    let (heading, body) = layout.heat;
    let most = report.heat.iter().flatten().copied().max().unwrap_or(0);
    let note = match hover {
        Some(Hit::Hour(day, hour)) => format!("{} {hour} h · {}", WEEKDAYS[day], duration(report.heat[day][hour])),
        _ if most == 0 => String::new(),
        _ => {
            let (day, hour) = (0..7)
                .flat_map(|day| (0..24).map(move |hour| (day, hour)))
                .max_by_key(|(day, hour)| report.heat[*day][*hour])
                .unwrap_or((0, 0));
            format!("most active {} {hour} h", WEEKDAYS[day])
        }
    };
    pen.heading(heading, "WHEN THEY WORK", &note);

    // Five shades, from no work to the busiest hour.
    const SHADES: [u8; 5] = [16, 60, 110, 170, 235];
    let shade = |millis: u64| match millis {
        0 => SHADES[0],
        _ => SHADES[1 + ((millis as f64 / most.max(1) as f64 * 4.0).ceil() as usize).clamp(1, 4) - 1],
    };
    for day in 0..7 {
        let rect = layout.hour_rect(day, 0);
        let y = rect.y + ((rect.h - pen.cell.1) / 2.0).round();
        if day % 2 == 0 || rect.h + pen.px(2.0) >= pen.cell.1 {
            pen.left(&WEEKDAYS[day][..1], false, body.x, y, pen.cell.0 * 2.0, pen.dim);
        }
        for (hour, millis) in report.heat[day].iter().enumerate() {
            let rect = layout.hour_rect(day, hour);
            let fill = pen.faint(shade(*millis));
            pen.fill(rect, fill);
            if hover == Some(Hit::Hour(day, hour)) {
                pen.r.rect(rect.x, rect.y + rect.h - pen.line, rect.w, pen.line, color(pen.palette.accent));
            }
        }
    }
    let below = layout.heat_origin.1 + layout.heat_step.1 * 7.0 + pen.px(4.0);
    for hour in [0, 6, 12, 18] {
        let x = layout.hour_rect(0, hour).x;
        pen.left(&hour.to_string(), false, x, below, pen.cell.0 * 3.0, pen.dim);
    }
    // What the shades mean, where there is room for it.
    let key_y = below + pen.cell.1 + pen.px(2.0);
    if key_y + pen.cell.1 <= body.y + body.h {
        let right = body.x + body.w;
        pen.right("more", false, right, key_y, pen.dim);
        let side = (pen.cell.1 * 0.6).round();
        let mut x = right - pen.cell.0 * 5.0 - (side + pen.px(2.0)) * 4.0;
        pen.right("less", false, x - pen.cell.0, key_y, pen.dim);
        for alpha in &SHADES[1..] {
            let fill = pen.faint(*alpha);
            pen.r.rect(x, key_y + ((pen.cell.1 - side) / 2.0).round(), side, side, fill);
            x += side + pen.px(2.0);
        }
    }
}

/// The columns of a table: where names end, where the bar runs, and the
/// right edges of the three columns of numbers.
fn columns(body: Rect, cell: (f32, f32)) -> (f32, (f32, f32), [f32; 3]) {
    let number = NUMBER_CHARS * cell.0;
    let right = body.x + body.w;
    let numbers = [right - number * 2.0, right - number, right];
    let name = (body.w * 0.36).round();
    let bar = (body.x + name, (numbers[0] - number - cell.0 - (body.x + name)).max(0.0));
    (name, bar, numbers)
}

fn models(pen: &mut Pen, report: &Report, layout: &Layout) {
    let (heading, body) = layout.models;
    let count = report.models.len();
    pen.heading(heading, "MODELS", &format!("{count} model{}", if count == 1 { "" } else { "s" }));
    if count == 0 {
        pen.left("No usage in this period", false, body.x, body.y + pen.px(6.0), body.w, pen.dim);
        return;
    }
    // Each maker's share of the tokens, as one bar and its legend.
    let weight = |model: &ale_agents::usage::ModelUse| model.input + model.output;
    let all: u64 = report.models.iter().map(weight).sum::<u64>().max(1);
    let mut makers: Vec<(&str, u64)> = Vec::new();
    for model in &report.models {
        let name = maker(&model.model);
        match makers.iter_mut().find(|(maker, _)| *maker == name) {
            Some((_, tokens)) => *tokens += weight(model),
            None => makers.push((name, weight(model))),
        }
    }
    makers.sort_by_key(|(_, tokens)| std::cmp::Reverse(*tokens));
    let thick = pen.px(6.0);
    let mut x = body.x;
    let bar_y = body.y + pen.px(6.0);
    let legend_y = bar_y + thick + pen.px(10.0) - pen.px(6.0) + pen.px(4.0);
    let mut legend_x = body.x;
    let space = pen.px(3.0);
    for (name, tokens) in &makers {
        let fill = color(maker_color(name, pen.palette));
        let width = ((body.w - space * (makers.len() - 1) as f32) * *tokens as f32 / all as f32).max(thick);
        let width = width.min(body.x + body.w - x);
        if width >= 1.0 {
            pen.pill(x, bar_y + thick / 2.0, width, thick, fill);
        }
        x += width + space;
        let share = (*tokens as f64 / all as f64 * 100.0).round();
        let text = format!("{name} {share:.0}%");
        if legend_x + pen.r.text_width(&text) + pen.cell.0 * 2.0 <= body.x + body.w {
            dot(pen.r, legend_x + pen.cell.0 / 2.0, legend_y + pen.cell.1 / 2.0, pen.px(4.0), fill);
            legend_x += pen.cell.0 * 1.5;
            legend_x += pen.left(&text, false, legend_x, legend_y, body.w, pen.dim) + pen.cell.0 * 2.0;
        }
    }

    let (name_width, (bar_x, bar_width), numbers) = columns(body, pen.cell);
    let Some(first) = layout.model_rows.first() else { return };
    let titles_y = first.y - pen.cell.1 - pen.px(4.0);
    for (title, right) in ["input", "output", "tasks"].iter().zip(numbers) {
        pen.right(title, false, right, titles_y, pen.dim);
    }
    // When they do not all fit, the last row stands for the rest.
    let shown = layout.model_rows.len();
    let listed = if count > shown { shown.saturating_sub(1) } else { shown };
    let most = report.models.first().map(weight).unwrap_or(1).max(1);
    for (index, rect) in layout.model_rows.iter().enumerate() {
        let y = rect.y + ((rect.h - pen.cell.1) / 2.0).round();
        let middle = rect.y + rect.h / 2.0;
        let (name, fill, text, tokens, input, output, tasks) = if index < listed {
            let model = &report.models[index];
            let fill = color(maker_color(maker(&model.model), pen.palette));
            (model_name(&model.model), fill, pen.fg, weight(model), model.input, model.output, model.tasks)
        } else {
            let rest = &report.models[listed..];
            let sum = |value: fn(&ale_agents::usage::ModelUse) -> u64| rest.iter().map(value).sum::<u64>();
            let tokens = sum(|model| model.input + model.output);
            (format!("{} more", rest.len()), pen.dim, pen.dim, tokens, sum(|m| m.input), sum(|m| m.output), sum(|m| m.tasks))
        };
        if index < listed {
            dot(pen.r, rect.x + pen.cell.0 / 2.0, middle, pen.px(4.0), fill);
        }
        pen.left(&name, false, rect.x + pen.cell.0 * 2.0, y, name_width - pen.cell.0 * 3.0, text);
        let width = (bar_width * tokens as f32 / most as f32).max(pen.px(2.0));
        pen.pill(bar_x, middle, width.min(bar_width), pen.px(6.0), fill);
        let faded = if index < listed { pen.fg } else { pen.dim };
        pen.right(&compact(input), false, numbers[0], y, faded);
        pen.right(&compact(output), false, numbers[1], y, faded);
        pen.right(&grouped(tasks), false, numbers[2], y, pen.dim);
    }
}

fn agents(pen: &mut Pen, view: &View, report: &Report, layout: &Layout, hover: Option<Hit>) {
    let (heading, body) = layout.agents;
    let note = match view.agent {
        Some(_) => "click it again to see every agent",
        None if report.agents.len() > 1 => "click one to see only its usage",
        None => "",
    };
    pen.heading(heading, "AGENTS", note);
    if report.agents.is_empty() {
        pen.left("No usage in this period", false, body.x, body.y + pen.px(6.0), body.w, pen.dim);
        return;
    }
    let (name_width, (bar_x, bar_width), numbers) = columns(body, pen.cell);
    for (title, right) in ["input", "output", "time"].iter().zip(numbers) {
        pen.right(title, false, right, body.y, pen.dim);
    }
    let most = report.agents.iter().map(|agent| agent.input + agent.output).max().unwrap_or(1).max(1);
    for (index, rect) in layout.agent_rows.iter().enumerate() {
        let agent = &report.agents[index];
        let kind = AgentKind::from_slug(&agent.agent);
        let chosen = view.agent == Some(agent.agent.as_str());
        // With one agent picked, the others step back.
        let text = if view.agent.is_some() && !chosen { pen.dim } else { pen.fg };
        if chosen {
            pen.fill(*rect, color(pen.palette.selection));
        } else if hover == Some(Hit::Agent(index)) {
            pen.fill(*rect, color(pen.palette.chrome_active));
        }
        let y = rect.y + ((rect.h - pen.cell.1) / 2.0).round();
        let side = pen.cell.1.round();
        if let Some(kind) = kind {
            pen.r.image(crate::icons::key(kind), rect.x + pen.px(4.0), y, side, side, [0.0, 0.0, 1.0, 1.0]);
        }
        let name_x = rect.x + pen.px(4.0) + side + pen.cell.0;
        let name = kind.map_or(agent.agent.as_str(), |kind| kind.name());
        pen.left(name, false, name_x, y, body.x + name_width - name_x - pen.cell.0, text);
        let tokens = agent.input + agent.output;
        let width = (bar_width * tokens as f32 / most as f32).max(pen.px(2.0));
        // Opaque: a bar's round ends overlap its middle.
        let fill = if view.agent.is_some() && !chosen { pen.border } else { pen.dim };
        pen.pill(bar_x, rect.y + rect.h / 2.0, width.min(bar_width), pen.px(6.0), fill);
        let number = |count: u64| if count == 0 { "—".to_owned() } else { compact(count) };
        pen.right(&number(agent.input), false, numbers[0], y, text);
        pen.right(&number(agent.output), false, numbers[1], y, text);
        pen.right(&duration(agent.millis), false, numbers[2] - pen.px(4.0), y, pen.dim);
    }
}

fn menu(pen: &mut Pen, view: &View, layout: &Layout, hover: Option<Hit>) {
    let (Some((frame, rows)), Some((selected, _))) = (&layout.menu, view.menu) else { return };
    pen.outline(*frame, color(pen.palette.chrome));
    let pad = pen.px(10.0);
    for (index, rect) in rows {
        let Some(row) = view.rows.get(*index) else { continue };
        if *index == selected || hover == Some(Hit::MenuRow(*index)) {
            pen.fill(*rect, color(pen.palette.selection));
        }
        let y = rect.y + ((rect.h - pen.cell.1) / 2.0).round();
        if row.checked {
            pen.left("✓", false, rect.x + pad, y, pen.cell.0 * 2.0, color(pen.palette.accent));
        }
        let x = rect.x + pad + pen.cell.0 * 2.0;
        let room = rect.x + rect.w - pad - x;
        let used = pen.left(&row.label, false, x, y, room, pen.fg);
        // The folder, as far as it fits, keeping its end.
        let space = ((room - used - pen.cell.0 * 2.0) / pen.cell.0).floor().max(0.0) as usize;
        let length = row.detail.chars().count();
        if space >= 6 && length > 0 {
            let detail: String = if length > space {
                std::iter::once('…').chain(row.detail.chars().skip(length - space + 1)).collect()
            } else {
                row.detail.clone()
            };
            pen.right(&detail, false, rect.x + rect.w - pad, y, pen.dim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: (f32, f32) = (1400.0, 900.0);
    const CELL: (f32, f32) = (8.0, 16.0);

    #[test]
    fn numbers_read_like_the_ones_people_write() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1000), "1K");
        assert_eq!(compact(22_940), "22.9K");
        assert_eq!(compact(963_400), "963K");
        assert_eq!(compact(999_960), "1M");
        assert_eq!(compact(61_300_000), "61.3M");
        assert_eq!(compact(10_000_000_000), "10B");
        assert_eq!(compact(11_600_000_000), "11.6B");
        assert_eq!(grouped(7), "7");
        assert_eq!(grouped(2674), "2,674");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(duration(0), "—");
        assert_eq!(duration(20_000), "<1 min");
        assert_eq!(duration(23 * 60_000), "23 min");
        assert_eq!(duration(552 * 60_000), "9.2 h");
        assert_eq!(duration(347 * 3_600_000), "347 h");
        assert_eq!(day_name(20_777, false), "Nov 20");
        assert_eq!(day_name(0, true), "Jan 1, 1970");
    }

    #[test]
    fn changes_are_shown_only_when_there_is_something_to_compare() {
        assert_eq!(change(150, 100), Some(("+50%".into(), true)));
        assert_eq!(change(50, 100), Some(("−50%".into(), false)));
        assert_eq!(change(100_000, 1), Some(("+999%".into(), true)));
        assert_eq!(change(5, 0), None);
    }

    #[test]
    fn models_are_named_and_grouped_by_maker() {
        assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_name("claude-fable-5-2"), "Fable 5.2");
        assert_eq!(model_name("claude-3-5-sonnet-20241022"), "Sonnet 3.5");
        assert_eq!(model_name("claude-haiku-5-5-20260101"), "Haiku 5.5");
        assert_eq!(model_name("gpt-6.1-sol"), "gpt-6.1-sol");
        assert_eq!(maker("claude-opus-5-5"), "Anthropic");
        assert_eq!(maker("gpt-6.2-astra"), "OpenAI");
        assert_eq!(maker("gemini-4-argon-high"), "Google");
        assert_eq!(maker("grok-4.7-build"), "xAI");
        assert_eq!(maker("llama-9"), "Other");
    }

    #[test]
    fn many_days_share_bars_and_none_is_left_out() {
        assert_eq!(bar_count(30, 600.0, 1.0), 30);
        assert_eq!(bar_count(0, 600.0, 1.0), 1);
        let bars = bar_count(1000, 600.0, 1.0);
        assert_eq!(bars, 200);
        let covered: usize = (0..bars).map(|bar| bar_days(bar, bars, 1000).len()).sum();
        assert_eq!(covered, 1000);
        assert_eq!(bar_days(29, 30, 30), 29..30);
        let days: Vec<Totals> = (0..10).map(|i| Totals { output: i, ..Totals::default() }).collect();
        assert_eq!(summed(&days, 5, |day| day.output), [1, 5, 9, 13, 17]);
        assert_eq!(summed(&[], 1, |day| day.output), Vec::<u64>::new());
    }

    #[test]
    fn controls_and_rows_are_where_clicks_find_them() {
        let sizes = Sizes { days: 30, models: 40, agents: 3, workspace_chars: 14, menu: None };
        let layout = Layout::new(WINDOW, CELL, 1.0, 40.0, sizes);
        let at = |rect: Rect| layout.hit(rect.center().0, rect.center().1);
        assert_eq!(at(layout.close), Some(Hit::Close));
        assert_eq!(at(layout.workspace), Some(Hit::Workspace));
        for (index, range) in layout.ranges.iter().enumerate() {
            assert_eq!(at(*range), Some(Hit::Range(index)));
        }
        // The controls sit side by side, below the title bar, in the window.
        assert!(layout.workspace.x + layout.workspace.w <= layout.ranges[0].x);
        assert!(layout.ranges[3].x + layout.ranges[3].w <= layout.close.x);
        assert!(layout.close.y >= 40.0 && layout.close.x + layout.close.w <= WINDOW.0);
        assert_eq!(layout.agent_rows.len(), 3);
        assert_eq!(at(layout.agent_rows[2]), Some(Hit::Agent(2)));
        // More models than fit: the rows stop at the window's edge.
        let last = layout.model_rows.last().unwrap();
        assert!(layout.model_rows.len() < 40 && last.y + last.h <= WINDOW.1);
        assert_eq!(layout.bars, 30);
        assert_eq!(layout.hit(layout.plot.x + 1.0, layout.plot.y + 1.0), Some(Hit::Bar(0)));
        assert_eq!(layout.hit(layout.plot.x + layout.plot.w - 1.0, layout.plot.y + 1.0), Some(Hit::Bar(29)));
        assert_eq!(at(layout.hour_rect(6, 23)), Some(Hit::Hour(6, 23)));
        assert_eq!(at(layout.hour_rect(0, 0)), Some(Hit::Hour(0, 0)));
        assert_eq!(layout.hit(2.0, 2.0), None);
        assert!(!Hit::Bar(0).clickable() && Hit::Agent(0).clickable());
    }

    #[test]
    fn the_open_menu_takes_the_pointer_and_scrolls_to_its_highlight() {
        let sizes = Sizes { days: 30, models: 2, agents: 2, workspace_chars: 14, menu: Some((40, 5)) };
        let layout = Layout::new(WINDOW, CELL, 1.0, 40.0, sizes);
        let (frame, rows) = layout.menu.as_ref().unwrap();
        assert!(frame.y >= layout.workspace.y + layout.workspace.h);
        assert!(frame.x + frame.w <= WINDOW.0 && frame.y + frame.h <= WINDOW.1);
        assert_eq!(rows.len(), menu_capacity(WINDOW, CELL, 1.0, frame.y));
        assert_eq!(rows[0].0, 5);
        let (x, y) = rows[2].1.center();
        assert_eq!(layout.hit(x, y), Some(Hit::MenuRow(7)));
        // Nothing behind the menu answers while it is open.
        let (x, y) = layout.close.center();
        assert_eq!(layout.hit(x, y), None);

        assert_eq!(menu_scroll(0, 5, 10), 0);
        assert_eq!(menu_scroll(7, 5, 10), 5);
        assert_eq!(menu_scroll(15, 5, 10), 6);
    }
}
