//! A record of agent sessions that have ended, so they can be found and
//! resumed later.
//!
//! Only metadata is kept: which agent, what the task was called, where it
//! ran, and the agent's own session id. Nothing the agent or the terminal
//! printed is stored; the agent's own transcript remains the only copy of
//! the conversation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::AgentKind;

/// Most sessions remembered; older ones are dropped.
const CAPACITY: usize = 200;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ArchivedSession {
    /// The agent's short name ("claude", "codex", ...).
    pub agent: String,
    pub title: String,
    /// Folder the agent ran in.
    pub cwd: Option<PathBuf>,
    /// Name of the workspace it belonged to.
    pub project: String,
    /// The agent's own id for the conversation, used to resume it.
    pub session_id: Option<String>,
    /// When it ended, in seconds since 1970.
    pub ended_at: u64,
    /// How long the session lasted, in seconds.
    pub duration: u64,
}

impl ArchivedSession {
    pub fn kind(&self) -> Option<AgentKind> {
        AgentKind::from_slug(&self.agent)
    }

    /// The command that continues this session, when the agent supports
    /// resuming by id and we know the id.
    pub fn resume_command(&self) -> Option<String> {
        resume_command(self.kind()?, self.session_id.as_deref())
    }
}

/// The command line that resumes a conversation.
///
/// Only Claude Code's is known for certain. The id is checked to be a
/// plain token before it goes anywhere near a shell.
pub fn resume_command(kind: AgentKind, session_id: Option<&str>) -> Option<String> {
    let id = session_id?;
    let safe = !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    match kind {
        AgentKind::Claude if safe => Some(format!("claude --resume {id}")),
        _ => None,
    }
}

/// The command that starts a fresh session of an agent.
pub fn start_command(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
        AgentKind::OpenCode => "opencode",
        AgentKind::Gemini => "gemini",
        AgentKind::Kiro => "kiro-cli",
    }
}

#[derive(Serialize, Deserialize, Default)]
struct ArchiveFile {
    sessions: Vec<ArchivedSession>,
}

/// The archive, newest session first.
#[derive(Default)]
pub struct Archive {
    sessions: Vec<ArchivedSession>,
    /// Where it is stored; `None` keeps it in memory only.
    path: Option<PathBuf>,
}

impl Archive {
    /// Loads the archive stored in `dir`. A missing or unreadable file
    /// gives an empty archive.
    pub fn load(dir: &Path) -> Archive {
        let path = dir.join("archive.json");
        let sessions = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<ArchiveFile>(&text).ok())
            .map(|file| file.sessions)
            .unwrap_or_default();
        Archive { sessions, path: Some(path) }
    }

    pub fn sessions(&self) -> &[ArchivedSession] {
        &self.sessions
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Adds a session at the front. An earlier entry for the same
    /// conversation (it was resumed and has ended again) is replaced.
    pub fn add(&mut self, session: ArchivedSession) {
        if session.session_id.is_some() {
            self.sessions.retain(|old| old.agent != session.agent || old.session_id != session.session_id);
        }
        self.sessions.insert(0, session);
        self.sessions.truncate(CAPACITY);
        self.save();
    }

    /// Removes and returns the session at `index`.
    pub fn take(&mut self, index: usize) -> Option<ArchivedSession> {
        if index >= self.sessions.len() {
            return None;
        }
        let session = self.sessions.remove(index);
        self.save();
        Some(session)
    }

    pub fn clear(&mut self) {
        self.sessions.clear();
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let file = ArchiveFile { sessions: self.sessions.clone() };
        let result = serde_json::to_string_pretty(&file).map_err(std::io::Error::other).and_then(|json| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // Write beside the file and rename, so it is never half-written.
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, json)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(&temporary, path)
        });
        if let Err(e) = result {
            log::warn!("archive not saved: {e}");
        }
    }
}

/// How long ago something happened, in words: "just now", "5m ago", ...
pub fn time_ago(now: u64, then: u64) -> String {
    let seconds = now.saturating_sub(then);
    match seconds {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

/// The current time in seconds since 1970.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(title: &str, id: Option<&str>) -> ArchivedSession {
        ArchivedSession {
            agent: "claude".into(),
            title: title.into(),
            cwd: Some("/repo".into()),
            project: "repo".into(),
            session_id: id.map(str::to_owned),
            ended_at: 1000,
            duration: 60,
        }
    }

    #[test]
    fn newest_first_and_resumed_sessions_replace_their_old_entry() {
        let mut archive = Archive::default();
        archive.add(session("first", Some("a")));
        archive.add(session("second", Some("b")));
        archive.add(session("first, continued", Some("a")));
        let titles: Vec<&str> = archive.sessions().iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["first, continued", "second"]);

        // Sessions without an id are never merged.
        archive.add(session("x", None));
        archive.add(session("x", None));
        assert_eq!(archive.len(), 4);
    }

    #[test]
    fn it_is_bounded() {
        let mut archive = Archive::default();
        for i in 0..CAPACITY + 20 {
            archive.add(session(&format!("s{i}"), None));
        }
        assert_eq!(archive.len(), CAPACITY);
        assert_eq!(archive.sessions()[0].title, format!("s{}", CAPACITY + 19));
    }

    #[test]
    fn it_survives_a_reload_and_tolerates_a_broken_file() {
        let dir = std::env::temp_dir().join(format!("ale-term-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut archive = Archive::load(&dir);
        assert!(archive.is_empty());
        archive.add(session("kept", Some("abc")));
        assert_eq!(Archive::load(&dir).sessions(), archive.sessions());

        assert_eq!(archive.take(0).map(|s| s.title), Some("kept".into()));
        assert_eq!(archive.take(5), None);
        assert!(Archive::load(&dir).is_empty());

        std::fs::write(dir.join("archive.json"), "{ nope").unwrap();
        assert!(Archive::load(&dir).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resume_commands_are_only_built_from_safe_ids() {
        let claude = AgentKind::Claude;
        assert_eq!(resume_command(claude, Some("3f2a-91bc_7")), Some("claude --resume 3f2a-91bc_7".into()));
        assert_eq!(resume_command(claude, None), None);
        assert_eq!(resume_command(claude, Some("")), None);
        // Anything that could mean something to a shell is refused.
        assert_eq!(resume_command(claude, Some("abc; rm -rf ~")), None);
        assert_eq!(resume_command(claude, Some("$(whoami)")), None);
        assert_eq!(resume_command(claude, Some("a b")), None);
        // Agents whose resume syntax is not known.
        assert_eq!(resume_command(AgentKind::Codex, Some("abc")), None);
    }

    #[test]
    fn times_read_naturally() {
        assert_eq!(time_ago(1000, 990), "just now");
        assert_eq!(time_ago(1000, 400), "10m ago");
        assert_eq!(time_ago(10_000, 0), "2h ago");
        assert_eq!(time_ago(400_000, 0), "4d ago");
        // A clock that went backwards does not underflow.
        assert_eq!(time_ago(5, 1000), "just now");
    }
}
