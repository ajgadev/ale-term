//! Finding out which coding agents are installed, so the app only offers
//! agents that will actually start.
//!
//! The question goes to the user's own shell, the one a new tab runs: an
//! app opened from the Finder does not see the `PATH` that `.zshrc` or
//! `.bash_profile` set up (nvm, `~/.local/bin`, Homebrew), and an agent may
//! be an alias rather than a file. The shell is started as an interactive
//! login shell, asked `command -v` for each agent's start command, and given
//! a few seconds. Only if it cannot answer are the usual install folders
//! searched instead.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::archive::start_command;
use crate::session::AgentKind;

/// An installed agent and where its command was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub kind: AgentKind,
    /// The file the command runs, with the home folder shown as `~`, or
    /// "alias" when it is one.
    pub location: String,
}

/// How long the shell may take to answer.
const TIMEOUT: Duration = Duration::from_secs(6);

/// Marks the lines we asked for, so anything a startup file prints is
/// ignored.
const MARK: &str = "ale-term-agent";

/// Finds the installed agents, in the order of [`AgentKind::ALL`]. `shell`
/// is the configured shell, if any. Takes up to a few seconds: call it
/// away from the main thread.
pub fn detect(shell: Option<&str>) -> Vec<Installed> {
    let shell = shell
        .map(str::to_owned)
        .or_else(|| std::env::var("SHELL").ok())
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/zsh".into());
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match ask_shell(&shell, home.as_deref()) {
        Some(found) => found,
        None => {
            log::info!("the shell did not say which agents are installed; searching the usual folders");
            search_folders(home.as_deref())
        }
    }
}

/// The script that asks the shell about each agent.
fn script(shell: &str) -> String {
    let commands: Vec<&str> = AgentKind::ALL.iter().map(|kind| start_command(*kind)).collect();
    let list = commands.join(" ");
    let fish = Path::new(shell).file_name().is_some_and(|name| name == "fish");
    if fish {
        format!("for c in {list}; set -l p (command -v $c 2>/dev/null); and printf '{MARK}\\t%s\\t%s\\n' $c $p; end")
    } else {
        format!(
            "for c in {list}; do p=$(command -v \"$c\" 2>/dev/null) && printf '{MARK}\\t%s\\t%s\\n' \"$c\" \"$p\"; done"
        )
    }
}

/// Runs the script in the user's shell. `None` when the shell could not be
/// run or did not answer in time.
fn ask_shell(shell: &str, home: Option<&Path>) -> Option<Vec<Installed>> {
    let mut command = Command::new(shell);
    command.args(["-i", "-l", "-c", &script(shell)]);
    run(command, home, TIMEOUT)
}

fn run(mut command: Command, home: Option<&Path>, timeout: Duration) -> Option<Vec<Installed>> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(home) = home {
        command.current_dir(home);
    }
    let mut child = command.spawn().ok()?;
    // Read on a thread: a startup file that prints a lot would otherwise
    // fill the pipe and stall the shell.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let text = reader.join().ok()?;
    Some(parse(&text, home))
}

/// Reads the marked lines: mark, command, where it was found.
fn parse(text: &str, home: Option<&Path>) -> Vec<Installed> {
    let mut found: Vec<Installed> = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        if parts.next() != Some(MARK) {
            continue;
        }
        let (Some(command), Some(location)) = (parts.next(), parts.next()) else { continue };
        let Some(kind) = AgentKind::ALL.into_iter().find(|kind| start_command(*kind) == command) else { continue };
        if location.trim().is_empty() || found.iter().any(|f| f.kind == kind) {
            continue;
        }
        found.push(Installed { kind, location: describe(location.trim(), home) });
    }
    found.sort_by_key(|f| AgentKind::ALL.iter().position(|kind| *kind == f.kind));
    found
}

/// How a location is shown: shortened under the home folder; an alias as
/// just "alias".
fn describe(location: &str, home: Option<&Path>) -> String {
    if location.starts_with("alias ") || location.contains('=') {
        return "alias".into();
    }
    let path = Path::new(location);
    if !path.is_absolute() {
        // A shell function or builtin.
        return location.to_owned();
    }
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => location.to_owned(),
    }
}

/// Where agents usually end up when the shell cannot be asked: the app's
/// own `PATH` and the folders installers use.
fn search_folders(home: Option<&Path>) -> Vec<Installed> {
    let mut folders: Vec<PathBuf> = std::env::var_os("PATH").map(|path| std::env::split_paths(&path).collect()).unwrap_or_default();
    folders.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    if let Some(home) = home {
        for folder in [".local/bin", ".claude/local", ".npm-global/bin", ".bun/bin", ".opencode/bin", ".cargo/bin", ".volta/bin"] {
            folders.push(home.join(folder));
        }
    }
    search_in(&folders, home)
}

fn search_in(folders: &[PathBuf], home: Option<&Path>) -> Vec<Installed> {
    AgentKind::ALL
        .into_iter()
        .filter_map(|kind| {
            let path = folders.iter().map(|folder| folder.join(start_command(kind))).find(|path| is_executable(path))?;
            Some(Installed { kind, location: describe(&path.to_string_lossy(), home) })
        })
        .collect()
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ale-term-available-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_command(dir: &Path, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn only_marked_lines_count_and_locations_are_shortened() {
        let home = Path::new("/Users/me");
        let output = "Welcome to zsh!\n\
            ale-term-agent\tkiro-cli\t/Users/me/.local/bin/kiro-cli\n\
            ale-term-agent\tclaude\talias claude=/Users/me/.claude/local/claude\n\
            ale-term-agent\tnot-an-agent\t/usr/bin/true\n\
            ale-term-agent\tcodex\t\n\
            ale-term-agent\tclaude\t/opt/homebrew/bin/claude\n";
        let found = parse(output, Some(home));
        assert_eq!(
            found,
            [
                Installed { kind: AgentKind::Claude, location: "alias".into() },
                Installed { kind: AgentKind::Kiro, location: "~/.local/bin/kiro-cli".into() },
            ]
        );
    }

    #[test]
    fn the_shell_finds_commands_on_its_path() {
        let dir = scratch("shell");
        fake_command(&dir, "codex");
        fake_command(&dir, "kiro-cli");
        // A plain `sh` with only that folder on its PATH, which is what
        // `ask_shell` does with the user's login shell.
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script("/bin/sh")]).env("PATH", format!("{}:/usr/bin:/bin", dir.display()));
        let found = run(command, None, TIMEOUT).unwrap();
        let kinds: Vec<AgentKind> = found.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [AgentKind::Codex, AgentKind::Kiro]);
        assert_eq!(found[0].location, dir.join("codex").to_string_lossy());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_shell_that_hangs_is_given_up_on() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let started = Instant::now();
        assert_eq!(run(command, None, Duration::from_millis(300)), None);
        assert!(started.elapsed() < Duration::from_secs(3));
        // A shell that cannot be started at all.
        assert_eq!(ask_shell("/nonexistent/shell", None), None);
    }

    #[test]
    fn folders_are_searched_for_executables_only() {
        let dir = scratch("folders");
        fake_command(&dir, "claude");
        std::fs::write(dir.join("gemini"), "not executable").unwrap();
        let found = search_in(std::slice::from_ref(&dir), Some(&dir));
        assert_eq!(found, [Installed { kind: AgentKind::Claude, location: "~/claude".into() }]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fish_gets_its_own_syntax() {
        assert!(script("/opt/homebrew/bin/fish").contains("set -l p (command -v $c"));
        assert!(script("/bin/zsh").contains("do p=$(command -v"));
    }
}
