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
            // A new or cleared conversation starts without a plan; one
            // resumed or compacted keeps it.
            if !matches!(payload["source"].as_str(), Some("resume" | "compact")) {
                session.plan.clear();
                session.plan_file = None;
            }
            session.set_state(AgentState::Idle, now)
        }
        "UserPromptSubmit" => {
            session.new_turn();
            if let Some(prompt) = text(payload, &["prompt"]) {
                // The first line of the first prompt names the task until
                // the agent provides a better title.
                session.suggest_title(prompt.lines().find(|line| !line.trim().is_empty()).unwrap_or(""));
                // A Markdown file named in the prompt may be the plan.
                for file in crate::plan::markdown_files_named(&prompt) {
                    session.consider_plan_file(&file, true);
                }
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
                    let action = describe_tool(payload);
                    session.note_action(&action, now);
                    session.set_activity(action);
                    // A question or a plan to approve waits for the user
                    // in every permission mode, and no permission request
                    // or notification says so.
                    let state = if asks_the_user(payload) { AgentState::NeedsYou } else { AgentState::Working };
                    session.set_state(state, now)
                }
            }
        }
        "PostToolUse" | "PostToolUseFailure" | "PostToolBatch" | "PermissionDenied" | "ElicitationResult" => {
            if payload.get("agent_id").is_some_and(|id| !id.is_null()) {
                return Change::None;
            }
            // The plan follows the to-do tools' successful calls, and a
            // plan file the agent reads or edits.
            if event == "PostToolUse" {
                let tool = payload["tool_name"].as_str().unwrap_or("");
                let input = &payload["tool_input"];
                let response = ["tool_response", "tool_output", "tool_result"]
                    .iter()
                    .map(|key| &payload[*key])
                    .find(|value| !value.is_null())
                    .unwrap_or(&Value::Null);
                session.plan.apply(tool, input, response);
                let file = input["file_path"].as_str().or_else(|| input["path"].as_str());
                if let Some(file) = file.filter(|f| f.to_ascii_lowercase().ends_with(".md")) {
                    // Editing a file of checkboxes is ticking them; reading
                    // one is taken as the plan only if there is none yet.
                    let editing = matches!(tool, "Edit" | "MultiEdit" | "Write");
                    session.consider_plan_file(file, editing);
                }
                session.refresh_plan_file();
            }
            // Also covers "the user answered the prompt": work resumed.
            session.set_state(AgentState::Working, now)
        }
        "PermissionRequest" => {
            let action = describe_tool(payload);
            session.set_activity(if asks_the_user(payload) { action } else { format!("Permission: {action}") });
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
            session.note_scheduled_wakeups(payload);
            // The main agent had stopped and was only waiting for these.
            if session.state == AgentState::Waiting {
                match session.waiting_for() {
                    Some(what) => session.set_activity(what),
                    None => {
                        session.activity = None;
                        return session.set_state(AgentState::Done, now);
                    }
                }
            }
            Change::Updated
        }
        "Stop" => {
            session.note_background_tasks(payload);
            session.note_scheduled_wakeups(payload);
            // Stopped is not always finished: the agent may be paused
            // until subagents or other background work wake it again.
            match session.waiting_for() {
                Some(what) => {
                    session.set_activity(what);
                    session.set_state(AgentState::Waiting, now)
                }
                None => {
                    session.activity = None;
                    session.set_state(AgentState::Done, now)
                }
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

/// Whether a tool call puts something to the user: Claude Code's
/// multiple-choice questions, and a plan to approve.
fn asks_the_user(payload: &Value) -> bool {
    matches!(payload["tool_name"].as_str(), Some("AskUserQuestion" | "ExitPlanMode"))
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
        "AskUserQuestion" => {
            let questions = input["questions"].as_array().map_or(&[][..], Vec::as_slice);
            let first = questions.iter().filter_map(|question| question["question"].as_str()).next();
            match (first.map(|question| clean(question, 100)).filter(|question| !question.is_empty()), questions.len()) {
                (Some(question), 0 | 1) => format!("Asks: {question}"),
                (Some(question), count) => format!("Asks: {question} (+{} more)", count - 1),
                (None, _) => "Has a question for you".into(),
            }
        }
        "ExitPlanMode" => "Plan ready for your review".into(),
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
    fn the_plan_follows_the_to_do_tools_of_the_main_agent() {
        let (mut s, now) = session();
        apply(&mut s, "UserPromptSubmit", &json!({ "prompt": "work through the plan" }), now);
        let todos = json!({ "todos": [
            { "content": "Add the tests", "activeForm": "Adding the tests", "status": "in_progress" },
            { "content": "Update the docs", "activeForm": "Updating the docs", "status": "pending" },
        ]});
        // Only a successful call counts.
        apply(&mut s, "PostToolUseFailure", &json!({ "tool_name": "TodoWrite", "tool_input": todos }), now);
        assert!(s.plan.is_empty());
        apply(&mut s, "PostToolUse", &json!({ "tool_name": "TodoWrite", "tool_input": todos, "tool_response": {} }), now);
        assert_eq!(s.plan.progress(), Some((0, 2)));
        // A subagent's own list is not the agent's plan.
        let inner = json!({ "agent_id": "a1", "tool_name": "TodoWrite", "tool_input": { "todos": [] } });
        apply(&mut s, "PostToolUse", &inner, now);
        assert_eq!(s.plan.progress(), Some((0, 2)));
        // Compacting keeps it; clearing the conversation drops it.
        apply(&mut s, "SessionStart", &json!({ "source": "compact" }), now);
        assert_eq!(s.plan.progress(), Some((0, 2)));
        apply(&mut s, "SessionStart", &json!({ "source": "clear" }), now);
        assert!(s.plan.is_empty());
    }

    #[test]
    fn a_markdown_plan_is_followed_as_its_boxes_are_ticked() {
        let dir = std::env::temp_dir().join(format!("ale-term-planfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plan = dir.join("PLAN.md");
        std::fs::write(&plan, "# Plan\n- [x] One\n- [ ] Two\n- [ ] Three\n").unwrap();
        std::fs::write(dir.join("README.md"), "no boxes\n").unwrap();
        let cwd = dir.to_string_lossy().into_owned();

        let (mut s, now) = session();
        apply(&mut s, "UserPromptSubmit", &json!({ "cwd": cwd, "prompt": "implement @PLAN.md step by step" }), now);
        assert_eq!(s.shown_plan().progress(), Some((1, 3)));
        assert_eq!(s.shown_plan().current().map(|i| i.text.as_str()), Some("Two"));

        // The agent ticks a box: the next finished tool call sees it.
        std::fs::write(&plan, "# Plan\n- [x] One\n- [x] Two\n- [ ] Three\n").unwrap();
        let edit = json!({ "cwd": cwd, "tool_name": "Edit", "tool_input": { "file_path": plan.to_string_lossy() }, "tool_response": {} });
        apply(&mut s, "PostToolUse", &edit, now);
        assert_eq!(s.shown_plan().progress(), Some((2, 3)));

        // Editing a Markdown file without boxes does not replace it.
        let readme = json!({ "tool_name": "Edit", "tool_input": { "file_path": dir.join("README.md").to_string_lossy() } });
        apply(&mut s, "PostToolUse", &readme, now);
        assert_eq!(s.shown_plan().progress(), Some((2, 3)));

        // The agent's own to-do list, once it has one, comes first.
        let todos = json!({ "tool_name": "TodoWrite", "tool_input": { "todos": [{ "content": "Own step", "status": "in_progress" }] }, "tool_response": {} });
        apply(&mut s, "PostToolUse", &todos, now);
        assert_eq!(s.shown_plan().progress(), Some((0, 1)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_latest_actions_are_remembered_for_the_summary() {
        let (mut s, now) = session();
        apply(&mut s, "UserPromptSubmit", &json!({ "prompt": "tidy up" }), now);
        let read = json!({ "cwd": "/repo", "tool_name": "Read", "tool_input": { "file_path": "/repo/a.rs" } });
        apply(&mut s, "PreToolUse", &read, now);
        // Reading the same file again in pieces counts once.
        apply(&mut s, "PreToolUse", &read, now);
        for n in 0..8 {
            let run = json!({ "tool_name": "Bash", "tool_input": { "command": format!("step {n}") } });
            apply(&mut s, "PreToolUse", &run, now);
        }
        let actions: Vec<&str> = s.recent.iter().map(|(_, action)| action.as_str()).collect();
        assert_eq!(actions.len(), crate::session::RECENT_ACTIONS);
        assert_eq!(actions.last(), Some(&"Running step 7"));
        // A subagent's tools are its own, not the main agent's.
        let inner = json!({ "agent_id": "a1", "tool_name": "Read", "tool_input": { "file_path": "/x.rs" } });
        apply(&mut s, "PreToolUse", &inner, now);
        assert_eq!(s.recent.back().map(|(_, a)| a.as_str()), Some("Running step 7"));
    }

    #[test]
    fn event_name_can_come_from_the_payload() {
        let (mut s, now) = session();
        let payload = json!({ "hook_event_name": "UserPromptSubmit", "prompt": "hi" });
        assert_eq!(apply(&mut s, "", &payload, now), Change::State(AgentState::Working));
    }

    #[test]
    fn questions_and_plans_need_the_user_in_any_permission_mode() {
        let (mut s, now) = session();
        apply(&mut s, "UserPromptSubmit", &json!({ "prompt": "set it up" }), now);
        let question = |count: usize| {
            let one = json!({ "question": "Which  database?", "header": "DB", "options": [], "multiSelect": false });
            json!({ "tool_name": "AskUserQuestion", "tool_input": { "questions": vec![one; count] } })
        };
        assert_eq!(apply(&mut s, "PreToolUse", &question(1), now), Change::State(AgentState::NeedsYou));
        assert_eq!(s.activity.as_deref(), Some("Asks: Which database?"));
        // Where a permission request follows, it says the same.
        assert_eq!(apply(&mut s, "PermissionRequest", &question(1), now), Change::Updated);
        assert_eq!(s.activity.as_deref(), Some("Asks: Which database?"));
        // Answered.
        assert_eq!(apply(&mut s, "PostToolUse", &question(1), now), Change::State(AgentState::Working));
        apply(&mut s, "PreToolUse", &question(3), now);
        assert_eq!(s.activity.as_deref(), Some("Asks: Which database? (+2 more)"));
        apply(&mut s, "PostToolUse", &question(3), now);

        let plan = json!({ "tool_name": "ExitPlanMode", "tool_input": { "plan": "1. Do it" } });
        assert_eq!(apply(&mut s, "PreToolUse", &plan, now), Change::State(AgentState::NeedsYou));
        assert_eq!(s.activity.as_deref(), Some("Plan ready for your review"));
        // A subagent's tool calls leave the agent's state alone.
        apply(&mut s, "PostToolUse", &plan, now);
        let inner = json!({ "tool_name": "AskUserQuestion", "agent_id": "sub-1", "tool_input": {} });
        apply(&mut s, "PreToolUse", &inner, now);
        assert_eq!(s.state, AgentState::Working);
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

        assert_eq!(apply(&mut s, "Stop", &json!({}), now), Change::State(AgentState::Waiting));
        apply(&mut s, "SubagentStop", &json!({ "agent_id": "a1" }), now);
        assert_eq!(s.state, AgentState::Waiting);
        let later = now + Duration::from_secs(3);
        assert_eq!(apply(&mut s, "SubagentStop", &json!({ "agent_id": "a2" }), later), Change::State(AgentState::Done));
    }

    #[test]
    fn a_stopped_agent_with_work_in_flight_is_waiting_not_done() {
        let (mut s, now) = session();
        apply(&mut s, "UserPromptSubmit", &json!({ "prompt": "ship it" }), now);
        // A workflow is running in the background.
        let workflow = json!({ "id": "w1", "type": "workflow", "status": "running", "description": "review changes" });
        let stop = json!({ "background_tasks": [workflow], "session_crons": [] });
        assert_eq!(apply(&mut s, "Stop", &stop, now), Change::State(AgentState::Waiting));
        assert_eq!(s.activity.as_deref(), Some("Waiting on workflow: review changes"));
        // It wakes, works, and stops with nothing left: now it is done.
        let tool = json!({ "tool_name": "Read", "tool_input": { "file_path": "/a/b.rs" } });
        assert_eq!(apply(&mut s, "PreToolUse", &tool, now), Change::State(AgentState::Working));
        assert_eq!(apply(&mut s, "Stop", &json!({ "background_tasks": [] }), now), Change::State(AgentState::Done));

        // A server left running is not something to wait for.
        let server = json!({ "id": "s1", "type": "shell", "status": "running", "description": "dev server", "command": "npm run dev" });
        apply(&mut s, "UserPromptSubmit", &json!({ "prompt": "start it" }), now);
        assert_eq!(apply(&mut s, "Stop", &json!({ "background_tasks": [server.clone()] }), now), Change::State(AgentState::Done));

        // Background subagents reported only in the list still count, and
        // several kinds are summed up.
        apply(&mut s, "UserPromptSubmit", &json!({ "prompt": "more" }), now);
        let agent = json!({ "id": "a9", "type": "subagent", "status": "running", "description": "explore", "agent_type": "Explore" });
        let monitor = json!({ "id": "m1", "type": "monitor", "status": "running", "description": "watch CI" });
        let stop = json!({ "background_tasks": [agent, monitor, workflow_again(), server] });
        assert_eq!(apply(&mut s, "Stop", &stop, now), Change::State(AgentState::Waiting));
        assert_eq!(s.activity.as_deref(), Some("Waiting on 1 subagent and 2 background tasks"));

        // A scheduled wake-up alone also means "not finished".
        apply(&mut s, "UserPromptSubmit", &json!({ "prompt": "check hourly" }), now);
        let cron = json!({ "id": "c1", "schedule": "0 * * * *", "recurring": true, "prompt": "check" });
        let stop = json!({ "background_tasks": [], "session_crons": [cron] });
        assert_eq!(apply(&mut s, "Stop", &stop, now), Change::State(AgentState::Waiting));
        assert_eq!(s.activity.as_deref(), Some("Paused until its scheduled wake-up"));
    }

    fn workflow_again() -> serde_json::Value {
        json!({ "id": "w2", "type": "workflow", "status": "running", "description": "second pass" })
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
