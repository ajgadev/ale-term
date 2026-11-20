//! ale-term: a fast, private terminal built for working with coding agents.
//!
//! This crate is the application shell. It owns the window and the event
//! loop and ties the other crates together:
//!
//! * `ale-core`   runs shells and emulates the terminal,
//! * `ale-render` draws rectangles and glyphs on the GPU,
//! * `ale-config` loads settings and themes.

mod app;
mod boxdraw;
mod chrome;
mod draw;
mod input;
mod layout;

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
    /// How long to wait before taking the snapshot, in milliseconds.
    pub snapshot_delay_ms: u64,
    /// Text typed into the first pane at startup (`\n` presses Enter).
    pub type_text: Option<String>,
    /// Open an extra tab and a split to exercise the layout.
    pub demo: bool,
}

const USAGE: &str = "\
ale-term [options]

  --cwd <dir>     Start the first tab in <dir>
  -v, --version   Print the version
  -h, --help      Print this help";

fn parse_args() -> Result<Options, String> {
    let mut options = Options { snapshot_delay_ms: 1500, ..Options::default() };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--cwd" => options.working_directory = Some(value("--cwd")?.into()),
            "--snapshot" => options.snapshot = Some(value("--snapshot")?.into()),
            "--snapshot-delay" => {
                options.snapshot_delay_ms =
                    value("--snapshot-delay")?.parse().map_err(|_| "--snapshot-delay needs a number")?;
            }
            "--type" => options.type_text = Some(value("--type")?),
            "--demo" => options.demo = true,
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
