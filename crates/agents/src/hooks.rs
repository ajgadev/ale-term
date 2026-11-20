//! Turning any supported agent's hook events into session state.
//!
//! Claude Code, Codex and Kiro use the same event names and nearly the same
//! payloads, so one implementation (`hooks_claude`) serves all three. Other
//! agents are translated to that vocabulary first.
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
        AgentKind::Claude | AgentKind::Kiro => hooks_claude::apply(session, event, payload, now),
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
