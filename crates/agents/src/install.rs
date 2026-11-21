//! Installing and removing the status hooks in an agent's own settings.
//!
//! This edits a file that belongs to another program, so it is done with
//! care: only on the user's say-so, changing nothing but our own entries,
//! keeping everything else (including key order) as it was, and leaving a
//! backup of the original next to it the first time.
//!
//! Claude Code's hooks go into its settings file, next to the user's own.
//! Kiro reads hooks from every file in `~/.kiro/hooks/`, so for Kiro we
//! write a file of our own there and never touch the user's: removing our
//! hooks means deleting that file.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::channel::ENV_BIN;

/// The Claude Code events we listen to.
pub const CLAUDE_EVENTS: [&str; 12] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Notification",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// The command a hook runs.
///
/// It calls the terminal's own executable, whose location the terminal
/// puts in the environment of every shell it starts. Outside ale-term the
/// variable is unset and the command becomes `true`, which does nothing:
/// the hooks are harmless in any other terminal.
pub fn hook_command(agent: &str) -> String {
    format!("\"${{{ENV_BIN}:-true}}\" hook {agent}")
}

fn is_ours(handler: &Value) -> bool {
    handler["command"].as_str().is_some_and(|command| command.contains(ENV_BIN) && command.contains(" hook "))
}

/// True when every event already has our hook.
pub fn claude_hooks_installed(settings: &Value) -> bool {
    CLAUDE_EVENTS.iter().all(|event| {
        settings["hooks"][event].as_array().is_some_and(|groups| {
            groups.iter().any(|group| group["hooks"].as_array().is_some_and(|hooks| hooks.iter().any(is_ours)))
        })
    })
}

/// Adds our hook to every event that lacks it. Existing settings and hooks
/// are left alone. Fails if the file's structure is not what we expect,
/// rather than guess.
pub fn add_claude_hooks(settings: &mut Value) -> Result<(), String> {
    let root = settings.as_object_mut().ok_or("the settings file is not a JSON object")?;
    let hooks = root.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks.as_object_mut().ok_or("\"hooks\" is not an object")?;
    for event in CLAUDE_EVENTS {
        let groups = hooks.entry(event).or_insert_with(|| Value::Array(Vec::new()));
        let groups = groups.as_array_mut().ok_or_else(|| format!("\"hooks.{event}\" is not a list"))?;
        let present = groups
            .iter()
            .any(|group| group["hooks"].as_array().is_some_and(|hooks| hooks.iter().any(is_ours)));
        if !present {
            // `async` lets the agent carry on without waiting for us.
            groups.push(json!({
                "hooks": [{ "type": "command", "command": hook_command("claude"), "async": true }]
            }));
        }
    }
    Ok(())
}

/// Removes our hooks, and any containers that are empty as a result.
pub fn remove_claude_hooks(settings: &mut Value) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else { return };
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else { continue };
        for group in groups.iter_mut() {
            if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                handlers.retain(|handler| !is_ours(handler));
            }
        }
        groups.retain(|group| group["hooks"].as_array().is_none_or(|handlers| !handlers.is_empty()));
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|groups| !groups.is_empty()));
    if hooks.is_empty()
        && let Some(root) = settings.as_object_mut()
    {
        root.remove("hooks");
    }
}

pub fn claude_settings_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude").join("settings.json"))
}

fn read_settings(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn write_settings(path: &Path, settings: &Value) -> Result<(), String> {
    let describe = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(describe)?;
    }
    // Keep the original the first time we touch the file.
    let backup = path.with_extension("json.before-ale-term");
    if path.exists() && !backup.exists() {
        std::fs::copy(path, &backup).map_err(describe)?;
    }
    let mut text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    text.push('\n');
    // Write beside the file and rename, so it is never left half-written.
    let temporary = path.with_extension("json.ale-term-tmp");
    std::fs::write(&temporary, text).map_err(describe)?;
    std::fs::rename(&temporary, path).map_err(describe)
}

/// Whether Claude Code's settings at `path` already have our hooks.
pub fn claude_installed_at(path: &Path) -> bool {
    read_settings(path).is_ok_and(|settings| claude_hooks_installed(&settings))
}

pub fn install_claude_at(path: &Path) -> Result<(), String> {
    let mut settings = read_settings(path)?;
    add_claude_hooks(&mut settings)?;
    write_settings(path, &settings)
}

pub fn uninstall_claude_at(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let mut settings = read_settings(path)?;
    remove_claude_hooks(&mut settings);
    write_settings(path, &settings)
}

// ---- Kiro -------------------------------------------------------------------

/// The Kiro CLI events we listen to (Kiro 3.x trigger names).
pub const KIRO_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "PreCompact",
    "Stop",
    "SessionEnd",
];

/// How long Kiro may wait for one of our hooks, in seconds. Kiro waits for
/// hooks to finish; ours only hands a line to a local socket, so this is a
/// safety net that keeps any problem from stalling the agent.
const KIRO_TIMEOUT: u64 = 5;

/// The command a Kiro hook runs for `event`.
///
/// The event is named on the command line, so we do not depend on how Kiro
/// spells it in the data it sends. `|| true` makes the hook always succeed:
/// Kiro blocks a tool when its PreToolUse hook fails. The hook prints
/// nothing, which Kiro takes as "no opinion": for a permission request it
/// shows its usual prompt, so the hook can never approve anything.
pub fn kiro_hook_command(event: &str) -> String {
    format!("{} {event} || true", hook_command("kiro"))
}

/// Our hooks file for Kiro, in the format of `~/.kiro/hooks/*.json`.
pub fn kiro_hook_file() -> Value {
    let hooks: Vec<Value> = KIRO_EVENTS
        .iter()
        .map(|event| {
            json!({
                "name": format!("ale-term status ({event})"),
                "description": "Tells ale-term what this Kiro session is doing. Does nothing outside ale-term.",
                "trigger": event,
                "action": { "type": "command", "command": kiro_hook_command(event) },
                "timeout": KIRO_TIMEOUT,
            })
        })
        .collect();
    json!({ "version": "v1", "hooks": hooks })
}

/// Where our Kiro hooks file goes: Kiro CLI loads every file in
/// `~/.kiro/hooks/`, for every agent.
pub fn kiro_hooks_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".kiro").join("hooks").join("ale-term.json"))
}

/// Whether Kiro looks installed: its settings folder exists.
pub fn kiro_present() -> bool {
    std::env::var_os("HOME").is_some_and(|home| PathBuf::from(home).join(".kiro").is_dir())
}

/// Whether our hooks file at `path` is there and hooks every event.
pub fn kiro_installed_at(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    let Ok(file) = serde_json::from_str::<Value>(&text) else { return false };
    let Some(hooks) = file["hooks"].as_array() else { return false };
    KIRO_EVENTS.iter().all(|event| {
        hooks.iter().any(|hook| hook["trigger"] == *event && hook["action"]["command"] == kiro_hook_command(event))
    })
}

/// Writes our hooks file, replacing an older one of ours.
pub fn install_kiro_at(path: &Path) -> Result<(), String> {
    let describe = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(describe)?;
    }
    let mut text = serde_json::to_string_pretty(&kiro_hook_file()).map_err(|e| e.to_string())?;
    text.push('\n');
    // Kiro would read any `.json` file here, so the half-written one must
    // not end in `.json`.
    let temporary = path.with_extension("ale-term-tmp");
    std::fs::write(&temporary, text).map_err(describe)?;
    std::fs::rename(&temporary, path).map_err(describe)
}

/// Deletes our hooks file. Nothing else in Kiro's folders is touched.
pub fn uninstall_kiro_at(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn existing() -> Value {
        json!({
            "model": "opus",
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "say done" }] }],
                "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "./check.sh" }] }]
            },
            "tui": { "theme": "dark" }
        })
    }

    #[test]
    fn adding_keeps_existing_settings_and_is_idempotent() {
        let mut settings = existing();
        assert!(!claude_hooks_installed(&settings));
        add_claude_hooks(&mut settings).unwrap();
        assert!(claude_hooks_installed(&settings));

        // The user's own hooks and other settings are untouched, in order.
        assert_eq!(settings["hooks"]["Stop"][0]["hooks"][0]["command"], "say done");
        assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        let keys: Vec<&String> = settings.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "hooks", "tui"]);
        // Ours is appended after theirs.
        assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(settings["hooks"]["Stop"][1]["hooks"][0]["async"], true);

        let once = settings.clone();
        add_claude_hooks(&mut settings).unwrap();
        assert_eq!(settings, once, "installing twice must change nothing");
    }

    #[test]
    fn removing_restores_the_original_exactly() {
        let mut settings = existing();
        add_claude_hooks(&mut settings).unwrap();
        remove_claude_hooks(&mut settings);
        assert_eq!(settings, existing());

        // From nothing and back to nothing.
        let mut empty = json!({ "model": "opus" });
        add_claude_hooks(&mut empty).unwrap();
        remove_claude_hooks(&mut empty);
        assert_eq!(empty, json!({ "model": "opus" }));
    }

    #[test]
    fn unexpected_shapes_are_refused() {
        assert!(add_claude_hooks(&mut json!([1, 2])).is_err());
        assert!(add_claude_hooks(&mut json!({ "hooks": "nope" })).is_err());
        assert!(add_claude_hooks(&mut json!({ "hooks": { "Stop": {} } })).is_err());
    }

    #[test]
    fn the_command_is_inert_outside_the_terminal() {
        let command = hook_command("claude");
        assert_eq!(command, "\"${ALE_TERM_BIN:-true}\" hook claude");
        // With the variable unset the shell runs `true hook claude`.
        let status = std::process::Command::new("sh").arg("-c").arg(&command).env_remove(ENV_BIN).output().unwrap();
        assert!(status.status.success() && status.stdout.is_empty() && status.stderr.is_empty());
    }

    #[test]
    fn files_are_backed_up_once_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("ale-term-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let original = "{\n  \"model\": \"opus\"\n}\n";
        std::fs::write(&path, original).unwrap();

        assert!(!claude_installed_at(&path));
        install_claude_at(&path).unwrap();
        assert!(claude_installed_at(&path));
        let backup = dir.join("settings.json.before-ale-term");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);

        uninstall_claude_at(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        // The backup is not overwritten by later installs.
        std::fs::write(&path, "{\n  \"model\": \"sonnet\"\n}\n").unwrap();
        install_claude_at(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);

        // A missing file is created; a broken one is left alone.
        let fresh = dir.join("new").join("settings.json");
        install_claude_at(&fresh).unwrap();
        assert!(claude_installed_at(&fresh));
        std::fs::write(&path, "{ broken").unwrap();
        assert!(install_claude_at(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ broken");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_kiro_file_hooks_every_event_and_can_never_block_or_approve() {
        let file = kiro_hook_file();
        assert_eq!(file["version"], "v1");
        let hooks = file["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), KIRO_EVENTS.len());
        for (hook, event) in hooks.iter().zip(KIRO_EVENTS) {
            assert_eq!(hook["trigger"], event);
            assert_eq!(hook["action"]["type"], "command");
            assert_eq!(hook["timeout"], KIRO_TIMEOUT);
        }
        // Outside ale-term the command does nothing, prints nothing and
        // succeeds, and it succeeds even when the hook itself fails.
        for unset in [true, false] {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg(kiro_hook_command("PreToolUse"));
            if unset {
                command.env_remove(ENV_BIN);
            } else {
                command.env(ENV_BIN, "/nonexistent/ale-term");
            }
            let output = command.output().unwrap();
            assert!(output.status.success() && output.stdout.is_empty(), "{output:?}");
        }
    }

    #[test]
    fn kiro_hooks_install_and_remove_only_our_own_file() {
        let dir = std::env::temp_dir().join(format!("ale-term-kiro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hooks = dir.join(".kiro").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let theirs = hooks.join("lint.json");
        std::fs::write(&theirs, "{\"version\":\"v1\",\"hooks\":[]}").unwrap();
        let ours = hooks.join("ale-term.json");

        assert!(!kiro_installed_at(&ours));
        install_kiro_at(&ours).unwrap();
        assert!(kiro_installed_at(&ours));
        install_kiro_at(&ours).unwrap();
        assert!(kiro_installed_at(&ours));
        // Only our file and the user's are there: no temporary left over.
        let mut names: Vec<String> =
            std::fs::read_dir(&hooks).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["ale-term.json", "lint.json"]);

        uninstall_kiro_at(&ours).unwrap();
        assert!(!ours.exists() && theirs.exists());
        uninstall_kiro_at(&ours).unwrap();

        // A file that lost one of our events no longer counts as installed.
        let mut file = kiro_hook_file();
        file["hooks"].as_array_mut().unwrap().pop();
        std::fs::write(&ours, file.to_string()).unwrap();
        assert!(!kiro_installed_at(&ours));
        let _ = std::fs::remove_dir_all(dir);
    }
}
