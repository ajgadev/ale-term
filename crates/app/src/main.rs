//! ale-term: a fast, private terminal built for working with coding agents.
//!
//! This crate is the application shell. It owns the window and the event
//! loop and ties the other crates together:
//!
//! * `ale-core`   runs shells and emulates the terminal,
//! * `ale-render` draws rectangles and glyphs on the GPU,
//! * `ale-config` loads settings and themes.

mod actions;
mod app;
mod board;
mod boxdraw;
mod chrome;
mod dock;
mod draw;
mod agent_map;
mod graph;
mod icons;
mod input;
mod layout;
mod links;
mod mention;
mod menu;
mod overlay;
mod settings;
mod preview;
mod sidebar;
mod start;
mod summary;
mod usage;
mod workspace;

use std::path::PathBuf;

use winit::event_loop::EventLoop;

/// Command-line options. The `snapshot`, `type` and `demo` flags exist for
/// automated visual checks and are not meant for everyday use.
#[derive(Default)]
pub struct Options {
    /// Folder the first tab starts in.
    pub working_directory: Option<PathBuf>,
    /// Save a picture of the window to this BMP file, then quit.
    pub snapshot: Option<PathBuf>,
    /// Also save what is actually on the window's surface at that moment.
    pub snapshot_shown: Option<PathBuf>,
    /// How long to wait before taking the snapshot, in milliseconds.
    pub snapshot_delay_ms: u64,
    /// Text typed into the first pane at startup (`\n` presses Enter).
    pub type_text: Option<String>,
    /// Open an extra tab and a split to exercise the layout.
    pub demo: bool,
    /// Steps run just before the snapshot: a palette command name, or
    /// `type:<text>` to type into the open palette or search bar.
    pub script: Vec<String>,
}

const USAGE: &str = "\
ale-term [options]

  --cwd <dir>     Start the first tab in <dir>
  -v, --version   Print the version
  -h, --help      Print this help

ale-term hooks [status | install <agent> | remove <agent>]

  Show, add or remove the hooks through which an agent reports its status
  to ale-term. <agent> is claude or kiro.";

fn parse_args() -> Result<Options, String> {
    let mut options = Options { snapshot_delay_ms: 1500, ..Options::default() };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--cwd" => options.working_directory = Some(value("--cwd")?.into()),
            "--snapshot" => options.snapshot = Some(value("--snapshot")?.into()),
            "--snapshot-shown" => options.snapshot_shown = Some(value("--snapshot-shown")?.into()),
            "--snapshot-delay" => {
                options.snapshot_delay_ms =
                    value("--snapshot-delay")?.parse().map_err(|_| "--snapshot-delay needs a number")?;
            }
            "--type" => options.type_text = Some(value("--type")?),
            "--demo" => options.demo = true,
            "--do" => options.script.push(value("--do")?),
            "-v" | "--version" => {
                println!("ale-term {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            // macOS passes a process serial number when launching from Finder.
            other if other.starts_with("-psn_") => {}
            other => return Err(format!("unknown option {other}\n\n{USAGE}")),
        }
    }
    Ok(options)
}

fn main() {
    // `ale-term hook <agent> [event]`: called by an agent's hook to report
    // its status to the running terminal. Handled before anything else so
    // it stays fast and silent.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("hook") {
        let agent = args.next().unwrap_or_default();
        let event = args.next().unwrap_or_default();
        let state_dir = workspace::state_dir();
        ale_agents::channel::run_hook_client(
            &agent,
            &event,
            ale_core::controlling_tty(),
            state_dir.as_deref(),
            std::io::stdin().lock(),
        );
        return;
    }

    // `ale-term hooks ...`: set up status reporting from the command line.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("hooks") {
        let action = args.next().unwrap_or_else(|| "status".into());
        let agent = args.next();
        std::process::exit(match hooks_command(&action, agent.as_deref()) {
            Ok(message) => {
                println!("{message}");
                0
            }
            Err(message) => {
                eprintln!("ale-term: {message}");
                1
            }
        });
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    let event_loop = match EventLoop::<app::UserEvent>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(e) => {
            eprintln!("ale-term: cannot start the window system: {e}");
            std::process::exit(1);
        }
    };
    let mut app = app::App::new(options, event_loop.create_proxy());
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("ale-term: {e}");
        std::process::exit(1);
    }
}

/// `ale-term hooks <action> [agent]`. Returns what to tell the user.
fn hooks_command(action: &str, agent: Option<&str>) -> Result<String, String> {
    use ale_agents::install;
    let claude = install::claude_settings_path().ok_or("cannot locate the home folder")?;
    let kiro = install::kiro_hooks_path().ok_or("cannot locate the home folder")?;
    let state = |installed: bool| if installed { "installed" } else { "not installed" };
    match (action, agent) {
        ("status", _) => Ok(format!(
            "Claude Code: {} ({})\nKiro: {} ({})",
            state(install::claude_installed_at(&claude)),
            claude.display(),
            state(install::kiro_installed_at(&kiro)),
            kiro.display(),
        )),
        ("install", Some("claude")) => {
            install::install_claude_at(&claude)?;
            Ok(format!("Claude Code status hooks installed in {}. Restart running sessions to use them.", claude.display()))
        }
        ("install", Some("kiro")) => {
            install::install_kiro_at(&kiro)?;
            Ok(format!("Kiro status hooks written to {}. New Kiro sessions use them.", kiro.display()))
        }
        ("remove", Some("claude")) => {
            install::uninstall_claude_at(&claude)?;
            Ok(format!("Removed ale-term's hooks from {}.", claude.display()))
        }
        ("remove", Some("kiro")) => {
            install::uninstall_kiro_at(&kiro)?;
            Ok(format!("Removed {}.", kiro.display()))
        }
        ("install" | "remove", _) => Err(format!("say which agent: ale-term hooks {action} claude|kiro")),
        _ => Err(format!("unknown hooks command {action:?}\n\n{USAGE}")),
    }
}
