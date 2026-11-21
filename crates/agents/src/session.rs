//! What one agent is doing: its state, title, current activity and
//! subagents.
//!
//! A [`Session`] is updated from two sources. Agents with hooks installed
//! report precisely (see the `hooks_*` modules). Without hooks we fall back
//! to watching the pane: output means "working", silence means "idle", a
//! bell means "needs you". Once a session has received any hook message the
//! guesswork is switched off for it.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Claude,
    Codex,
    OpenCode,
    Gemini,
    Kiro,
}

impl AgentKind {
    pub const ALL: [AgentKind; 5] =
        [AgentKind::Claude, AgentKind::Codex, AgentKind::OpenCode, AgentKind::Gemini, AgentKind::Kiro];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::OpenCode => "OpenCode",
            AgentKind::Gemini => "Gemini",
            AgentKind::Kiro => "Kiro",
        }
    }

    /// Short identifier used in hook commands and files.
    pub fn slug(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::OpenCode => "opencode",
            AgentKind::Gemini => "gemini",
            AgentKind::Kiro => "kiro",
        }
    }

    pub fn from_slug(slug: &str) -> Option<AgentKind> {
        AgentKind::ALL.into_iter().find(|kind| kind.slug() == slug)
    }

    /// Recognizes an agent from the command line the user ran, e.g.
    /// `claude --resume`, `FOO=1 npx @openai/codex` or `/opt/bin/gemini`.
    pub fn from_command(command: &str) -> Option<AgentKind> {
        let mut words = command.split_whitespace().peekable();
        // Skip leading `VAR=value` assignments and common wrappers.
        while let Some(word) = words.peek() {
            let is_assignment = word.contains('=') && !word.starts_with('-');
            let is_wrapper = matches!(*word, "sudo" | "env" | "command" | "exec" | "time" | "caffeinate");
            if is_assignment || is_wrapper {
                words.next();
            } else {
                break;
            }
        }
        let first = words.next()?;
        let program = first.rsplit('/').next().unwrap_or(first);
        // Package runners: the agent is the package name that follows.
        if matches!(program, "npx" | "bunx" | "pnpx" | "uvx") || (program == "pnpm" && words.peek() == Some(&"dlx")) {
            let package = words.find(|word| !word.starts_with('-') && *word != "dlx")?;
            let package = package.split_once("@latest").map_or(package, |(name, _)| name);
            return match package {
                "@anthropic-ai/claude-code" => Some(AgentKind::Claude),
                "@openai/codex" => Some(AgentKind::Codex),
                "opencode-ai" | "opencode" => Some(AgentKind::OpenCode),
                "@google/gemini-cli" => Some(AgentKind::Gemini),
                _ => None,
            };
        }
        match program {
            "claude" => Some(AgentKind::Claude),
            "codex" => Some(AgentKind::Codex),
            "opencode" => Some(AgentKind::OpenCode),
            "gemini" => Some(AgentKind::Gemini),
            "kiro-cli" | "kiro" => Some(AgentKind::Kiro),
            // Amazon Q Developer CLI, Kiro's predecessor: `q` or `q chat`.
            "q" if matches!(words.next(), None | Some("chat")) => Some(AgentKind::Kiro),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentState {
    Working,
    /// Blocked on the user: a permission prompt or a question.
    NeedsYou,
    /// Finished its turn and the user has not looked yet.
    Done,
    /// Waiting for a prompt; nothing new to see.
    Idle,
    Error,
    RateLimited,
    /// The main agent stopped but its subagents are still running.
    Waiting,
}

impl AgentState {
    pub fn label(self) -> &'static str {
        match self {
            AgentState::Working => "Working",
            AgentState::NeedsYou => "Needs you",
            AgentState::Done => "Done",
            AgentState::Idle => "Idle",
            AgentState::Error => "Error",
            AgentState::RateLimited => "Rate limited",
            AgentState::Waiting => "Waiting on tasks",
        }
    }

    /// States that should draw the user's attention.
    pub fn wants_attention(self) -> bool {
        matches!(self, AgentState::NeedsYou | AgentState::Error | AgentState::RateLimited)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Subagent {
    pub id: String,
    /// The subagent's type, e.g. "Explore".
    pub kind: String,
    /// What it was asked to do, when known.
    pub description: String,
    /// The subagent that started it; `None` when the main agent did.
    pub parent: Option<String>,
    /// What it is doing right now, e.g. "Reading src/lib.rs".
    pub activity: Option<String>,
    /// Its final answer, once it has finished.
    pub result: Option<String>,
    /// Where its conversation is recorded.
    pub transcript: Option<PathBuf>,
    pub started: Instant,
    pub finished: Option<Instant>,
}

/// Something the agent left running in the background (a dev server, a
/// long test run).
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundTask {
    pub id: String,
    pub label: String,
    /// What it is, as the agent names it: "shell", "subagent", "workflow",
    /// "monitor"... Empty when not reported.
    pub kind: String,
}

impl BackgroundTask {
    /// Whether the agent is paused until this finishes. A shell command
    /// left running is as often a server that never ends as a job being
    /// waited for, so it does not count.
    pub fn awaited(&self) -> bool {
        !matches!(self.kind.as_str(), "shell" | "bash" | "local_bash")
    }
}

/// What happened to a session as the result of an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    None,
    /// Something visible changed (activity text, subagents, ...).
    Updated,
    /// The state changed; alerts are decided on this.
    State(AgentState),
    /// The agent's session is over.
    Ended,
}

/// How many of its latest actions a session remembers.
pub const RECENT_ACTIONS: usize = 6;

#[derive(Clone, Debug)]
pub struct Session {
    pub kind: AgentKind,
    pub state: AgentState,
    /// When the session was first noticed.
    pub started: Instant,
    /// When the state last changed.
    pub since: Instant,
    /// The task: the agent's own session title, or the first prompt.
    pub title: Option<String>,
    /// What it is doing right now, e.g. "Editing src/main.rs".
    pub activity: Option<String>,
    pub session_id: Option<String>,
    pub transcript: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub subagents: Vec<Subagent>,
    pub background_tasks: Vec<BackgroundTask>,
    /// Wake-ups the agent has scheduled for itself (a timer, a loop).
    pub scheduled_wakeups: usize,
    /// Subagent launches that have been requested but not yet reported as
    /// started: who asked, and for what. The start report itself carries
    /// neither, so they are matched up in order.
    pending_launches: VecDeque<(Option<String>, String)>,
    /// True once a hook message has arrived: the state is then reported,
    /// not guessed.
    pub hooked: bool,
    /// The main agent's latest actions ("Editing src/main.rs"), oldest
    /// first, with when each began; at most [`RECENT_ACTIONS`].
    pub recent: VecDeque<(Instant, String)>,
    last_output: Option<Instant>,
    /// Output before this moment is a reaction to something the user or
    /// the terminal did (typing, resizing, switching windows), not work.
    settle_until: Option<Instant>,
    /// The current run of closely spaced output: when it began and how
    /// many times the pane has printed in it.
    burst: Option<(Instant, u32)>,
}

/// Silence after which an agent without hooks is assumed to be idle.
const QUIET_AFTER: Duration = Duration::from_millis(2500);
/// After the user types, or the pane is resized or regains focus, programs
/// repaint. Output this soon afterwards is that, not work.
const SETTLE: Duration = Duration::from_millis(700);
/// Output counts as one continuous run while the gaps stay under this.
const BURST_GAP: Duration = Duration::from_millis(600);
/// A run must last this long, with at least this many updates, before it
/// is taken for an agent working. An agent at work animates a spinner
/// several times a second for as long as it works; an idle one only
/// repaints now and then (a clock, a usage meter), which must not count.
const BURST_DURATION: Duration = Duration::from_millis(1200);
const BURST_UPDATES: u32 = 6;
/// Most subagents remembered per session; the oldest finished ones go first.
const MAX_SUBAGENTS: usize = 64;

impl Session {
    pub fn new(kind: AgentKind, now: Instant) -> Session {
        Session {
            kind,
            state: AgentState::Idle,
            started: now,
            since: now,
            title: None,
            activity: None,
            session_id: None,
            transcript: None,
            cwd: None,
            subagents: Vec::new(),
            background_tasks: Vec::new(),
            scheduled_wakeups: 0,
            pending_launches: VecDeque::new(),
            hooked: false,
            recent: VecDeque::new(),
            last_output: None,
            settle_until: None,
            burst: None,
        }
    }

    /// Sets the state, returning the change to report.
    pub(crate) fn set_state(&mut self, state: AgentState, now: Instant) -> Change {
        if self.state == state {
            return Change::Updated;
        }
        self.state = state;
        self.since = now;
        Change::State(state)
    }

    /// Back to waiting for a prompt (e.g. the conversation was cleared).
    pub fn set_idle(&mut self, now: Instant) -> Change {
        self.set_state(AgentState::Idle, now)
    }

    pub fn running_subagents(&self) -> usize {
        self.subagents.iter().filter(|subagent| subagent.finished.is_none()).count()
    }

    /// The user looked at this agent: a finished turn is no longer news.
    pub fn mark_seen(&mut self, now: Instant) -> Change {
        if self.state == AgentState::Done { self.set_state(AgentState::Idle, now) } else { Change::None }
    }

    // ---- Guessing, for agents without hooks ---------------------------------

    /// The user typed into the pane.
    /// The user typed into the pane.
    pub fn on_input(&mut self, now: Instant) {
        self.on_disturbance(now);
    }

    /// Something made the program repaint: typing, a resize, the window
    /// gaining or losing focus. What it prints next is not work.
    pub fn on_disturbance(&mut self, now: Instant) {
        self.settle_until = Some(now + SETTLE);
        self.burst = None;
    }

    /// The pane printed something.
    ///
    /// Only sustained output counts as working; see `BURST_DURATION`.
    pub fn on_output(&mut self, now: Instant) -> Change {
        if self.hooked || self.settle_until.is_some_and(|until| now < until) {
            return Change::None;
        }
        let continues = self.last_output.is_some_and(|last| now.duration_since(last) <= BURST_GAP);
        let (began, updates) = match self.burst {
            Some((began, updates)) if continues => (began, updates + 1),
            _ => (now, 1),
        };
        self.burst = Some((began, updates));
        self.last_output = Some(now);

        let sustained = updates >= BURST_UPDATES && now.duration_since(began) >= BURST_DURATION;
        if sustained && self.state != AgentState::Working {
            return self.set_state(AgentState::Working, now);
        }
        Change::None
    }

    /// The pane rang the bell or asked for a notification: agents do that
    /// when they want the user.
    pub fn on_attention(&mut self, now: Instant) -> Change {
        if self.hooked {
            return Change::None;
        }
        self.last_output = None;
        self.burst = None;
        self.set_state(AgentState::NeedsYou, now)
    }

    /// Call periodically. Returns what changed, and when to call again (if
    /// there is anything to wait for).
    pub fn tick(&mut self, now: Instant) -> (Change, Option<Instant>) {
        let mut change = Change::None;
        let mut next: Option<Instant> = None;

        if !self.hooked
            && self.state == AgentState::Working
            && let Some(last) = self.last_output
        {
            let quiet_at = last + QUIET_AFTER;
            if now >= quiet_at {
                // We cannot tell "finished" from "waiting for you" by
                // silence alone, so call it done and let the user look.
                change = self.set_state(AgentState::Done, now);
            } else {
                next = Some(next.map_or(quiet_at, |n| n.min(quiet_at)));
            }
        }
        (change, next)
    }

    // ---- Helpers for the hook adapters ---------------------------------------

    /// Records fields that every hook message may carry.
    pub(crate) fn note_common(&mut self, payload: &Value) {
        self.hooked = true;
        if let Some(id) = text(payload, &["session_id", "sessionId", "conversation_id"]) {
            self.session_id = Some(id);
        }
        if let Some(path) = text(payload, &["transcript_path", "transcriptPath"]) {
            self.transcript = Some(PathBuf::from(path));
        }
        if let Some(cwd) = text(payload, &["cwd", "working_directory"]) {
            self.cwd = Some(PathBuf::from(cwd));
        }
    }

    pub(crate) fn set_activity(&mut self, activity: impl Into<String>) {
        self.activity = Some(clean(&activity.into(), 120));
    }

    /// Remembers an action of the main agent for its summary. The same
    /// action twice in a row (reading a file in pieces) counts once.
    pub(crate) fn note_action(&mut self, action: &str, now: Instant) {
        let action = clean(action, 120);
        if self.recent.back().is_some_and(|(_, last)| *last == action) {
            return;
        }
        self.recent.push_back((now, action));
        while self.recent.len() > RECENT_ACTIONS {
            self.recent.pop_front();
        }
    }

    /// Uses `title` as the session title unless one is already set.
    pub(crate) fn suggest_title(&mut self, title: &str) {
        if self.title.is_none() {
            let title = clean(title, 80);
            if !title.is_empty() {
                self.title = Some(title);
            }
        }
    }

    /// A new turn begins: what the previous turn's subagents did is no
    /// longer current.
    pub(crate) fn new_turn(&mut self) {
        self.subagents.retain(|subagent| subagent.finished.is_none());
        self.pending_launches.clear();
    }

    /// Someone (the main agent, or the subagent `parent`) asked for a
    /// subagent to be launched.
    pub(crate) fn launch_requested(&mut self, parent: Option<String>, description: &str) {
        self.pending_launches.push_back((parent, clean(description, 120)));
        self.pending_launches.truncate(16);
    }

    pub(crate) fn subagent_started(&mut self, id: &str, kind: &str, now: Instant) {
        if self.subagents.iter().any(|subagent| subagent.id == id) {
            return;
        }
        let (parent, description) = self.pending_launches.pop_front().unwrap_or_default();
        // Only accept a parent we actually know about.
        let parent = parent.filter(|parent| self.subagents.iter().any(|s| &s.id == parent));
        // While it runs, its conversation is recorded next to the main one.
        let transcript = self.subagent_transcript(id);
        self.subagents.push(Subagent {
            id: clean(id, 80),
            kind: clean(kind, 40),
            description,
            parent,
            activity: None,
            result: None,
            transcript,
            started: now,
            finished: None,
        });
        if self.subagents.len() > MAX_SUBAGENTS
            && let Some(oldest) = self.subagents.iter().position(|s| s.finished.is_some())
        {
            self.subagents.remove(oldest);
        }
    }

    /// Where Claude Code records a subagent's conversation: in a folder
    /// named after the main transcript, `<session>/subagents/agent-<id>.jsonl`.
    fn subagent_transcript(&self, id: &str) -> Option<PathBuf> {
        let safe = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        let main = self.transcript.as_ref()?;
        safe.then(|| main.with_extension("").join("subagents").join(format!("agent-{id}.jsonl")))
    }

    pub(crate) fn subagent_activity(&mut self, id: &str, activity: String) {
        if let Some(subagent) = self.subagents.iter_mut().find(|subagent| subagent.id == id) {
            subagent.activity = Some(clean(&activity, 120));
        }
    }

    /// Records where a subagent's conversation is kept, as reported by the
    /// agent itself. This replaces the location we worked out.
    pub(crate) fn subagent_transcript_reported(&mut self, id: &str, path: &str) {
        if let Some(subagent) = self.subagents.iter_mut().find(|subagent| subagent.id == id) {
            let reported = PathBuf::from(path);
            if subagent.transcript.as_ref() != Some(&reported) {
                log::debug!("subagent transcript: expected {:?}, agent reports {reported:?}", subagent.transcript);
            }
            subagent.transcript = Some(reported);
        }
    }

    pub(crate) fn subagent_finished(&mut self, id: &str, result: Option<String>, now: Instant) {
        if let Some(subagent) = self.subagents.iter_mut().find(|subagent| subagent.id == id) {
            subagent.finished.get_or_insert(now);
            subagent.activity = None;
            subagent.result = result.map(|text| clean(&text, 200));
        }
    }

    /// Replaces the list of background tasks from a hook's report. The
    /// entries' exact shape is not documented, so the likely fields are
    /// tried and anything unrecognizable is skipped.
    pub(crate) fn note_background_tasks(&mut self, payload: &Value) {
        let Some(tasks) = payload.get("background_tasks").and_then(Value::as_array) else { return };
        self.background_tasks = tasks
            .iter()
            .filter_map(|task| {
                let label = text(task, &["description", "command", "name", "title", "summary"])
                    .or_else(|| task.as_str().map(str::to_owned))?;
                let id = text(task, &["id", "task_id", "shell_id", "bash_id"]).unwrap_or_else(|| label.clone());
                let kind = text(task, &["type", "task_type"]).unwrap_or_default();
                Some(BackgroundTask { id: clean(&id, 80), label: clean(&label, 120), kind: clean(&kind, 40) })
            })
            .take(32)
            .collect();
    }

    /// Notes the wake-ups the agent has scheduled, from a hook's report.
    pub(crate) fn note_scheduled_wakeups(&mut self, payload: &Value) {
        if let Some(crons) = payload.get("session_crons").and_then(Value::as_array) {
            self.scheduled_wakeups = crons.len();
        }
    }

    /// What a stopped agent is still waiting for, in words, or `None` when
    /// it is simply finished: subagents, other background work that will
    /// wake it, or a wake-up it scheduled.
    pub fn waiting_for(&self) -> Option<String> {
        let plural = |count: usize, what: &str| format!("{count} {what}{}", if count == 1 { "" } else { "s" });
        let subagents = self.running_subagents();
        // Subagents can also appear in the background list; count them once.
        let tasks: Vec<&BackgroundTask> =
            self.background_tasks.iter().filter(|task| task.awaited() && task.kind != "subagent").collect();
        let listed_subagents = self.background_tasks.iter().filter(|task| task.kind == "subagent").count();
        let subagents = subagents.max(listed_subagents);
        let mut parts = Vec::new();
        if subagents > 0 {
            parts.push(plural(subagents, "subagent"));
        }
        match tasks.as_slice() {
            [] => {}
            [one] => parts.push(if one.kind.is_empty() { one.label.clone() } else { format!("{}: {}", one.kind, one.label) }),
            many => parts.push(plural(many.len(), "background task")),
        }
        if parts.is_empty() {
            return (self.scheduled_wakeups > 0).then(|| "Paused until its scheduled wake-up".to_owned());
        }
        Some(format!("Waiting on {}", parts.join(" and ")))
    }
}

/// The first of `keys` that holds a non-empty string.
pub(crate) fn text(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key)?.as_str())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Makes text from an agent safe and tidy to show: one line, no control
/// characters, at most `max` characters.
pub fn clean(input: &str, max: usize) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for c in input.chars() {
        if c.is_whitespace() || c.is_control() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
        if out.chars().count() >= max {
            out.push('…');
            break;
        }
    }
    out
}

/// A path as short as is still useful: relative to `cwd` when inside it,
/// otherwise its last two components.
pub(crate) fn short_path(path: &str, cwd: Option<&str>) -> String {
    if let Some(cwd) = cwd
        && let Some(rest) = path.strip_prefix(cwd)
        && let Some(rest) = rest.strip_prefix('/')
        && rest.len() <= 28
    {
        return rest.to_owned();
    }
    let parts: Vec<&str> = path.rsplit('/').take(2).collect();
    match parts.as_slice() {
        [file, parent] if !parent.is_empty() => format!("{parent}/{file}"),
        [file, ..] => (*file).to_owned(),
        [] => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_agents_from_command_lines() {
        let kind = AgentKind::from_command;
        assert_eq!(kind("claude"), Some(AgentKind::Claude));
        assert_eq!(kind("claude --resume abc"), Some(AgentKind::Claude));
        assert_eq!(kind("/Users/me/.local/bin/claude -c"), Some(AgentKind::Claude));
        assert_eq!(kind("ANTHROPIC_MODEL=x claude"), Some(AgentKind::Claude));
        assert_eq!(kind("npx @anthropic-ai/claude-code@latest"), Some(AgentKind::Claude));
        assert_eq!(kind("npx -y @openai/codex"), Some(AgentKind::Codex));
        assert_eq!(kind("pnpm dlx @google/gemini-cli"), Some(AgentKind::Gemini));
        assert_eq!(kind("codex exec 'fix it'"), Some(AgentKind::Codex));
        assert_eq!(kind("opencode"), Some(AgentKind::OpenCode));
        assert_eq!(kind("gemini -m pro"), Some(AgentKind::Gemini));
        assert_eq!(kind("kiro-cli chat"), Some(AgentKind::Kiro));
        assert_eq!(kind("q chat"), Some(AgentKind::Kiro));
        // Not agents.
        assert_eq!(kind("q --version"), None);
        assert_eq!(kind("cat claude.md"), None);
        assert_eq!(kind("npm run dev"), None);
        assert_eq!(kind("echo claude"), None);
        assert_eq!(kind(""), None);
        assert_eq!(kind("npx prettier ."), None);
    }

    #[test]
    fn slugs_round_trip() {
        for kind in AgentKind::ALL {
            assert_eq!(AgentKind::from_slug(kind.slug()), Some(kind));
        }
        assert_eq!(AgentKind::from_slug("vim"), None);
    }

    /// Prints every `step_ms` from `from` for `count` updates; returns the
    /// changes seen and the time of the last update.
    fn print(session: &mut Session, from: Instant, step_ms: u64, count: u64) -> (Vec<Change>, Instant) {
        let mut changes = Vec::new();
        let mut at = from;
        for i in 0..count {
            at = from + Duration::from_millis(i * step_ms);
            let change = session.on_output(at);
            if change != Change::None {
                changes.push(change);
            }
        }
        (changes, at)
    }

    #[test]
    fn without_hooks_sustained_output_means_working_and_silence_means_done() {
        let start = Instant::now();
        let mut session = Session::new(AgentKind::Codex, start);
        // A spinner: ten updates a second for three seconds.
        let (changes, last) = print(&mut session, start, 100, 30);
        assert_eq!(changes, [Change::State(AgentState::Working)]);

        // Still within the quiet period: asked to check back.
        let (change, next) = session.tick(last + Duration::from_secs(1));
        assert_eq!(change, Change::None);
        assert_eq!(next, Some(last + QUIET_AFTER));

        let (change, next) = session.tick(last + QUIET_AFTER);
        assert_eq!(change, Change::State(AgentState::Done));
        assert_eq!(next, None);

        // Looking at it clears "done".
        assert_eq!(session.mark_seen(last + Duration::from_secs(9)), Change::State(AgentState::Idle));
        assert_eq!(session.mark_seen(last + Duration::from_secs(10)), Change::None);
    }

    #[test]
    fn an_idle_program_repainting_now_and_then_is_not_working() {
        let start = Instant::now();
        let mut session = Session::new(AgentKind::Claude, start);
        // A status line refreshing every two seconds, for a minute.
        let (changes, _) = print(&mut session, start, 2000, 30);
        assert_eq!(changes, []);
        // A quick burst (a full-screen repaint in several writes).
        let (changes, _) = print(&mut session, start + Duration::from_secs(100), 20, 12);
        assert_eq!(changes, []);
        // Many updates, but spread too thin to be a spinner.
        let (changes, _) = print(&mut session, start + Duration::from_secs(200), 700, 20);
        assert_eq!(changes, []);
        assert_eq!(session.state, AgentState::Idle);
        assert_eq!(session.tick(start + Duration::from_secs(400)), (Change::None, None));
    }

    #[test]
    fn repaints_after_typing_resizing_or_focus_are_not_work() {
        let start = Instant::now();
        let mut session = Session::new(AgentKind::Gemini, start);
        // The user holds a key down for three seconds: every key press is
        // echoed, which looks exactly like sustained output.
        for i in 0..30 {
            let at = start + Duration::from_millis(i * 100);
            session.on_input(at);
            assert_eq!(session.on_output(at + Duration::from_millis(20)), Change::None);
        }
        assert_eq!(session.state, AgentState::Idle);
        // A resize repaint.
        session.on_disturbance(start + Duration::from_secs(10));
        let (changes, _) = print(&mut session, start + Duration::from_secs(10), 50, 10);
        assert_eq!(changes, []);
        // Real work afterwards is still noticed.
        let (changes, _) = print(&mut session, start + Duration::from_secs(20), 100, 30);
        assert_eq!(changes, [Change::State(AgentState::Working)]);
    }

    #[test]
    fn a_bell_means_the_agent_wants_the_user() {
        let start = Instant::now();
        let mut session = Session::new(AgentKind::Kiro, start);
        let (_, last) = print(&mut session, start, 100, 30);
        assert_eq!(session.on_attention(last), Change::State(AgentState::NeedsYou));
        // Silence must not turn "needs you" into "done".
        assert_eq!(session.tick(last + Duration::from_secs(60)).0, Change::None);
        assert_eq!(session.state, AgentState::NeedsYou);
    }

    #[test]
    fn hooked_sessions_ignore_guesswork() {
        let start = Instant::now();
        let mut session = Session::new(AgentKind::Claude, start);
        session.note_common(&serde_json::json!({ "session_id": "s1", "cwd": "/repo" }));
        assert!(session.hooked);
        let (changes, last) = print(&mut session, start, 100, 30);
        assert_eq!(changes, []);
        assert_eq!(session.on_attention(last), Change::None);
        assert_eq!(session.state, AgentState::Idle);
        assert_eq!(session.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn subagents_pick_up_who_launched_them_and_why() {
        let start = Instant::now();
        let mut session = Session::new(AgentKind::Claude, start);
        session.transcript = Some("/t/projects/repo/abc.jsonl".into());

        session.launch_requested(None, "find the bug");
        session.subagent_started("a1", "Explore", start);
        // The same start reported twice changes nothing.
        session.subagent_started("a1", "Explore", start);
        assert_eq!(session.subagents.len(), 1);
        let first = &session.subagents[0];
        assert_eq!((first.description.as_str(), &first.parent), ("find the bug", &None));
        assert_eq!(first.transcript.as_deref(), Some(std::path::Path::new("/t/projects/repo/abc/subagents/agent-a1.jsonl")));

        // A subagent launching its own subagent.
        session.launch_requested(Some("a1".into()), "read the logs");
        session.subagent_started("a2", "general-purpose", start);
        assert_eq!(session.subagents[1].parent.as_deref(), Some("a1"));
        // A parent we have never heard of is not believed.
        session.launch_requested(Some("ghost".into()), "x");
        session.subagent_started("a3", "Explore", start);
        assert_eq!(session.subagents[2].parent, None);

        session.subagent_activity("a2", "Reading app.log".into());
        assert_eq!(session.subagents[1].activity.as_deref(), Some("Reading app.log"));
        session.subagent_finished("a2", Some("nothing unusual".into()), start + Duration::from_secs(5));
        assert_eq!(session.running_subagents(), 2);
        assert_eq!(session.subagents[1].result.as_deref(), Some("nothing unusual"));
        assert_eq!(session.subagents[1].activity, None);

        // A new turn forgets finished subagents but keeps running ones.
        session.new_turn();
        let ids: Vec<&str> = session.subagents.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["a1", "a3"]);
    }

    #[test]
    fn subagent_ids_cannot_escape_the_transcript_folder() {
        let mut session = Session::new(AgentKind::Claude, Instant::now());
        session.transcript = Some("/t/abc.jsonl".into());
        session.subagent_started("../../etc/passwd", "x", Instant::now());
        assert_eq!(session.subagents[0].transcript, None);
    }

    #[test]
    fn background_tasks_are_read_tolerantly() {
        let mut session = Session::new(AgentKind::Claude, Instant::now());
        session.note_background_tasks(&serde_json::json!({
            "background_tasks": [
                { "id": "sh1", "command": "npm run dev" },
                { "description": "watching tests" },
                "plain string",
                { "unknown": 1 },
                42
            ]
        }));
        let labels: Vec<&str> = session.background_tasks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["npm run dev", "watching tests", "plain string"]);
        assert_eq!(session.background_tasks[0].id, "sh1");
        // No list in the report leaves the tasks as they were.
        session.note_background_tasks(&serde_json::json!({}));
        assert_eq!(session.background_tasks.len(), 3);
        session.note_background_tasks(&serde_json::json!({ "background_tasks": [] }));
        assert!(session.background_tasks.is_empty());
    }

    #[test]
    fn text_from_agents_is_cleaned() {
        assert_eq!(clean("  fix\n\tthe   bug \x1b[31m ", 80), "fix the bug [31m");
        assert_eq!(clean("abcdef", 3), "abc…");
        assert_eq!(clean("\n\n", 10), "");
    }

    #[test]
    fn paths_are_shortened() {
        assert_eq!(short_path("/repo/src/main.rs", Some("/repo")), "src/main.rs");
        assert_eq!(short_path("/repo/crates/app/src/components/sidebar.rs", Some("/repo")), "components/sidebar.rs");
        assert_eq!(short_path("/other/place/lib.rs", Some("/repo")), "place/lib.rs");
        assert_eq!(short_path("file.rs", None), "file.rs");
        assert_eq!(short_path("/repository/x.rs", Some("/repo")), "repository/x.rs");
    }
}
