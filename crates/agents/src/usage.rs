//! How much the agents have been used: tokens, tasks, turns and working
//! time, read from the agents' own conversation records and kept as hourly
//! totals.
//!
//! Only numbers are kept: counts per hour, agent, project folder and model.
//! Nothing an agent or the user wrote is stored. The totals outlive the
//! records they were read from (Claude Code removes its own after a
//! while), and remember how far into each record they have read, so
//! reading again only looks at what is new.
//!
//! What is counted:
//!
//! * a **task** is one conversation, counted when the agent first replies;
//! * a **turn** is one prompt the user sent that the agent replied to;
//! * **working time** runs from a prompt (or a tool's result) to the
//!   agent's next reply, so time spent waiting for the user does not count;
//! * **input** tokens include what was read from and written to the cache.
//!
//! The record format is internal to Claude Code and may change. Lines that
//! are not understood are skipped.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

use crate::session::AgentKind;

const VERSION: u32 = 1;
const HOUR_MS: u64 = 3_600_000;
const DAY: i64 = 86_400;

/// The longest stretch between two events of a turn that counts as work.
/// A longer one is almost always a question left unanswered.
const LONGEST_STEP_MS: u64 = 30 * 60 * 1000;

/// How many of a record's latest reply ids are remembered. A reply is
/// written as several consecutive lines, each repeating its token counts.
const RECENT_IDS: usize = 8;

/// How deep below the records folder files are looked for.
const DEPTH: usize = 5;

/// Tokens used with one model, in one hour.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Tokens {
    /// Input that was neither read from nor written to the cache.
    pub input: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    /// Conversations that used this model for the first time.
    pub tasks: u64,
}

impl Tokens {
    /// Everything sent to the model, cached or not.
    pub fn all_input(&self) -> u64 {
        self.input + self.cache_write + self.cache_read
    }
}

/// One hour of one agent's work in one project.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
struct Bucket {
    /// Hours since 1970, in UTC.
    hour: u64,
    /// The agent's short name ("claude", "codex", ...).
    agent: String,
    /// The project folder; empty for folders outside any project.
    project: String,
    turns: u64,
    tasks: u64,
    /// Working time, in milliseconds.
    millis: u64,
    /// Conversations that were active during the hour.
    sessions: u64,
    models: BTreeMap<String, Tokens>,
}

/// How far a record has been read, and what of its conversation carries
/// over to the lines not yet written.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
struct Mark {
    offset: u64,
    /// The user sent a prompt the agent has not replied to yet.
    pending: bool,
    /// When the turn in progress last moved, in milliseconds since 1970.
    last: Option<u64>,
    /// The conversation has been counted as a task.
    started: bool,
    /// Models the conversation has used.
    models: Vec<String>,
    /// The last hour the conversation was counted as active in.
    hour: Option<u64>,
    /// Ids of its latest replies.
    recent: Vec<String>,
    /// Where the agent is running and with which model, for records that
    /// say so once rather than on every line.
    cwd: Option<PathBuf>,
    model: Option<String>,
    /// The conversation's running totals of input, cached input and
    /// output tokens, for records that report totals.
    totals: [u64; 3],
    /// For a record that is one document: its size when last read, and
    /// how many of its messages have been counted.
    size: u64,
    count: u64,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct StoreFile {
    version: u32,
    files: BTreeMap<String, Mark>,
    buckets: Vec<Bucket>,
}

/// The totals, and how far each record has been read.
#[derive(Clone, Default)]
pub struct Store {
    files: BTreeMap<String, Mark>,
    buckets: BTreeMap<(u64, String, String), Bucket>,
    /// Where it is stored; `None` keeps it in memory only.
    path: Option<PathBuf>,
}

impl Store {
    /// Loads the totals stored in `dir`. A missing or unreadable file, or
    /// one from a different version, gives empty totals.
    pub fn load(dir: &Path) -> Store {
        let path = dir.join("usage.json");
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<StoreFile>(&text).ok())
            .filter(|file| file.version == VERSION)
            .unwrap_or_default();
        let buckets = file
            .buckets
            .into_iter()
            .map(|bucket| ((bucket.hour, bucket.agent.clone(), bucket.project.clone()), bucket))
            .collect();
        Store { files: file.files, buckets, path: Some(path) }
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        let file = StoreFile {
            version: VERSION,
            files: self.files.clone(),
            buckets: self.buckets.values().cloned().collect(),
        };
        let json = serde_json::to_string(&file).map_err(std::io::Error::other)?;
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
    }

    fn bucket(&mut self, hour: u64, agent: AgentKind, project: &str) -> &mut Bucket {
        self.buckets.entry((hour, agent.slug().to_owned(), project.to_owned())).or_insert_with(|| Bucket {
            hour,
            agent: agent.slug().to_owned(),
            project: project.to_owned(),
            ..Bucket::default()
        })
    }

    /// Reads what is new in every agent's records: Claude Code's, Codex's
    /// and Gemini CLI's under the home folder, and the journal ale-term
    /// keeps itself (in `journal`) for the agents whose records it cannot
    /// read. `project_of` names the project a folder belongs to. Returns
    /// whether anything changed.
    pub fn scan(&mut self, home: &Path, journal: Option<&Path>, project_of: &dyn Fn(&Path) -> Option<PathBuf>) -> bool {
        let mut projects = Projects { known: HashMap::new(), project_of };
        let mut changed = self.scan_records(&home.join(".claude").join("projects"), Reader::Claude, &mut projects);
        changed |= self.scan_records(&home.join(".codex").join("sessions"), Reader::Codex, &mut projects);
        changed |= self.scan_records(&home.join(".gemini").join("tmp"), Reader::Gemini, &mut projects);
        if let Some(journal) = journal {
            changed |= self.scan_records(journal, Reader::Journal, &mut projects);
        }
        changed
    }

    /// Reads what is new in one agent's records under `root`, a folder or
    /// a single file.
    fn scan_records(&mut self, root: &Path, reader: Reader, projects: &mut Projects) -> bool {
        let mut records = Vec::new();
        if root.is_file() {
            records.push((root.to_owned(), false));
        } else {
            find_records(root, DEPTH, reader, false, &mut records);
        }
        let mut changed = false;
        for (path, nested) in &records {
            let key = path.to_string_lossy().into_owned();
            let mut mark = self.files.get(&key).cloned().unwrap_or_default();
            let before = mark.clone();
            match reader {
                // Older Gemini records are one document, written anew
                // each time; only its new messages are counted.
                Reader::Gemini if path.extension().is_some_and(|extension| extension == "json") => {
                    let Ok(size) = std::fs::metadata(path).map(|meta| meta.len()) else { continue };
                    if size == mark.size {
                        continue;
                    }
                    mark.size = size;
                    let document = std::fs::read_to_string(path).ok();
                    let Some(document) = document.and_then(|text| serde_json::from_str::<GeminiDocument>(&text).ok())
                    else {
                        continue;
                    };
                    let folder = gemini_folder(path);
                    let seen = mark.count as usize;
                    mark.count = document.messages.len() as u64;
                    for message in document.messages.into_iter().skip(seen) {
                        self.gemini_message(message, false, folder.as_deref(), &mut mark, projects);
                    }
                }
                _ => {
                    let Some(text) = read_new_lines(path, &mut mark.offset) else { continue };
                    // Gemini's records do not say where the agent ran;
                    // a file beside them does.
                    let folder = if reader == Reader::Gemini { gemini_folder(path) } else { None };
                    for line in text.lines() {
                        match reader {
                            Reader::Claude => self.claude_line(line, *nested, &mut mark, projects),
                            Reader::Codex => self.codex_line(line, &mut mark, projects),
                            Reader::Gemini => {
                                if let Ok(message) = serde_json::from_str::<GeminiMessage>(line) {
                                    self.gemini_message(message, *nested, folder.as_deref(), &mut mark, projects);
                                }
                            }
                            Reader::Journal => self.journal_line(line, projects),
                        }
                    }
                }
            }
            if mark != before {
                self.files.insert(key, mark);
                changed = true;
            }
        }
        // Records that are gone need no bookmark.
        let known = self.files.len();
        self.files.retain(|key, _| {
            !Path::new(key).starts_with(root) || records.iter().any(|(path, _)| path.to_string_lossy() == *key)
        });
        changed || self.files.len() != known
    }

    /// The agent did something at `time`: the stretch since the turn last
    /// moved is working time, and its conversation was active this hour.
    fn work(&mut self, mark: &mut Mark, agent: AgentKind, time: u64, project: &str) {
        let hour = time / HOUR_MS;
        let bucket = self.bucket(hour, agent, project);
        if let Some(last) = mark.last.replace(time) {
            bucket.millis += time.saturating_sub(last).min(LONGEST_STEP_MS);
        }
        if mark.hour != Some(hour) {
            mark.hour = Some(hour);
            bucket.sessions += 1;
        }
    }

    /// The agent replied, using `used` tokens of `model`. A reply in the
    /// conversation itself (`main`, not one of its subagents) also counts
    /// its task the first time, and the turn its prompt began.
    fn reply(&mut self, mark: &mut Mark, at: (AgentKind, u64, &str), model: &str, used: Tokens, main: bool) {
        let (agent, time, project) = at;
        let bucket = self.bucket(time / HOUR_MS, agent, project);
        if main && !std::mem::replace(&mut mark.started, true) {
            bucket.tasks += 1;
        }
        if main && std::mem::take(&mut mark.pending) {
            bucket.turns += 1;
        }
        let tokens = bucket.models.entry(model.to_owned()).or_default();
        tokens.input += used.input;
        tokens.cache_write += used.cache_write;
        tokens.cache_read += used.cache_read;
        tokens.output += used.output;
        if main && !mark.models.iter().any(|known| known == model) {
            mark.models.push(model.to_owned());
            tokens.tasks += 1;
        }
    }

    /// Whether a reply with this id was counted already; remembers it.
    fn repeated(mark: &mut Mark, id: Option<String>) -> bool {
        let Some(id) = id else { return false };
        if mark.recent.contains(&id) {
            return true;
        }
        mark.recent.push(id);
        if mark.recent.len() > RECENT_IDS {
            mark.recent.remove(0);
        }
        false
    }

    /// Counts one line of a Claude Code record.
    fn claude_line(&mut self, line: &str, subagent: bool, mark: &mut Mark, projects: &mut Projects) {
        // Most lines are neither; skip them without parsing.
        if !line.contains("\"type\":\"assistant\"") && !line.contains("\"type\":\"user\"") {
            return;
        }
        let Ok(line) = serde_json::from_str::<Line>(line) else { return };
        let Some(time) = line.timestamp.as_deref().and_then(parse_time) else { return };
        let Some(message) = line.message else { return };
        let subagent = subagent || line.sidechain;
        let agent = AgentKind::Claude;
        match line.kind.as_str() {
            // A subagent's prompts and tool results happen inside its
            // parent's turn, which is already being timed.
            "user" if subagent => {}
            "user" if message.content.tool_result => {
                if mark.last.is_some() {
                    self.work(mark, agent, time, &projects.of(line.cwd.as_deref()));
                }
            }
            "user" => {
                mark.last = Some(time);
                // Notes Claude Code adds for itself are not prompts.
                mark.pending |= !line.meta && !line.compact_summary;
            }
            "assistant" => {
                let (Some(model), Some(usage)) = (message.model, message.usage) else { return };
                // Messages Claude Code writes itself, such as errors.
                if model.starts_with('<') {
                    return;
                }
                let project = projects.of(line.cwd.as_deref());
                if !subagent {
                    self.work(mark, agent, time, &project);
                    // The turn is over once the agent stops to answer.
                    if message.stop_reason.as_deref() == Some("end_turn") {
                        mark.last = None;
                    }
                }
                if Self::repeated(mark, message.id) {
                    return;
                }
                let used = Tokens {
                    input: usage.input_tokens.unwrap_or(0),
                    cache_write: usage.cache_creation_input_tokens.unwrap_or(0),
                    cache_read: usage.cache_read_input_tokens.unwrap_or(0),
                    output: usage.output_tokens.unwrap_or(0),
                    tasks: 0,
                };
                self.reply(mark, (agent, time, &project), &model, used, !subagent);
            }
            _ => {}
        }
    }

    /// Counts one line of a Codex session record. Codex reports the
    /// conversation's running token totals after each reply.
    fn codex_line(&mut self, line: &str, mark: &mut Mark, projects: &mut Projects) {
        let Ok(line) = serde_json::from_str::<CodexLine>(line) else { return };
        let Some(time) = line.timestamp.as_deref().and_then(parse_time) else { return };
        let payload = line.payload;
        if payload.cwd.is_some() {
            mark.cwd = payload.cwd;
        }
        if payload.model.is_some() {
            mark.model = payload.model;
        }
        let agent = AgentKind::Codex;
        let project = projects.of(mark.cwd.as_deref());
        match (line.kind.as_str(), payload.kind.as_deref()) {
            ("event_msg", Some("user_message")) => {
                mark.last = Some(time);
                mark.pending = true;
            }
            ("event_msg", Some("task_started" | "turn_started")) => {
                mark.last.get_or_insert(time);
            }
            ("event_msg", Some("task_complete" | "turn_complete" | "turn_aborted")) => {
                if mark.last.is_some() {
                    self.work(mark, agent, time, &project);
                }
                mark.last = None;
            }
            ("event_msg", Some("token_count")) => {
                let Some(info) = payload.info else { return };
                let total = info.total_token_usage.unwrap_or_default();
                let now = [total.input_tokens, total.cached_input_tokens, total.output_tokens];
                // Normally the growth of the totals; if they went back
                // (the record was rewound), what the last reply used.
                let grew = now.iter().zip(mark.totals).all(|(now, before)| *now >= before);
                let [input, cached, output] = if grew {
                    [now[0] - mark.totals[0], now[1] - mark.totals[1], now[2] - mark.totals[2]]
                } else {
                    let last = info.last_token_usage.unwrap_or_default();
                    [last.input_tokens, last.cached_input_tokens, last.output_tokens]
                };
                mark.totals = now;
                if input + output == 0 {
                    return;
                }
                self.work(mark, agent, time, &project);
                // Codex counts cached input as part of the input.
                let used = Tokens {
                    input: input.saturating_sub(cached),
                    cache_read: cached.min(input),
                    output,
                    ..Tokens::default()
                };
                let model = mark.model.clone().unwrap_or_else(|| "codex".into());
                self.reply(mark, (agent, time, &project), &model, used, true);
            }
            ("event_msg" | "response_item", _) if mark.last.is_some() => self.work(mark, agent, time, &project),
            _ => {}
        }
    }

    /// Counts one message of a Gemini CLI chat record. A message is
    /// written again whenever it changes; its tokens arrive with the last
    /// of those, and are counted once.
    fn gemini_message(&mut self, message: GeminiMessage, subagent: bool, folder: Option<&Path>, mark: &mut Mark, projects: &mut Projects) {
        let (Some(kind), Some(time)) = (message.kind.as_deref(), message.timestamp.as_deref().and_then(parse_time))
        else {
            return;
        };
        let agent = AgentKind::Gemini;
        let project = projects.of(folder);
        match kind {
            "user" if !subagent => {
                mark.last = Some(time);
                mark.pending = true;
            }
            "gemini" => {
                if !subagent && mark.last.is_some() {
                    self.work(mark, agent, time, &project);
                }
                let Some(tokens) = message.tokens else { return };
                if Self::repeated(mark, message.id) {
                    return;
                }
                // The prompt's count includes what came from the cache;
                // thinking is counted apart from the answer.
                let used = Tokens {
                    input: tokens.input.saturating_sub(tokens.cached),
                    cache_read: tokens.cached.min(tokens.input),
                    output: tokens.output + tokens.thoughts,
                    ..Tokens::default()
                };
                let model = message.model.unwrap_or_else(|| "gemini".into());
                self.reply(mark, (agent, time, &project), &model, used, !subagent);
            }
            _ => {}
        }
    }

    /// Counts one line of ale-term's own journal: a stretch of work by an
    /// agent whose records it cannot read.
    fn journal_line(&mut self, line: &str, projects: &mut Projects) {
        let Ok(stretch) = serde_json::from_str::<Stretch>(line) else { return };
        let Some(agent) = AgentKind::from_slug(&stretch.agent) else { return };
        let project = projects.of(stretch.cwd.as_deref());
        let bucket = self.bucket(stretch.time / HOUR_MS, agent, &project);
        bucket.millis += stretch.millis;
        bucket.tasks += u64::from(stretch.task);
        bucket.turns += u64::from(stretch.turn);
        bucket.sessions += u64::from(stretch.active);
    }
}

/// Whose records are being read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reader {
    Claude,
    Codex,
    Gemini,
    /// ale-term's own journal.
    Journal,
}

/// Names the project each folder belongs to, asking once per folder.
struct Projects<'a> {
    known: HashMap<PathBuf, String>,
    project_of: &'a dyn Fn(&Path) -> Option<PathBuf>,
}

impl Projects<'_> {
    /// The project of a folder; empty when it is in none, or unknown.
    fn of(&mut self, folder: Option<&Path>) -> String {
        let Some(folder) = folder.filter(|folder| !folder.as_os_str().is_empty()) else { return String::new() };
        if let Some(project) = self.known.get(folder) {
            return project.clone();
        }
        let project = (self.project_of)(folder).map(|root| root.to_string_lossy().into_owned()).unwrap_or_default();
        self.known.insert(folder.to_owned(), project.clone());
        project
    }
}

/// A stretch of an agent's work, as ale-term saw it in one of its panes.
/// Written to the journal for agents that keep their records in a form
/// ale-term does not read.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Stretch {
    /// When it ended, in milliseconds since 1970.
    pub time: u64,
    /// The agent's short name.
    pub agent: String,
    /// Folder the agent ran in.
    pub cwd: Option<PathBuf>,
    /// How long it lasted, in milliseconds.
    pub millis: u64,
    /// It is the first of its session.
    pub task: bool,
    /// It began with a prompt from the user.
    pub turn: bool,
    /// It is the session's first in this hour.
    pub active: bool,
}

/// Name of the journal in ale-term's state folder.
pub const JOURNAL: &str = "activity.jsonl";

/// Whether ale-term keeps its own journal for an agent, because it cannot
/// read the agent's records (Kiro and OpenCode keep theirs in a database).
pub fn journaled(agent: AgentKind) -> bool {
    matches!(agent, AgentKind::Kiro | AgentKind::OpenCode)
}

/// Adds a stretch to the journal at `path`.
pub fn append_stretch(path: &Path, stretch: &Stretch) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut line = serde_json::to_string(stretch).map_err(std::io::Error::other)?;
    line.push('\n');
    options.open(path)?.write_all(line.as_bytes())
}

/// The folder a Gemini CLI chat record belongs to: Gemini keeps each
/// project's chats in a folder of their own, with the project's path in a
/// file called `.project_root`.
fn gemini_folder(record: &Path) -> Option<PathBuf> {
    let chats = record.ancestors().find(|folder| folder.file_name().is_some_and(|name| name == "chats"))?;
    let text = std::fs::read_to_string(chats.parent()?.join(".project_root")).ok()?;
    let folder = text.trim();
    (!folder.is_empty()).then(|| PathBuf::from(folder))
}

/// Collects an agent's records under `folder`, and whether each is nested:
/// a subagent's, kept in a folder below its conversation's. Links are not
/// followed.
fn find_records(folder: &Path, depth: usize, reader: Reader, nested: bool, found: &mut Vec<(PathBuf, bool)>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let in_chats = folder.file_name().is_some_and(|name| name == "chats");
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if kind.is_dir() && depth > 0 {
            let nested = match reader {
                Reader::Claude => nested || name == "subagents",
                // Below the chats, each folder is a conversation's subagents.
                Reader::Gemini => nested || in_chats,
                Reader::Codex | Reader::Journal => false,
            };
            find_records(&path, depth - 1, reader, nested, found);
        } else if kind.is_file() {
            let lines = name.ends_with(".jsonl");
            let wanted = match reader {
                Reader::Claude => lines,
                Reader::Codex => lines && name.starts_with("rollout-"),
                // Gemini keeps other things beside its chats.
                Reader::Gemini => (in_chats || nested) && (lines || (name.ends_with(".json") && name.starts_with("session-"))),
                Reader::Journal => false,
            };
            if wanted {
                found.push((path, nested));
            }
        }
    }
}

/// The complete lines added to a file since `offset`, which moves past
/// them. A line still being written is left for next time.
fn read_new_lines(path: &Path, offset: &mut u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length < *offset {
        // The file was replaced by a shorter one. What it held is already
        // counted; carry on from its end.
        *offset = length;
    }
    if length == *offset {
        return None;
    }
    file.seek(SeekFrom::Start(*offset)).ok()?;
    let mut bytes = Vec::with_capacity((length - *offset) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let complete = bytes.iter().rposition(|byte| *byte == b'\n')? + 1;
    bytes.truncate(complete);
    *offset += complete as u64;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "isSidechain")]
    sidechain: bool,
    #[serde(default, rename = "isMeta")]
    meta: bool,
    #[serde(default, rename = "isCompactSummary")]
    compact_summary: bool,
    timestamp: Option<String>,
    cwd: Option<PathBuf>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    stop_reason: Option<String>,
    usage: Option<Usage>,
    #[serde(default)]
    content: Shape,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct CodexLine {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    payload: CodexPayload,
}

/// The parts of a Codex line's payload that matter here, whichever kind
/// of line it is.
#[derive(Deserialize, Default)]
#[serde(default)]
struct CodexPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    cwd: Option<PathBuf>,
    model: Option<String>,
    info: Option<CodexUsage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CodexUsage {
    total_token_usage: Option<CodexTokens>,
    last_token_usage: Option<CodexTokens>,
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(default)]
struct CodexTokens {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GeminiDocument {
    messages: Vec<GeminiMessage>,
}

/// A message of a Gemini chat. Lines of other kinds have none of these
/// fields and are passed over.
#[derive(Deserialize, Default)]
#[serde(default)]
struct GeminiMessage {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
    tokens: Option<GeminiTokens>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GeminiTokens {
    input: u64,
    output: u64,
    cached: u64,
    thoughts: u64,
}

/// What a message's content is made of, without keeping any of it.
#[derive(Default)]
struct Shape {
    /// It carries the result of a tool call.
    tool_result: bool,
}

impl<'de> Deserialize<'de> for Shape {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Shape, D::Error> {
        #[derive(Deserialize)]
        struct Block {
            #[serde(default, rename = "type")]
            kind: String,
        }
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Shape;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("text or a list of content blocks")
            }

            fn visit_str<E>(self, _: &str) -> Result<Shape, E> {
                Ok(Shape::default())
            }

            fn visit_unit<E>(self) -> Result<Shape, E> {
                Ok(Shape::default())
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut blocks: A) -> Result<Shape, A::Error> {
                let mut shape = Shape::default();
                while let Some(block) = blocks.next_element::<Block>()? {
                    shape.tool_result |= block.kind == "tool_result";
                }
                Ok(shape)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// Milliseconds since 1970 for a time like `2026-11-21T16:05:08.183Z`.
fn parse_time(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || !text.is_ascii() || bytes[10] != b'T' || !text.ends_with('Z') {
        return None;
    }
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let millis = match text[19..text.len() - 1].strip_prefix('.') {
        Some(fraction) => format!("{fraction:0<3}").get(..3)?.parse::<i64>().ok()?,
        None => 0,
    };
    let seconds = days_from_date(year, month, day) * DAY + hour * 3600 + minute * 60 + second;
    u64::try_from(seconds * 1000 + millis).ok()
}

/// Days since 1970-01-01 for a calendar date.
pub fn days_from_date(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year.rem_euclid(400);
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468
}

/// The calendar date (year, month, day) of a day counted from 1970-01-01.
pub fn date_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let day = (of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 } as u32;
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

// ---- Reports ----------------------------------------------------------------

/// What a report covers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Query<'a> {
    /// The last this many days, today included; `None` for everything.
    pub days: Option<u32>,
    /// Only this project folder (empty for folders outside any project).
    pub project: Option<&'a str>,
    /// Only this agent, by its short name.
    pub agent: Option<&'a str>,
    /// The present, in seconds since 1970.
    pub now: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Input tokens, cached or not.
    pub input: u64,
    /// The part of the input read from the cache.
    pub cache_read: u64,
    pub output: u64,
    pub tasks: u64,
    pub turns: u64,
    /// Working time, in milliseconds.
    pub millis: u64,
}

impl Totals {
    fn add(&mut self, bucket: &Bucket) {
        for tokens in bucket.models.values() {
            self.input += tokens.all_input();
            self.cache_read += tokens.cache_read;
            self.output += tokens.output;
        }
        self.tasks += bucket.tasks;
        self.turns += bucket.turns;
        self.millis += bucket.millis;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelUse {
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub tasks: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentUse {
    /// The agent's short name.
    pub agent: String,
    pub input: u64,
    pub output: u64,
    pub millis: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectUse {
    /// The project folder; empty for folders outside any project.
    pub project: String,
    /// Input and output tokens together.
    pub tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// The first and last day covered, in local days since 1970.
    pub first_day: i64,
    pub today: i64,
    /// One entry per day from `first_day` to `today`.
    pub days: Vec<Totals>,
    pub total: Totals,
    /// The same number of days just before, when the query has a length.
    pub previous: Option<Totals>,
    /// The most conversations active within one hour.
    pub busiest_hour: u64,
    /// Working time by weekday (Monday first) and hour of the day, in
    /// milliseconds.
    pub heat: [[u64; 24]; 7],
    /// Most used first.
    pub models: Vec<ModelUse>,
    /// Every agent used in these days and this project, whichever agent
    /// the query asks for. Most used first.
    pub agents: Vec<AgentUse>,
    /// Every project used in these days, whichever the query asks for.
    /// Most used first.
    pub projects: Vec<ProjectUse>,
}

impl Store {
    /// Adds the totals up for a query. `utc_offset` gives the local time's
    /// distance from UTC, in seconds, at a time in seconds since 1970.
    pub fn report(&self, query: &Query, utc_offset: &dyn Fn(u64) -> i64) -> Report {
        // Local time of each bucket: its day and its hour of the day.
        let local = |hour: u64| {
            let seconds = hour * 3600;
            let local = seconds as i64 + utc_offset(seconds);
            (local.div_euclid(DAY), (local.rem_euclid(DAY) / 3600) as usize)
        };
        let today = (query.now as i64 + utc_offset(query.now)).div_euclid(DAY);
        let in_project = |bucket: &Bucket| query.project.is_none_or(|project| bucket.project == project);
        let by_agent = |bucket: &Bucket| query.agent.is_none_or(|agent| bucket.agent == agent);
        let first_day = match query.days {
            Some(days) => today - i64::from(days.max(1)) + 1,
            None => self.buckets.values().filter(|bucket| in_project(bucket)).map(|bucket| local(bucket.hour).0).min().unwrap_or(today),
        }
        .min(today);
        let length = today - first_day + 1;

        let mut report = Report {
            first_day,
            today,
            days: vec![Totals::default(); length as usize],
            previous: query.days.map(|_| Totals::default()),
            ..Report::default()
        };
        let mut models: BTreeMap<&str, ModelUse> = BTreeMap::new();
        let mut agents: BTreeMap<&str, AgentUse> = BTreeMap::new();
        let mut projects: BTreeMap<&str, u64> = BTreeMap::new();
        let mut active: BTreeMap<u64, u64> = BTreeMap::new();
        for bucket in self.buckets.values() {
            let (day, hour) = local(bucket.hour);
            if day > today {
                continue;
            }
            if day < first_day {
                if day >= first_day - length
                    && in_project(bucket)
                    && by_agent(bucket)
                    && let Some(previous) = &mut report.previous
                {
                    previous.add(bucket);
                }
                continue;
            }
            let mut sum = Totals::default();
            sum.add(bucket);
            *projects.entry(&bucket.project).or_default() += sum.input + sum.output;
            if !in_project(bucket) {
                continue;
            }
            let agent = agents.entry(&bucket.agent).or_default();
            agent.input += sum.input;
            agent.output += sum.output;
            agent.millis += sum.millis;
            if !by_agent(bucket) {
                continue;
            }
            report.total.add(bucket);
            report.days[(day - first_day) as usize].add(bucket);
            // 1970-01-01 was a Thursday.
            report.heat[(day + 3).rem_euclid(7) as usize][hour] += bucket.millis;
            *active.entry(bucket.hour).or_default() += bucket.sessions;
            for (name, tokens) in &bucket.models {
                let model = models.entry(name).or_default();
                model.input += tokens.all_input();
                model.output += tokens.output;
                model.tasks += tokens.tasks;
            }
        }
        report.busiest_hour = active.values().copied().max().unwrap_or(0);
        report.models = models.into_iter().map(|(name, model)| ModelUse { model: name.to_owned(), ..model }).collect();
        report.models.sort_by_key(|model| std::cmp::Reverse(model.input + model.output));
        report.agents = agents.into_iter().map(|(name, agent)| AgentUse { agent: name.to_owned(), ..agent }).collect();
        report.agents.sort_by_key(|agent| std::cmp::Reverse((agent.input + agent.output, agent.millis)));
        report.projects =
            projects.into_iter().map(|(project, tokens)| ProjectUse { project: project.to_owned(), tokens }).collect();
        report.projects.sort_by_key(|project| std::cmp::Reverse(project.tokens));
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(time: &str, content: &str) -> String {
        format!(r#"{{"type":"user","timestamp":"{time}","cwd":"/work/app/src","message":{{"role":"user","content":{content}}}}}"#)
    }

    fn reply(time: &str, id: &str, model: &str, stop: &str, tokens: [u64; 4]) -> String {
        let [input, write, read, output] = tokens;
        format!(
            r#"{{"type":"assistant","timestamp":"{time}","cwd":"/work/app/src","message":{{"id":"{id}","model":"{model}","stop_reason":"{stop}","content":[{{"type":"text","text":"hi"}}],"usage":{{"input_tokens":{input},"cache_creation_input_tokens":{write},"cache_read_input_tokens":{read},"output_tokens":{output},"service_tier":"standard"}}}}}}"#
        )
    }

    const TOOL_RESULT: &str = r#"[{"type":"tool_result","content":"ok"}]"#;

    /// A conversation: one prompt answered in two replies around a tool
    /// call, then a second prompt.
    fn conversation() -> Vec<String> {
        vec![
            r#"{"type":"mode","mode":"default"}"#.to_owned(),
            user("2026-11-20T10:00:00.000Z", r#""build it""#),
            reply("2026-11-20T10:00:10.000Z", "a", "claude-opus-5-5", "tool_use", [2, 100, 1000, 50]),
            // The same reply again, as its next content block.
            reply("2026-11-20T10:00:12.000Z", "a", "claude-opus-5-5", "tool_use", [2, 100, 1000, 50]),
            user("2026-11-20T10:00:20.000Z", TOOL_RESULT),
            reply("2026-11-20T10:00:30.000Z", "b", "claude-opus-5-5", "end_turn", [3, 0, 1100, 20]),
            // An hour passes before the next prompt: not working time.
            user("2026-11-20T11:30:00.000Z", r#"[{"type":"text","text":"and test it"},{"type":"image","source":{}}]"#),
            reply("2026-11-20T11:30:05.000Z", "c", "claude-fable-5-2", "end_turn", [1, 0, 2000, 10]),
        ]
    }

    fn project(_: &Path) -> Option<PathBuf> {
        Some("/work/app".into())
    }

    fn counted(lines: &[String], subagent: bool) -> Store {
        let mut store = Store::default();
        let mut mark = Mark::default();
        let mut projects = Projects { known: HashMap::new(), project_of: &project };
        for line in lines {
            store.claude_line(line, subagent, &mut mark, &mut projects);
        }
        store
    }

    fn noon(day: i64) -> u64 {
        (day * DAY + 12 * 3600) as u64
    }

    const NOV_20: i64 = 20_777;

    #[test]
    fn dates_convert_both_ways() {
        assert_eq!(days_from_date(1970, 1, 1), 0);
        assert_eq!(days_from_date(2026, 11, 20), NOV_20);
        assert_eq!(date_from_days(NOV_20), (2026, 11, 20));
        assert_eq!(date_from_days(days_from_date(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(date_from_days(-1), (1969, 12, 31));
        assert_eq!(parse_time("1970-01-01T00:00:01.500Z"), Some(1500));
        assert_eq!(parse_time("1970-01-02T00:00:00Z"), Some(86_400_000));
        assert_eq!(parse_time("yesterday"), None);
    }

    #[test]
    fn a_conversation_is_counted_once_per_reply() {
        let store = counted(&conversation(), false);
        let report = store.report(&Query { days: Some(7), now: noon(NOV_20), ..Query::default() }, &|_| 0);
        assert_eq!(
            report.total,
            Totals {
                input: 2 + 100 + 1000 + 3 + 1100 + 1 + 2000,
                cache_read: 4100,
                output: 80,
                tasks: 1,
                turns: 2,
                // Prompt to reply, reply to tool result, tool result to
                // reply, and the second prompt to its reply.
                millis: (12 + 8 + 10 + 5) * 1000,
            }
        );
        assert_eq!(report.busiest_hour, 1);
        assert_eq!(report.days.len(), 7);
        assert_eq!(report.days[6], report.total);
        let models: Vec<_> = report.models.iter().map(|m| (m.model.as_str(), m.output, m.tasks)).collect();
        assert_eq!(models, [("claude-opus-5-5", 70, 1), ("claude-fable-5-2", 10, 1)]);
        assert_eq!(report.agents.len(), 1);
        assert_eq!((report.agents[0].agent.as_str(), report.agents[0].millis), ("claude", 35_000));
        assert_eq!(report.projects, [ProjectUse { project: "/work/app".into(), tokens: report.total.input + 80 }]);
        // 2026-11-20 is a Friday.
        assert_eq!(report.heat[4][10], 30_000);
        assert_eq!(report.heat[4][11], 5000);
    }

    #[test]
    fn a_subagent_adds_tokens_but_no_task_turn_or_time() {
        let store = counted(&conversation(), true);
        let report = store.report(&Query { days: Some(1), now: noon(NOV_20), ..Query::default() }, &|_| 0);
        assert_eq!(report.total, Totals { input: 4206, cache_read: 4100, output: 80, ..Totals::default() });
        assert_eq!(report.busiest_hour, 0);
    }

    #[test]
    fn notes_and_unanswered_prompts_are_not_turns() {
        let mut lines = vec![
            // A command run in Claude Code itself: never answered.
            user("2026-11-20T09:00:00.000Z", r#""<command-name>/clear</command-name>""#),
            r#"{"type":"user","isMeta":true,"timestamp":"2026-11-20T09:00:01.000Z","message":{"content":"note"}}"#.to_owned(),
        ];
        lines.extend(conversation());
        // A message Claude Code wrote itself, and a line that is not JSON.
        lines.push(reply("2026-11-20T11:31:00.000Z", "d", "<synthetic>", "end_turn", [9, 9, 9, 9]));
        lines.push(r#"{"type":"assistant","#.to_owned());
        let report = counted(&lines, false).report(&Query { now: noon(NOV_20), ..Query::default() }, &|_| 0);
        assert_eq!((report.total.turns, report.total.tasks, report.total.output), (2, 1, 80));
    }

    #[test]
    fn a_long_wait_inside_a_turn_is_capped() {
        let lines = [
            user("2026-11-20T10:00:00.000Z", r#""go""#),
            reply("2026-11-20T10:00:01.000Z", "a", "m", "tool_use", [1, 0, 0, 1]),
            // The permission question sat there for three hours.
            user("2026-11-20T13:00:01.000Z", TOOL_RESULT),
        ];
        let report = counted(&lines, false).report(&Query { now: noon(NOV_20), ..Query::default() }, &|_| 0);
        assert_eq!(report.total.millis, 1000 + LONGEST_STEP_MS);
    }

    #[test]
    fn ranges_filters_and_the_days_before() {
        let mut store = Store::default();
        let mut add = |day: i64, agent: AgentKind, project: &str, output: u64| {
            let bucket = store.bucket((day * 24 + 12) as u64, agent, project);
            bucket.tasks += 1;
            bucket.sessions += 1;
            bucket.models.entry("m".into()).or_default().output += output;
        };
        add(NOV_20, AgentKind::Claude, "/a", 1);
        add(NOV_20, AgentKind::Codex, "/a", 2);
        add(NOV_20 - 1, AgentKind::Claude, "/b", 4);
        add(NOV_20 - 2, AgentKind::Claude, "/a", 8);
        add(NOV_20 - 40, AgentKind::Claude, "/a", 16);
        let report = |days, project, agent| {
            store.report(&Query { days, project, agent, now: noon(NOV_20) }, &|_| 0)
        };

        let two = report(Some(2), None, None);
        assert_eq!((two.total.output, two.total.tasks, two.busiest_hour), (7, 3, 2));
        assert_eq!(two.previous.map(|previous| previous.output), Some(8));
        assert_eq!(two.days.iter().map(|day| day.output).collect::<Vec<_>>(), [4, 3]);
        assert_eq!((two.first_day, two.today), (NOV_20 - 1, NOV_20));

        let everything = report(None, None, None);
        assert_eq!((everything.total.output, everything.days.len(), everything.previous), (31, 41, None));

        // One project: the other still shows in the list to choose from.
        let a = report(Some(2), Some("/a"), None);
        assert_eq!(a.total.output, 3);
        assert_eq!(a.projects.iter().map(|p| p.project.as_str()).collect::<Vec<_>>(), ["/b", "/a"]);
        // One agent: the other still shows in the agents table.
        let codex = report(Some(2), Some("/a"), Some("codex"));
        assert_eq!(codex.total.output, 2);
        assert_eq!(codex.agents.iter().map(|a| a.agent.as_str()).collect::<Vec<_>>(), ["codex", "claude"]);
        // Nothing at all: an empty report for today.
        let empty = Store::default().report(&Query { now: noon(NOV_20), ..Query::default() }, &|_| 0);
        assert_eq!((empty.days.len(), empty.total), (1, Totals::default()));
    }

    #[test]
    fn days_follow_local_time() {
        let mut store = Store::default();
        // 23:00 UTC on a Friday is already Saturday two hours to the east.
        store.bucket((NOV_20 * 24 + 23) as u64, AgentKind::Claude, "").millis = 5;
        let east = store.report(&Query { days: Some(1), now: noon(NOV_20 + 1), ..Query::default() }, &|_| 7200);
        assert_eq!((east.total.millis, east.heat[5][1]), (5, 5));
        let utc = store.report(&Query { days: Some(1), now: noon(NOV_20 + 1), ..Query::default() }, &|_| 0);
        assert_eq!(utc.total.millis, 0);
        assert_eq!(utc.previous.map(|previous| previous.millis), Some(5));
    }

    #[test]
    fn scanning_again_reads_only_what_is_new() {
        let dir = std::env::temp_dir().join(format!("ale-usage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let records = dir.join("projects");
        let session = records.join("-work-app");
        std::fs::create_dir_all(session.join("abc").join("subagents")).unwrap();
        let lines = conversation();
        let main = session.join("abc.jsonl");
        // The last line is still being written.
        std::fs::write(&main, format!("{}\n{}", lines[..6].join("\n"), &lines[6][..40])).unwrap();
        let helper = session.join("abc").join("subagents").join("agent-1.jsonl");
        std::fs::write(&helper, format!("{}\n", reply("2026-11-20T10:00:15.000Z", "s", "claude-haiku-5-5", "end_turn", [5, 0, 0, 7]))).unwrap();
        std::fs::write(session.join("notes.txt"), "not a record").unwrap();

        let project_of = |folder: &Path| folder.parent().map(Path::to_path_buf);
        let query = Query { now: noon(NOV_20), ..Query::default() };
        let mut store = Store::load(&dir.join("state"));
        assert!(store.is_empty());
        assert!(store.scan_records(&records, Reader::Claude, &mut Projects { known: HashMap::new(), project_of: &project_of }));
        let first = store.report(&query, &|_| 0);
        assert_eq!((first.total.turns, first.total.output, first.total.tasks), (1, 77, 1));
        assert_eq!(first.projects[0].project, "/work/app");
        assert!(!store.scan_records(&records, Reader::Claude, &mut Projects { known: HashMap::new(), project_of: &project_of }));
        store.save().unwrap();

        // The rest arrives; a fresh start picks up where the last left off.
        std::fs::write(&main, format!("{}\n", lines.join("\n"))).unwrap();
        let mut store = Store::load(&dir.join("state"));
        assert_eq!(store.report(&query, &|_| 0), first);
        assert!(store.scan_records(&records, Reader::Claude, &mut Projects { known: HashMap::new(), project_of: &project_of }));
        let second = store.report(&query, &|_| 0);
        assert_eq!((second.total.turns, second.total.output, second.total.tasks), (2, 87, 1));
        assert_eq!(second.total.millis, 35_000);
        assert_eq!(second.models.len(), 3);

        // Records that are removed keep their totals.
        std::fs::remove_file(&main).unwrap();
        assert!(store.scan_records(&records, Reader::Claude, &mut Projects { known: HashMap::new(), project_of: &project_of }));
        assert_eq!(store.report(&query, &|_| 0), second);
        assert_eq!(store.files.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn projects_by_parent() -> impl Fn(&Path) -> Option<PathBuf> {
        |folder: &Path| folder.parent().map(Path::to_path_buf)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ale-usage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn codex_totals_are_counted_as_they_grow() {
        let count = |time: &str, total: [u64; 3], last: [u64; 3]| {
            let usage = |[input, cached, output]: [u64; 3]| {
                format!(r#"{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output},"reasoning_output_tokens":1,"total_tokens":0}}"#)
            };
            format!(
                r#"{{"timestamp":"{time}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{},"last_token_usage":{},"model_context_window":1}},"rate_limits":null}}}}"#,
                usage(total),
                usage(last)
            )
        };
        let event = |time: &str, kind: &str| format!(r#"{{"timestamp":"{time}","type":"event_msg","payload":{{"type":"{kind}"}}}}"#);
        let lines = [
            r#"{"timestamp":"2026-11-20T10:00:00.000Z","type":"session_meta","payload":{"id":"s","cwd":"/work/app/src","originator":"codex_cli"}}"#.to_owned(),
            r#"{"timestamp":"2026-11-20T10:00:00.000Z","type":"turn_context","payload":{"cwd":"/work/app/src","model":"gpt-6.2-astra"}}"#.to_owned(),
            event("2026-11-20T10:00:01.000Z", "task_started"),
            event("2026-11-20T10:00:01.000Z", "user_message"),
            // Nothing is known yet.
            r#"{"timestamp":"2026-11-20T10:00:02.000Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#.to_owned(),
            count("2026-11-20T10:00:11.000Z", [1000, 800, 50], [1000, 800, 50]),
            r#"{"timestamp":"2026-11-20T10:00:15.000Z","type":"response_item","payload":{"type":"function_call_output"}}"#.to_owned(),
            count("2026-11-20T10:00:21.000Z", [2500, 2000, 80], [1500, 1200, 30]),
            // The same totals again add nothing.
            count("2026-11-20T10:00:21.500Z", [2500, 2000, 80], [1500, 1200, 30]),
            event("2026-11-20T10:00:23.000Z", "task_complete"),
            // After a long pause, a second turn; the totals started over.
            event("2026-11-20T12:00:00.000Z", "user_message"),
            count("2026-11-20T12:00:04.000Z", [400, 100, 7], [400, 100, 7]),
            event("2026-11-20T12:00:05.000Z", "turn_aborted"),
        ];
        let dir = scratch("codex");
        let sessions = dir.join(".codex").join("sessions").join("2026").join("11").join("20");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("rollout-2026-11-20T10-00-00-abc.jsonl"), format!("{}\n", lines.join("\n"))).unwrap();
        std::fs::write(sessions.join("other.jsonl"), format!("{}\n", lines.join("\n"))).unwrap();

        let mut store = Store::default();
        assert!(store.scan(&dir, None, &projects_by_parent()));
        let report = store.report(&Query { now: noon(NOV_20), ..Query::default() }, &|_| 0);
        assert_eq!(
            report.total,
            Totals { input: 2900, cache_read: 2100, output: 87, tasks: 1, turns: 2, millis: 22_000 + 5000 }
        );
        assert_eq!(report.agents[0].agent, "codex");
        assert_eq!((report.models[0].model.as_str(), report.models[0].tasks), ("gpt-6.2-astra", 1));
        assert_eq!(report.projects[0].project, "/work/app");
        assert!(!store.scan(&dir, None, &projects_by_parent()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gemini_messages_written_again_are_counted_once() {
        let message = |id: &str, time: &str, kind: &str, tokens: &str| {
            format!(r#"{{"id":"{id}","timestamp":"{time}","type":"{kind}","content":"x","model":"gemini-4-argon"{tokens}}}"#)
        };
        let tokens = r#","tokens":{"input":1000,"output":40,"cached":600,"thoughts":10,"tool":0,"total":1050}"#;
        let lines = [
            r#"{"sessionId":"s","projectHash":"h","startTime":"2026-11-20T10:00:00.000Z","lastUpdated":"2026-11-20T10:00:00.000Z","kind":"main"}"#.to_owned(),
            message("u1", "2026-11-20T10:00:00.000Z", "user", ""),
            // The reply is written as it starts, and again with its tokens.
            message("g1", "2026-11-20T10:00:08.000Z", "gemini", ""),
            message("g1", "2026-11-20T10:00:08.000Z", "gemini", tokens),
            r#"{"$set":{"lastUpdated":"2026-11-20T10:00:09.000Z"}}"#.to_owned(),
            message("g1", "2026-11-20T10:00:08.000Z", "gemini", tokens),
            message("i1", "2026-11-20T10:00:09.000Z", "info", ""),
        ];
        let dir = scratch("gemini");
        let project = dir.join(".gemini").join("tmp").join("app");
        let chats = project.join("chats");
        std::fs::create_dir_all(chats.join("s")).unwrap();
        std::fs::write(project.join(".project_root"), "/work/app/src\n").unwrap();
        std::fs::write(project.join("logs.json"), "[]").unwrap();
        std::fs::write(chats.join("session-2026-11-20T10-00-abcdefgh.jsonl"), format!("{}\n", lines.join("\n"))).unwrap();
        // A subagent of that conversation: tokens only.
        let helper = [message("u9", "2026-11-20T10:00:02.000Z", "user", ""), message("g9", "2026-11-20T10:00:03.000Z", "gemini", tokens)];
        std::fs::write(chats.join("s").join("sub.jsonl"), format!("{}\n", helper.join("\n"))).unwrap();
        // An older record: one document, in a folder that does not say
        // where it ran.
        let old = dir.join(".gemini").join("tmp").join("0123abcd").join("chats");
        std::fs::create_dir_all(&old).unwrap();
        let document = |messages: &[String]| format!(r#"{{"sessionId":"o","messages":[{}]}}"#, messages.join(","));
        let first = [message("u1", "2026-11-19T09:00:00.000Z", "user", ""), message("g1", "2026-11-19T09:00:02.000Z", "gemini", tokens)];
        std::fs::write(old.join("session-2026-11-19T09-00-o.json"), document(&first)).unwrap();

        let query = Query { now: noon(NOV_20), ..Query::default() };
        let mut store = Store::default();
        assert!(store.scan(&dir, None, &projects_by_parent()));
        let report = store.report(&query, &|_| 0);
        assert_eq!(
            report.total,
            Totals { input: 3000, cache_read: 1800, output: 150, tasks: 2, turns: 2, millis: 8000 + 2000 }
        );
        assert_eq!(report.agents[0].agent, "gemini");
        let projects: Vec<_> = report.projects.iter().map(|project| project.project.as_str()).collect();
        assert_eq!(projects, ["/work/app", ""]);
        assert!(!store.scan(&dir, None, &projects_by_parent()));

        // The old conversation goes on: only its new messages are counted.
        let mut longer = first.to_vec();
        longer.push(message("u2", "2026-11-19T09:10:00.000Z", "user", ""));
        longer.push(message("g2", "2026-11-19T09:10:01.000Z", "gemini", tokens));
        std::fs::write(old.join("session-2026-11-19T09-00-o.json"), document(&longer)).unwrap();
        assert!(store.scan(&dir, None, &projects_by_parent()));
        let report = store.report(&query, &|_| 0);
        assert_eq!((report.total.output, report.total.tasks, report.total.turns), (200, 2, 3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_journal_counts_agents_whose_records_cannot_be_read() {
        assert!(journaled(AgentKind::Kiro) && journaled(AgentKind::OpenCode) && !journaled(AgentKind::Claude));
        let dir = scratch("journal");
        let journal = dir.join("state").join(JOURNAL);
        let time = (NOV_20 * DAY + 10 * 3600) as u64 * 1000;
        let stretch = Stretch {
            time,
            agent: "kiro".into(),
            cwd: Some("/work/app/src".into()),
            millis: 60_000,
            task: true,
            turn: true,
            active: true,
        };
        append_stretch(&journal, &stretch).unwrap();
        append_stretch(&journal, &Stretch { time: time + 90_000, millis: 30_000, task: false, active: false, ..stretch.clone() }).unwrap();
        append_stretch(&journal, &Stretch { agent: "nobody".into(), ..stretch.clone() }).unwrap();

        let query = Query { now: noon(NOV_20), ..Query::default() };
        let mut store = Store::default();
        assert!(store.scan(&dir, Some(&journal), &projects_by_parent()));
        let report = store.report(&query, &|_| 0);
        assert_eq!(report.total, Totals { tasks: 1, turns: 2, millis: 90_000, ..Totals::default() });
        assert_eq!(report.agents, [AgentUse { agent: "kiro".into(), input: 0, output: 0, millis: 90_000 }]);
        assert_eq!((report.busiest_hour, report.models.len()), (1, 0));
        // More is added later; what was read is not read again.
        assert!(!store.scan(&dir, Some(&journal), &projects_by_parent()));
        append_stretch(&journal, &Stretch { time: time + 200_000, millis: 1000, task: false, ..stretch }).unwrap();
        assert!(store.scan(&dir, Some(&journal), &projects_by_parent()));
        assert_eq!(store.report(&query, &|_| 0).total.millis, 91_000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
