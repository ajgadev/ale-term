//! Turning any supported agent's hook events into session state.
//!
//! Claude Code and Codex use the same event names and nearly the same
//! payloads, so one implementation (`hooks_claude`) serves both. Other
//! agents are translated to that vocabulary first: Kiro shares the event
//! names but has its own tools; Gemini CLI names everything differently.
//!
//! Only the Claude Code mapping has been exercised against the real agent.
//! The others follow each agent's published hook documentation.

use std::time::Instant;

use serde_json::Value;

use crate::hooks_claude;
use crate::session::{AgentKind, Change, Session};

pub fn apply(session: &mut Session, event: &str, payload: &Value, now: Instant) -> Change {
    let event = if event.is_empty() { payload["hook_event_name"].as_str().unwrap_or("") } else { event };
    match session.kind {
        AgentKind::Claude => hooks_claude::apply(session, event, payload, now),
        AgentKind::Kiro => {
            let (event, payload) = translate_kiro(event, payload);
            hooks_claude::apply(session, &event, &payload, now)
        }
        AgentKind::Codex => {
            // Codex adds an event for the user interrupting a turn, which
            // leaves it waiting for input like a finished turn does.
            let event = if event == "Interrupt" { "Stop" } else { event };
            hooks_claude::apply(session, event, payload, now)
        }
        AgentKind::Gemini => {
            let (event, payload) = translate_gemini(event, payload);
            hooks_claude::apply(session, event, &payload, now)
        }
        // OpenCode has no command hooks; a plugin could send these names.
        AgentKind::OpenCode => hooks_claude::apply(session, event, payload, now),
    }
}

/// Kiro uses Claude Code's event names, though older versions (and some
/// payloads) spell them in camelCase (`preToolUse`, `agentSpawn`). Its
/// built-in tools have names of their own; the common ones are renamed to
/// their Claude Code counterparts so they are described the same way.
fn translate_kiro(event: &str, payload: &Value) -> (String, Value) {
    let event = match event {
        "agentSpawn" | "AgentSpawn" => "SessionStart".to_owned(),
        other => {
            let mut chars = other.chars();
            chars.next().map_or_else(String::new, |first| first.to_ascii_uppercase().to_string() + chars.as_str())
        }
    };
    let mut payload = payload.clone();
    let tool = payload["tool_name"].as_str().unwrap_or("").to_owned();
    let input = &payload["tool_input"];
    // Kiro's file tools take `path`, or a list of operations each with one.
    let path = input["path"].as_str().or_else(|| input["operations"][0]["path"].as_str()).unwrap_or("").to_owned();
    let renamed = match tool.as_str() {
        "fs_read" | "read" => Some(("Read", serde_json::json!({ "file_path": path }))),
        "fs_write" | "write" if input["command"] == "create" => {
            Some(("Write", serde_json::json!({ "file_path": path })))
        }
        "fs_write" | "write" => Some(("Edit", serde_json::json!({ "file_path": path }))),
        "execute_bash" | "execute_cmd" | "shell" => Some(("Bash", input.clone())),
        "web_fetch" => Some(("WebFetch", input.clone())),
        "web_search" => Some(("WebSearch", input.clone())),
        "grep" | "glob" => Some(("Grep", input.clone())),
        _ => None,
    };
    if let (Some((name, input)), Some(object)) = (renamed, payload.as_object_mut()) {
        object.insert("tool_name".into(), name.into());
        object.insert("tool_input".into(), input);
    }
    (event, payload)
}

/// Gemini CLI names its events differently and reports permission prompts
/// through its notification event.
fn translate_gemini<'a>(event: &'a str, payload: &Value) -> (&'a str, Value) {
    let mut payload = payload.clone();
    let event = match event {
        "BeforeAgent" => "UserPromptSubmit",
        "AfterAgent" => "Stop",
        "BeforeTool" => "PreToolUse",
        "AfterTool" => "PostToolUse",
        "PreCompress" => "PreCompact",
        "Notification" => {
            if payload["notification_type"] == "ToolPermission" {
                payload["notification_type"] = Value::from("permission_prompt");
            }
            "Notification"
        }
        other => other,
    };
    (event, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::AgentState;
    use serde_json::json;

    #[test]
    fn gemini_events_are_translated() {
        let now = Instant::now();
        let mut session = Session::new(AgentKind::Gemini, now);
        let submit = json!({ "hook_event_name": "BeforeAgent", "prompt": "refactor" });
        assert_eq!(apply(&mut session, "", &submit, now), Change::State(AgentState::Working));
        let ask = json!({ "notification_type": "ToolPermission", "message": "Allow write?" });
        assert_eq!(apply(&mut session, "Notification", &ask, now), Change::State(AgentState::NeedsYou));
        assert_eq!(session.activity.as_deref(), Some("Allow write?"));
        assert_eq!(apply(&mut session, "AfterTool", &json!({}), now), Change::State(AgentState::Working));
        assert_eq!(apply(&mut session, "AfterAgent", &json!({}), now), Change::State(AgentState::Done));
    }

    #[test]
    fn kiro_events_and_tools_are_translated() {
        let now = Instant::now();
        let mut session = Session::new(AgentKind::Kiro, now);
        let submit = json!({ "prompt": "tidy the docs" });
        assert_eq!(apply(&mut session, "userPromptSubmit", &submit, now), Change::State(AgentState::Working));
        let write = json!({ "cwd": "/repo", "tool_name": "fs_write", "tool_input": { "command": "str_replace", "path": "/repo/README.md" } });
        apply(&mut session, "PreToolUse", &write, now);
        assert_eq!(session.activity.as_deref(), Some("Editing README.md"));
        let shell = json!({ "tool_name": "execute_bash", "tool_input": { "command": "cargo test" } });
        apply(&mut session, "preToolUse", &shell, now);
        assert_eq!(session.activity.as_deref(), Some("Running cargo test"));
        let read = json!({ "cwd": "/repo", "tool_name": "fs_read", "tool_input": { "operations": [{ "mode": "Line", "path": "/repo/src/lib.rs" }] } });
        apply(&mut session, "PreToolUse", &read, now);
        assert_eq!(session.activity.as_deref(), Some("Reading src/lib.rs"));
        // Unknown tools keep their own name.
        apply(&mut session, "PreToolUse", &json!({ "tool_name": "use_aws", "tool_input": {} }), now);
        assert_eq!(session.activity.as_deref(), Some("Using use_aws"));
        let ask = json!({ "tool_name": "execute_bash", "tool_input": { "command": "rm -rf build" }, "permission": { "capability": "shell" } });
        assert_eq!(apply(&mut session, "PermissionRequest", &ask, now), Change::State(AgentState::NeedsYou));
        assert_eq!(apply(&mut session, "stop", &json!({}), now), Change::State(AgentState::Done));
        assert_eq!(translate_kiro("agentSpawn", &json!({})).0, "SessionStart");
        assert_eq!(translate_kiro("", &json!({})).0, "");
    }

    #[test]
    fn codex_and_kiro_share_the_claude_vocabulary() {
        let now = Instant::now();
        for kind in [AgentKind::Codex, AgentKind::Kiro] {
            let mut session = Session::new(kind, now);
            apply(&mut session, "UserPromptSubmit", &json!({ "prompt": "go" }), now);
            let request = json!({ "tool_name": "Bash", "tool_input": { "command": "ls" } });
            assert_eq!(apply(&mut session, "PermissionRequest", &request, now), Change::State(AgentState::NeedsYou));
            assert_eq!(apply(&mut session, "Stop", &json!({}), now), Change::State(AgentState::Done));
        }
        let mut codex = Session::new(AgentKind::Codex, now);
        apply(&mut codex, "UserPromptSubmit", &json!({}), now);
        assert_eq!(apply(&mut codex, "Interrupt", &json!({}), now), Change::State(AgentState::Done));
    }
}
