//! What one agent is doing: its state, title, current activity and
//! subagents.
//!
//! A [`Session`] is updated from two sources. Agents with hooks installed
//! report precisely (see the `hooks_*` modules). Without hooks we fall back
//! to watching the pane: output means "working", silence means "idle", a
//! bell means "needs you". Once a session has received any hook message the
//! guesswork is switched off for it.

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
    WaitingOnSubagents,
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
            AgentState::WaitingOnSubagents => "Waiting on subagents",
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
    pub started: Instant,
    pub finished: Option<Instant>,
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

#[derive(Clone, Debug)]
pub struct Session {
    pub kind: AgentKind,
    pub state: AgentState,
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
    /// True once a hook message has arrived: the state is then reported,
    /// not guessed.
    pub hooked: bool,
    last_output: Option<Instant>,
    last_input: Option<Instant>,
}

/// Silence after which an agent without hooks is assumed to be idle.
const QUIET_AFTER: Duration = Duration::from_millis(2500);
/// Output this soon after a key press is just the key being echoed.
const ECHO_WINDOW: Duration = Duration::from_millis(400);
/// How long a finished subagent stays listed.
const KEEP_FINISHED: Duration = Duration::from_secs(20);

impl Session {
    pub fn new(kind: AgentKind, now: Instant) -> Session {
        Session {
            kind,
            state: AgentState::Idle,
            since: now,
            title: None,
            activity: None,
            session_id: None,
            transcript: None,
            cwd: None,
            subagents: Vec::new(),
            hooked: false,
            last_output: None,
            last_input: None,
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
    pub fn on_input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    /// The pane printed something.
    pub fn on_output(&mut self, now: Instant) -> Change {
        if self.hooked {
            return Change::None;
        }
        if self.last_input.is_some_and(|input| now.duration_since(input) < ECHO_WINDOW) {
            return Change::None;
        }
        self.last_output = Some(now);
        if self.state == AgentState::Working { Change::None } else { self.set_state(AgentState::Working, now) }
    }

    /// The pane rang the bell or asked for a notification: agents do that
    /// when they want the user.
    pub fn on_attention(&mut self, now: Instant) -> Change {
        if self.hooked {
            return Change::None;
        }
        self.last_output = None;
        self.set_state(AgentState::NeedsYou, now)
    }

    /// Call periodically. Returns what changed, and when to call again (if
    /// there is anything to wait for).
    pub fn tick(&mut self, now: Instant) -> (Change, Option<Instant>) {
        let before = self.subagents.len();
        self.subagents
            .retain(|s| s.finished.is_none_or(|finished| now.duration_since(finished) < KEEP_FINISHED));
        let mut change = if self.subagents.len() != before { Change::Updated } else { Change::None };
        let mut next = self.subagents.iter().filter_map(|s| s.finished).map(|f| f + KEEP_FINISHED).min();

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

    /// Uses `title` as the session title unless one is already set.
    pub(crate) fn suggest_title(&mut self, title: &str) {
        if self.title.is_none() {
            let title = clean(title, 80);
            if !title.is_empty() {
                self.title = Some(title);
            }
        }
    }

    pub(crate) fn subagent_started(&mut self, id: &str, kind: &str, description: &str, now: Instant) {
        if self.subagents.iter().any(|subagent| subagent.id == id) {
            return;
        }
        self.subagents.push(Subagent {
            id: clean(id, 80),
            kind: clean(kind, 40),
            description: clean(description, 120),
            started: now,
            finished: None,
        });
    }

    pub(crate) fn subagent_finished(&mut self, id: &str, now: Instant) {
        if let Some(subagent) = self.subagents.iter_mut().find(|subagent| subagent.id == id) {
            subagent.finished.get_or_insert(now);
        }
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

    #[test]
    fn without_hooks_output_means_working_and_silence_means_done() {
        let start = Instant::now();
        let mut session = Session::new(AgentKind::Codex, start);
        assert_eq!(session.on_output(start), Change::State(AgentState::Working));
        assert_eq!(session.on_output(start + Duration::from_secs(1)), Change::None);

        // Still within the quiet period: asked to check back.
        let (change, next) = session.tick(start + Duration::from_secs(2));
        assert_eq!(change, Change::None);
        assert_eq!(next, Some(start + Duration::from_secs(1) + QUIET_AFTER));

        let (change, next) = session.tick(start + Duration::from_secs(4));
        assert_eq!(change, Change::State(AgentState::Done));
        assert_eq!(next, None);

        // Looking at it clears "done".
        assert_eq!(session.mark_seen(start + Duration::from_secs(5)), Change::State(AgentState::Idle));
        assert_eq!(session.mark_seen(start + Duration::from_secs(6)), Change::None);
    }

    #[test]
    fn typing_echo_is_not_mistaken_for_work() {
        let start = Instant::now();
        let mut session = Session::new(AgentKind::Gemini, start);
        session.on_input(start);
        assert_eq!(session.on_output(start + Duration::from_millis(50)), Change::None);
        assert_eq!(session.state, AgentState::Idle);
        // Output well after the key press is the agent.
        assert_eq!(session.on_output(start + Duration::from_secs(1)), Change::State(AgentState::Working));
    }

    #[test]
    fn a_bell_means_the_agent_wants_the_user() {
        let start = Instant::now();
        let mut session = Session::new(AgentKind::Kiro, start);
        session.on_output(start);
        assert_eq!(session.on_attention(start), Change::State(AgentState::NeedsYou));
        // Silence must not turn "needs you" into "done".
        assert_eq!(session.tick(start + Duration::from_secs(60)).0, Change::None);
        assert_eq!(session.state, AgentState::NeedsYou);
    }

    #[test]
    fn hooked_sessions_ignore_guesswork() {
        let start = Instant::now();
        let mut session = Session::new(AgentKind::Claude, start);
        session.note_common(&serde_json::json!({ "session_id": "s1", "cwd": "/repo" }));
        assert!(session.hooked);
        assert_eq!(session.on_output(start), Change::None);
        assert_eq!(session.on_attention(start), Change::None);
        assert_eq!(session.state, AgentState::Idle);
        assert_eq!(session.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn finished_subagents_linger_briefly() {
        let start = Instant::now();
        let mut session = Session::new(AgentKind::Claude, start);
        session.subagent_started("a1", "Explore", "find the bug", start);
        session.subagent_started("a1", "Explore", "duplicate", start);
        assert_eq!(session.running_subagents(), 1);
        session.subagent_finished("a1", start + Duration::from_secs(5));
        assert_eq!(session.running_subagents(), 0);
        assert_eq!(session.subagents.len(), 1);
        let (change, _) = session.tick(start + Duration::from_secs(5) + KEEP_FINISHED);
        assert_eq!(change, Change::Updated);
        assert!(session.subagents.is_empty());
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
