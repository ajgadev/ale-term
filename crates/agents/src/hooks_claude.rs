//! Claude Code: turning its hook events into session state.
//!
//! Field names follow Claude Code's hooks reference. A few payloads are not
//! fully documented, so lookups try the plausible names and missing fields
//! are simply skipped: an unknown event or field never does harm.

use std::time::Instant;

use serde_json::Value;

use crate::session::{AgentState, Change, Session, clean, short_path, text};

/// Applies one hook event. `event` is the hook event name; when empty it is
/// read from the payload's `hook_event_name`.
pub fn apply(session: &mut Session, event: &str, payload: &Value, now: Instant) -> Change {
    session.note_common(payload);
    let event = if event.is_empty() { payload["hook_event_name"].as_str().unwrap_or("") } else { event };
    if let Some(title) = text(payload, &["session_title"]) {
        session.title = Some(clean(&title, 80));
    }

    match event {
        "SessionStart" => {
            session.activity = None;
            session.set_state(AgentState::Idle, now)
        }
        "UserPromptSubmit" => {
            session.new_turn();
            if let Some(prompt) = text(payload, &["prompt"]) {
                // The first line of the first prompt names the task until
                // the agent provides a better title.
                session.suggest_title(prompt.lines().find(|line| !line.trim().is_empty()).unwrap_or(""));
            }
            session.set_activity("Thinking");
            session.set_state(AgentState::Working, now)
        }
        "PreToolUse" => {
            // Inside a subagent the event carries that subagent's id.
            let caller = text(payload, &["agent_id"]);
            // A request to launch a subagent: remember who asked and what
            // for, since the start report that follows says neither.
            if matches!(payload["tool_name"].as_str(), Some("Agent" | "Task")) {
                let description = payload["tool_input"]["description"].as_str().unwrap_or("");
                session.launch_requested(caller.clone(), description);
            }
            match caller {
                // What a subagent does is its own activity; the main
                // agent's state is not affected.
                Some(id) => {
                    session.subagent_activity(&id, describe_tool(payload));
                    Change::Updated
                }
                None => {
                    session.set_activity(describe_tool(payload));
                    session.set_state(AgentState::Working, now)
                }
            }
        }
        "PostToolUse" | "PostToolUseFailure" | "PostToolBatch" | "PermissionDenied" | "ElicitationResult" => {
            if payload.get("agent_id").is_some_and(|id| !id.is_null()) {
                return Change::None;
            }
            // Also covers "the user answered the prompt": work resumed.
            session.set_state(AgentState::Working, now)
        }
        "PermissionRequest" => {
            session.set_activity(format!("Permission: {}", describe_tool(payload)));
            session.set_state(AgentState::NeedsYou, now)
        }
        "Elicitation" => {
            session.set_activity(text(payload, &["message"]).unwrap_or_else(|| "Has a question".into()));
            session.set_state(AgentState::NeedsYou, now)
        }
        "Notification" => notification(session, payload, now),
        "PreCompact" => {
            session.set_activity("Compacting the conversation");
            Change::Updated
        }
        "SubagentStart" => {
            let id = text(payload, &["agent_id"]).unwrap_or_default();
            let kind = text(payload, &["agent_type"]).unwrap_or_else(|| "subagent".into());
            if !id.is_empty() {
                session.subagent_started(&id, &kind, now);
            }
            Change::Updated
        }
        "SubagentStop" => {
            if let Some(id) = text(payload, &["agent_id"]) {
                if let Some(path) = text(payload, &["agent_transcript_path"]) {
                    session.subagent_transcript_reported(&id, &path);
                }
                session.subagent_finished(&id, text(payload, &["last_assistant_message"]), now);
            }
            session.note_background_tasks(payload);
            // The main agent had stopped and was only waiting for these.
            if session.state == AgentState::WaitingOnSubagents && session.running_subagents() == 0 {
                return session.set_state(AgentState::Done, now);
            }
            Change::Updated
        }
        "Stop" => {
            session.note_background_tasks(payload);
            if session.running_subagents() > 0 {
                session.set_activity(format!("{} subagents still running", session.running_subagents()));
                session.set_state(AgentState::WaitingOnSubagents, now)
            } else {
                session.activity = None;
                session.set_state(AgentState::Done, now)
            }
        }
        "StopFailure" => {
            // The failure kind's field name is not documented; look in the
            // likely places.
            let reason = text(payload, &["error_type", "error", "reason", "failure_type", "matcher"])
                .unwrap_or_else(|| "unknown".into());
            let rate_limited = reason.contains("rate_limit") || reason.contains("billing") || reason.contains("quota");
            session.set_activity(reason.replace('_', " "));
            session.set_state(if rate_limited { AgentState::RateLimited } else { AgentState::Error }, now)
        }
        "SessionEnd" => Change::Ended,
        _ => Change::None,
    }
}

fn notification(session: &mut Session, payload: &Value, now: Instant) -> Change {
    let kind = text(payload, &["notification_type", "type"]).unwrap_or_default();
    let message = text(payload, &["message", "title"]);
    match kind.as_str() {
        "permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input" => {
            session.set_activity(message.unwrap_or_else(|| "Waiting for your answer".into()));
            session.set_state(AgentState::NeedsYou, now)
        }
        "agent_completed" => session.set_state(AgentState::Done, now),
        "quota_auto_resume_fired" => session.set_state(AgentState::Working, now),
        "quota_auto_resume_stale" | "quota_auto_resume_disabled" => {
            session.set_activity(message.unwrap_or_else(|| "Usage limit reached".into()));
            session.set_state(AgentState::RateLimited, now)
        }
        // "idle_prompt" (still waiting for the next prompt) and the rest
        // carry nothing new.
        _ => Change::None,
    }
}

/// A short description of a tool call, e.g. "Editing src/main.rs".
fn describe_tool(payload: &Value) -> String {
    let tool = payload["tool_name"].as_str().unwrap_or("tool");
    describe(tool, &payload["tool_input"], payload["cwd"].as_str())
}

/// A short description of a call to `tool` with `input`.
pub(crate) fn describe(tool: &str, input: &Value, cwd: Option<&str>) -> String {
    let field = |key: &str| input[key].as_str().unwrap_or("");
    let path = |key: &str| short_path(field(key), cwd);
    match tool {
        "Bash" => format!("Running {}", clean(field("command"), 80)),
        "Read" => format!("Reading {}", path("file_path")),
        "Edit" | "MultiEdit" => format!("Editing {}", path("file_path")),
        "Write" => format!("Writing {}", path("file_path")),
        "NotebookEdit" => format!("Editing {}", path("notebook_path")),
        "Grep" | "Glob" => format!("Searching for {}", clean(field("pattern"), 60)),
        "WebFetch" => format!("Fetching {}", clean(field("url"), 80)),
        "WebSearch" => format!("Searching the web for {}", clean(field("query"), 60)),
        "Task" | "Agent" => format!("Delegating: {}", clean(field("description"), 80)),
        "TodoWrite" => "Updating the task list".into(),
        other => format!("Using {}", clean(other, 40)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::AgentKind;
    use serde_json::json;
    use std::time::Duration;

    fn session() -> (Session, Instant) {
        let now = Instant::now();
        (Session::new(AgentKind::Claude, now), now)
    }

    #[test]
    fn a_turn_goes_working_then_done() {
        let (mut s, now) = session();
        let base = json!({ "session_id": "abc", "cwd": "/repo", "transcript_path": "/t/abc.jsonl" });

        assert_eq!(apply(&mut s, "SessionStart", &base, now), Change::Updated);
        assert_eq!(s.state, AgentState::Idle);

        let prompt = json!({ "cwd": "/repo", "prompt": "\n  Fix the login bug\nand add tests" });
        assert_eq!(apply(&mut s, "UserPromptSubmit", &prompt, now), Change::State(AgentState::Working));
        assert_eq!(s.title.as_deref(), Some("Fix the login bug"));
        assert_eq!(s.activity.as_deref(), Some("Thinking"));

        let edit = json!({ "cwd": "/repo", "tool_name": "Edit", "tool_input": { "file_path": "/repo/src/auth.rs" } });
        assert_eq!(apply(&mut s, "PreToolUse", &edit, now), Change::Updated);
        assert_eq!(s.activity.as_deref(), Some("Editing src/auth.rs"));

        assert_eq!(apply(&mut s, "Stop", &base, now), Change::State(AgentState::Done));
        assert_eq!(s.activity, None);
        assert!(s.hooked);
        assert_eq!(s.transcript.as_deref(), Some(std::path::Path::new("/t/abc.jsonl")));

        // A later prompt does not replace the title.
        apply(&mut s, "UserPromptSubmit", &json!({ "prompt": "thanks" }), now);
        assert_eq!(s.title.as_deref(), Some("Fix the login bug"));
        // But the agent's own title does.
        apply(&mut s, "PostToolUse", &json!({ "session_title": "Login bug fix" }), now);
        assert_eq!(s.title.as_deref(), Some("Login bug fix"));
    }

    #[test]
    fn event_name_can_come_from_the_payload() {
        let (mut s, now) = session();
        let payload = json!({ "hook_event_name": "UserPromptSubmit", "prompt": "hi" });
        assert_eq!(apply(&mut s, "", &payload, now), Change::State(AgentState::Working));
    }

    #[test]
    fn permission_prompts_need_the_user_until_work_resumes() {
        let (mut s, now) = session();
        apply(&mut s, "UserPromptSubmit", &json!({ "prompt": "deploy" }), now);

        let request = json!({ "tool_name": "Bash", "tool_input": { "command": "rm -rf build" } });
        assert_eq!(apply(&mut s, "PermissionRequest", &request, now), Change::State(AgentState::NeedsYou));
        assert_eq!(s.activity.as_deref(), Some("Permission: Running rm -rf build"));

        // The notification for the same prompt keeps the state.
        let notice = json!({ "notification_type": "permission_prompt", "message": "Claude needs your permission" });
        assert_eq!(apply(&mut s, "Notification", &notice, now), Change::Updated);
        assert_eq!(s.state, AgentState::NeedsYou);

        // Approved: the tool ran.
        assert_eq!(apply(&mut s, "PostToolUse", &request, now), Change::State(AgentState::Working));
        // An idle reminder changes nothing.
        assert_eq!(apply(&mut s, "Notification", &json!({ "notification_type": "idle_prompt" }), now), Change::None);
    }

    #[test]
    fn subagents_are_tracked_and_hold_the_session_open() {
        let (mut s, now) = session();
        apply(&mut s, "UserPromptSubmit", &json!({ "prompt": "audit" }), now);
        // The real sequence: the launch request names the task, the start
        // report names the subagent.
        let launch = |what: &str| json!({ "tool_name": "Agent", "tool_input": { "description": what } });
        let start = |id: &str| json!({ "agent_id": id, "agent_type": "Explore" });
        apply(&mut s, "PreToolUse", &launch("look around"), now);
        apply(&mut s, "SubagentStart", &start("a1"), now);
        apply(&mut s, "PreToolUse", &launch("check the tests"), now);
        apply(&mut s, "SubagentStart", &start("a2"), now);
        assert_eq!(s.running_subagents(), 2);
        assert_eq!((s.subagents[0].kind.as_str(), s.subagents[0].description.as_str()), ("Explore", "look around"));
        assert_eq!(s.subagents[1].description, "check the tests");
        assert_eq!(s.activity.as_deref(), Some("Delegating: check the tests"));

        // A subagent's own tool calls are its activity, not the main one's.
        let inner = json!({ "agent_id": "a1", "tool_name": "Read", "tool_input": { "file_path": "/x/y.rs" } });
        assert_eq!(apply(&mut s, "PreToolUse", &inner, now), Change::Updated);
        assert_eq!(s.subagents[0].activity.as_deref(), Some("Reading x/y.rs"));
        assert_eq!(s.activity.as_deref(), Some("Delegating: check the tests"));

        // A subagent launching one of its own becomes its parent.
        let nested = json!({ "agent_id": "a1", "tool_name": "Agent", "tool_input": { "description": "dig deeper" } });
        apply(&mut s, "PreToolUse", &nested, now);
        apply(&mut s, "SubagentStart", &start("a3"), now);
        assert_eq!(s.subagents[2].parent.as_deref(), Some("a1"));
        apply(&mut s, "SubagentStop", &json!({ "agent_id": "a3", "last_assistant_message": "found it" }), now);
        assert_eq!(s.subagents[2].result.as_deref(), Some("found it"));

        assert_eq!(apply(&mut s, "Stop", &json!({}), now), Change::State(AgentState::WaitingOnSubagents));
        apply(&mut s, "SubagentStop", &json!({ "agent_id": "a1" }), now);
        assert_eq!(s.state, AgentState::WaitingOnSubagents);
        let later = now + Duration::from_secs(3);
        assert_eq!(apply(&mut s, "SubagentStop", &json!({ "agent_id": "a2" }), later), Change::State(AgentState::Done));
    }

    #[test]
    fn failures_map_to_error_or_rate_limit() {
        let (mut s, now) = session();
        assert_eq!(apply(&mut s, "StopFailure", &json!({ "error_type": "rate_limit" }), now), Change::State(AgentState::RateLimited));
        assert_eq!(s.activity.as_deref(), Some("rate limit"));
        assert_eq!(apply(&mut s, "StopFailure", &json!({ "error": "server_error" }), now), Change::State(AgentState::Error));
        assert_eq!(apply(&mut s, "StopFailure", &json!({}), now), Change::Updated);
        assert_eq!(s.state, AgentState::Error);
    }

    #[test]
    fn session_end_and_unknown_events() {
        let (mut s, now) = session();
        assert_eq!(apply(&mut s, "SomethingNew", &json!({ "weird": [1, 2] }), now), Change::None);
        assert_eq!(apply(&mut s, "SessionEnd", &json!({}), now), Change::Ended);
    }

    #[test]
    fn hostile_text_is_cleaned_before_display() {
        let (mut s, now) = session();
        let payload = json!({ "tool_name": "Bash", "tool_input": { "command": "echo \u{1b}]0;pwned\u{7}\nrm x" } });
        apply(&mut s, "PreToolUse", &payload, now);
        let activity = s.activity.unwrap();
        assert!(!activity.chars().any(char::is_control), "{activity:?}");
    }
}
