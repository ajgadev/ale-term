//! Reading an agent's conversation record to show what it has been doing.
//!
//! Claude Code writes each conversation (and each subagent's, separately)
//! as a file of JSON lines. This module turns the end of such a file into a
//! few readable lines: what the agent said and which tools it called.
//!
//! The file format is internal to Claude Code and may change. Everything
//! here is best effort: lines that are not understood are skipped, and a
//! file that cannot be read simply yields nothing.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::hooks_claude::describe;
use crate::session::clean;

/// How much of the end of the file is read. Plenty for the recent
/// activity, and bounded however long the conversation is.
const TAIL_BYTES: u64 = 256 * 1024;

/// One line of the output view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// What the agent was asked.
    Prompt(String),
    /// Something the agent said.
    Text(String),
    /// A tool call, described briefly ("Reading src/main.rs").
    Tool(String),
}

/// The whole records at the end of a transcript, at most [`TAIL_BYTES`]
/// of them.
fn read_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().map_or(0, |meta| meta.len());
    let start = length.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        // We began mid-line; the first piece is not a whole record.
        let first = text.find('\n').map_or(text.len(), |at| at + 1);
        text.drain(..first);
    }
    Some(text)
}

/// Reads the most recent entries of a transcript, oldest first.
pub fn tail(path: &Path, max_entries: usize) -> Vec<Entry> {
    let Some(text) = read_tail(path) else { return Vec::new() };
    let mut entries = Vec::new();
    for line in text.lines() {
        if let Ok(record) = serde_json::from_str::<Value>(line) {
            read_record(&record, &mut entries);
        }
    }
    let excess = entries.len().saturating_sub(max_entries);
    entries.drain(..excess);
    entries
}

/// How big a conversation has grown, going by its latest reply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// The model that wrote the reply, as the agent names it
    /// ("claude-opus-5-5").
    pub model: Option<String>,
    /// Tokens in the conversation after the reply: everything the model
    /// was given (cached or not) plus what it wrote.
    pub context: u64,
}

/// The size of a conversation after its latest reply, from the usage
/// Claude Code records with each one. `None` before the first reply.
pub fn usage(path: &Path) -> Option<Usage> {
    let text = read_tail(path)?;
    text.lines().rev().filter(|line| line.contains("\"usage\"")).find_map(|line| {
        let record = serde_json::from_str::<Value>(line).ok()?;
        if record["type"] != "assistant" {
            return None;
        }
        let message = &record["message"];
        let used = &message["usage"];
        let count = |key: &str| used[key].as_u64().unwrap_or(0);
        let context = count("input_tokens")
            + count("cache_creation_input_tokens")
            + count("cache_read_input_tokens")
            + count("output_tokens");
        // Messages the agent makes up itself (an interrupted turn) report
        // nothing.
        let model = message["model"].as_str().filter(|model| !model.starts_with('<'));
        (context > 0).then(|| Usage { model: model.map(|model| clean(model, 60)), context })
    })
}

fn read_record(record: &Value, out: &mut Vec<Entry>) {
    let cwd = record["cwd"].as_str();
    let content = &record["message"]["content"];
    match record["type"].as_str() {
        Some("user") => {
            // A plain string is a prompt; a list holds tool results, which
            // would only repeat what the tool lines already say.
            if let Some(prompt) = content.as_str() {
                push_text(prompt, out, Entry::Prompt);
            }
        }
        Some("assistant") => {
            for item in content.as_array().into_iter().flatten() {
                match item["type"].as_str() {
                    Some("text") => push_text(item["text"].as_str().unwrap_or(""), out, Entry::Text),
                    Some("tool_use") => {
                        let name = item["name"].as_str().unwrap_or("tool");
                        out.push(Entry::Tool(describe(name, &item["input"], cwd)));
                    }
                    // Private reasoning is not shown.
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Adds the non-empty lines of `text`, cleaned and capped.
fn push_text(text: &str, out: &mut Vec<Entry>, make: fn(String) -> Entry) {
    for line in text.lines().filter(|line| !line.trim().is_empty()).take(40) {
        out.push(make(clean(line, 400)));
    }
}

/// What the agent said last: the lines of its latest message, as one
/// paragraph. At the end of a turn this is usually its summary of what it
/// did. `None` when the transcript holds nothing it said.
pub fn last_message(path: &Path) -> Option<String> {
    let entries = tail(path, 200);
    let end = entries.iter().rposition(|entry| matches!(entry, Entry::Text(_)))?;
    let start = entries[..end].iter().rposition(|entry| !matches!(entry, Entry::Text(_))).map_or(0, |at| at + 1);
    let lines: Vec<&str> = entries[start..=end]
        .iter()
        .filter_map(|entry| match entry {
            Entry::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    Some(clean(&lines.join(" "), 600))
}

/// Size and modification time of a transcript, for noticing changes
/// without rereading it.
pub fn stamp(path: &Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_last_message_is_the_latest_run_of_text() {
        let say = |text: &str| json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": text }] } });
        let tool = json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "Bash", "input": { "command": "ls" } }] } });
        let path = write("last", &[say("Looking around."), tool.clone(), say("Done: the cards line up.\nTests pass."), tool]);
        assert_eq!(last_message(&path).as_deref(), Some("Done: the cards line up. Tests pass."));
        let quiet = write("quiet", &[json!({ "type": "user", "message": { "content": "hi" } })]);
        assert_eq!(last_message(&quiet), None);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(quiet);
    }

    fn write(name: &str, records: &[Value]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("ale-term-transcript-{name}-{}.jsonl", std::process::id()));
        let text: String = records.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(&path, text).unwrap();
        path
    }

    /// Records shaped like the ones Claude Code writes.
    fn sample() -> Vec<Value> {
        vec![
            json!({ "type": "user", "cwd": "/repo", "message": { "role": "user", "content": "Find where redirects are built\n\nBe brief." } }),
            json!({ "type": "attachment", "attachment": { "kind": "whatever" } }),
            json!({ "type": "assistant", "cwd": "/repo", "message": { "role": "assistant", "content": [
                { "type": "thinking", "thinking": "private", "signature": "x" }
            ] } }),
            json!({ "type": "assistant", "cwd": "/repo", "message": { "role": "assistant", "content": [
                { "type": "tool_use", "id": "t1", "name": "Grep", "input": { "pattern": "redirect" } }
            ] } }),
            json!({ "type": "user", "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "src/auth.rs:10: redirect(...)" }
            ] } }),
            json!({ "type": "assistant", "cwd": "/repo", "message": { "role": "assistant", "content": [
                { "type": "tool_use", "id": "t2", "name": "Read", "input": { "file_path": "/repo/src/auth.rs" } },
                { "type": "text", "text": "Redirects are built in `src/auth.rs`.\n" }
            ] } }),
        ]
    }

    #[test]
    fn shows_prompts_text_and_tool_calls_but_not_reasoning_or_results() {
        let path = write("sample", &sample());
        assert_eq!(
            tail(&path, 50),
            [
                Entry::Prompt("Find where redirects are built".into()),
                Entry::Prompt("Be brief.".into()),
                Entry::Tool("Searching for redirect".into()),
                Entry::Tool("Reading src/auth.rs".into()),
                Entry::Text("Redirects are built in `src/auth.rs`.".into()),
            ]
        );
        // Only the most recent entries when asked for fewer.
        assert_eq!(tail(&path, 2).len(), 2);
        assert_eq!(tail(&path, 2)[1], Entry::Text("Redirects are built in `src/auth.rs`.".into()));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn usage_comes_from_the_latest_reply() {
        let reply = |model: &str, tokens: [u64; 4]| {
            let [input, write, read, output] = tokens;
            json!({ "type": "assistant", "message": { "model": model, "content": [{ "type": "text", "text": "hi" }],
                "usage": { "input_tokens": input, "cache_creation_input_tokens": write, "cache_read_input_tokens": read, "output_tokens": output } } })
        };
        let mut records = sample();
        records.push(reply("claude-sonnet-5-5", [10, 20, 30, 40]));
        records.push(reply("claude-opus-5-5", [3, 1_000, 340_000, 900]));
        // An interrupted turn, and a prompt after it, change nothing.
        records.push(reply("<synthetic>", [0, 0, 0, 0]));
        records.push(json!({ "type": "user", "message": { "content": "next" } }));
        let path = write("usage", &records);
        assert_eq!(usage(&path), Some(Usage { model: Some("claude-opus-5-5".into()), context: 341_903 }));
        // Nothing replied yet, or no file at all.
        let quiet = write("usage-quiet", &sample());
        assert_eq!(usage(&quiet), None);
        assert_eq!(usage(Path::new("/nonexistent/x.jsonl")), None);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(quiet);
    }

    #[test]
    fn broken_and_missing_files_yield_nothing() {
        assert!(tail(Path::new("/nonexistent/x.jsonl"), 10).is_empty());
        let path = std::env::temp_dir().join(format!("ale-term-transcript-bad-{}.jsonl", std::process::id()));
        std::fs::write(&path, "not json\n{\"type\":\"assistant\"}\n{ broken\n").unwrap();
        assert!(tail(&path, 10).is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn long_files_are_read_from_the_end_only() {
        // Far more than TAIL_BYTES of filler, then one last message.
        let filler = json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "x".repeat(900) }] } });
        let mut records = vec![filler; 400];
        records.push(json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "the end" }] } }));
        let path = write("long", &records);
        assert!(std::fs::metadata(&path).unwrap().len() > TAIL_BYTES);
        let entries = tail(&path, 1000);
        assert_eq!(entries.last(), Some(&Entry::Text("the end".into())));
        // Fewer than all 401: the start of the file was skipped, and the
        // record cut in half by that was not misread.
        assert!(entries.len() < 401 && entries.len() > 100, "{}", entries.len());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn hostile_text_is_cleaned() {
        let path = write("hostile", &[json!({ "type": "assistant", "message": { "content": [
            { "type": "text", "text": "ok \u{1b}]0;title\u{7} done" }
        ] } })]);
        let entries = tail(&path, 10);
        let Entry::Text(text) = &entries[0] else { panic!() };
        assert!(!text.chars().any(char::is_control), "{text:?}");
        let _ = std::fs::remove_file(path);
    }
}
