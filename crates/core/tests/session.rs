//! End-to-end checks against a real shell in a real PTY.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ale_core::vt::grid::Dimensions;
use ale_core::vt::index::{Column, Line, Point};
use ale_core::{GridSize, SpawnOptions, TermEvent, Terminal};

/// Polls `check` until it returns true or five seconds pass.
fn wait_for(mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn spawn(directory: &str) -> (Terminal, mpsc::Receiver<&'static str>) {
    let (tx, rx) = mpsc::channel();
    let callback = Arc::new(move |_id, event| {
        let name = match event {
            TermEvent::Exit => "exit",
            TermEvent::Wakeup => "wakeup",
            _ => "other",
        };
        let _ = tx.send(name);
    });
    let options = SpawnOptions {
        // A plain `sh` keeps the test independent of the user's shell setup.
        shell: Some(("/bin/sh".into(), vec![])),
        working_directory: Some(PathBuf::from(directory)),
        scrollback: 100,
        size: GridSize { columns: 40, lines: 10, cell_width: 8, cell_height: 16 },
    };
    (Terminal::spawn(1, options, callback).expect("spawn shell"), rx)
}

fn screen_text(terminal: &Terminal) -> String {
    let term = terminal.lock();
    let last = Point::new(Line(term.screen_lines() as i32 - 1), Column(term.columns() - 1));
    term.bounds_to_string(Point::new(Line(0), Column(0)), last)
}

#[test]
fn shell_output_reaches_the_grid_and_exit_is_reported() {
    let (terminal, events) = spawn("/");
    terminal.write(&b"echo hello-$((40+2))\n"[..]);
    assert!(wait_for(|| screen_text(&terminal).contains("hello-42")), "{}", screen_text(&terminal));

    terminal.write(&b"exit\n"[..]);
    assert!(wait_for(|| events.try_iter().any(|event| event == "exit")));
}

#[test]
fn reports_the_working_directory_and_follows_cd() {
    let (terminal, _events) = spawn("/usr");
    assert!(wait_for(|| terminal.working_directory() == Some(PathBuf::from("/usr"))));

    terminal.write(&b"cd /usr/bin\n"[..]);
    assert!(wait_for(|| terminal.working_directory() == Some(PathBuf::from("/usr/bin"))));
}

#[test]
fn resize_changes_the_grid_and_tells_the_shell() {
    let (terminal, _events) = spawn("/");
    terminal.resize(GridSize { columns: 60, lines: 20, cell_width: 8, cell_height: 16 });
    assert_eq!(terminal.lock().columns(), 60);

    terminal.write(&b"stty size\n"[..]);
    assert!(wait_for(|| screen_text(&terminal).contains("20 60")), "{}", screen_text(&terminal));
}
