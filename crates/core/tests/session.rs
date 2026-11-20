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
        integration: None,
        env: Vec::new(),
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

/// Starts a real zsh with an empty home folder (so no user configuration
/// interferes) and the integration script loaded.
fn spawn_integrated_zsh() -> (Terminal, PathBuf) {
    let home = std::env::temp_dir().join(format!("ale-term-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&home).unwrap();
    let zdotdir = ale_core::integration::install(&home).unwrap();
    let options = SpawnOptions {
        shell: Some(("/bin/zsh".into(), vec!["-i".into()])),
        working_directory: Some(PathBuf::from("/")),
        scrollback: 100,
        size: GridSize { columns: 60, lines: 10, cell_width: 8, cell_height: 16 },
        integration: Some(zdotdir),
        env: vec![("HOME".into(), home.to_string_lossy().into_owned())],
    };
    let terminal = Terminal::spawn(2, options, Arc::new(|_, _| {})).expect("spawn zsh");
    (terminal, home)
}

#[test]
fn zsh_integration_reports_folder_commands_and_prompts() {
    let (terminal, home) = spawn_integrated_zsh();
    assert!(wait_for(|| terminal.shell().integrated), "integration never loaded:\n{}", screen_text(&terminal));
    assert!(wait_for(|| terminal.shell().cwd == Some(PathBuf::from("/"))));

    // Folder changes are reported by the shell itself, spaces included.
    let spaced = home.join("my dir");
    std::fs::create_dir_all(&spaced).unwrap();
    terminal.write(format!("cd '{}'\n", spaced.display()).into_bytes());
    let expected = spaced.canonicalize().unwrap();
    assert!(
        wait_for(|| terminal.shell().cwd.and_then(|p| p.canonicalize().ok()) == Some(expected.clone())),
        "{:?}",
        terminal.shell()
    );

    // A running command is reported with its command line...
    terminal.write(&b"sleep 30; echo \"a;b\"\n"[..]);
    assert!(
        wait_for(|| terminal.shell().command.as_deref() == Some("sleep 30; echo \"a;b\"")),
        "{:?}",
        terminal.shell()
    );
    // ...and its exit status once it ends (Ctrl+C gives 130).
    terminal.write(&b"\x03"[..]);
    assert!(wait_for(|| terminal.shell().command.is_none() && terminal.shell().last_exit == Some(130)), "{:?}", terminal.shell());

    // The prompt's cells carry the prompt tag.
    assert!(wait_for(|| {
        let term = terminal.lock();
        let cursor_line = term.grid().cursor.point.line;
        term.grid()[cursor_line][Column(0)]
            .hyperlink()
            .is_some_and(|link| link.uri().starts_with(ale_core::PROMPT_TAG))
    }));
    let _ = std::fs::remove_dir_all(home);
}

/// A 2x2 red PNG, base64-encoded.
const RED_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEUlEQVR4nGP4z8DwH4QZYAwAR8oH+WdZbrcAAAAASUVORK5CYII=";

/// The text of one screen row, without trailing spaces.
fn row_text(terminal: &Terminal, line: i32) -> String {
    let term = terminal.lock();
    let row: String = (0..term.columns()).map(|column| term.grid()[Line(line)][Column(column)].c).collect();
    row.trim_end().to_owned()
}

/// True when some screen row consists of exactly `text`.
fn has_row(terminal: &Terminal, text: &str) -> bool {
    (0..10).any(|line| row_text(terminal, line) == text)
}

fn image_cell_at(terminal: &Terminal, line: i32, column: usize) -> Option<ale_core::image::ImageCell> {
    let term = terminal.lock();
    let link = term.grid()[Line(line)][Column(column)].hyperlink()?;
    ale_core::image::ImageCell::decode(link.uri())
}

#[test]
fn iterm_image_is_decoded_and_occupies_tagged_cells() {
    let (terminal, _events) = spawn("/");
    // 3 cells wide; a square image is then 24px = 1.5 cells tall -> 2 rows.
    terminal.write(format!("printf '\\033]1337;File=inline=1;width=3:{RED_PNG}\\007'; echo after\n").into_bytes());
    assert!(wait_for(|| has_row(&terminal, "after")), "{}", screen_text(&terminal));

    let (images, removed) = terminal.take_images();
    assert_eq!(images.len(), 1);
    assert_eq!((images[0].width, images[0].height), (2, 2));
    assert_eq!(&images[0].rgba[..4], [255, 0, 0, 255]);
    assert!(removed.is_empty());

    // Find the image's first row and check the tagged area.
    let first = (0..10).find(|line| image_cell_at(&terminal, *line, 0).is_some()).expect("no image cells");
    let top = image_cell_at(&terminal, first, 0).unwrap();
    assert_eq!((top.id, top.row), (images[0].id, 0));
    assert_eq!((top.width_cells, top.height_cells), (3.0, 1.5));
    assert_eq!(image_cell_at(&terminal, first, 2).map(|c| c.row), Some(0));
    assert_eq!(image_cell_at(&terminal, first, 3), None);
    assert_eq!(image_cell_at(&terminal, first + 1, 0).map(|c| c.row), Some(1));
    assert_eq!(image_cell_at(&terminal, first + 2, 0), None);
    // Text continues on the line below the image.
    assert_eq!(row_text(&terminal, first + 2), "after");
}

#[test]
fn kitty_image_query_transmit_place_and_delete() {
    let (terminal, _events) = spawn("/");
    // Raw mode off-echo so replies from the terminal are not echoed back
    // as garbage; `cat -v` shows them in readable form.
    let apc = |body: &str| format!("printf '\\033_G{body}\\033\\\\'");
    let query = apc(&format!("a=q,i=5,f=100;{RED_PNG}"));
    // Sent in two chunks.
    let first_chunk = apc(&format!("a=t,i=9,f=100,m=1;{}", &RED_PNG[..40]));
    let second_chunk = apc(&format!("m=0;{}", &RED_PNG[40..]));
    let place = apc("a=p,i=9,c=4,r=2");
    let script = format!(
        "stty -echo; {query}; {first_chunk}; {second_chunk}; {place}; sleep 0.3; echo; echo done; cat -v\n"
    );
    terminal.write(script.into_bytes());
    assert!(wait_for(|| has_row(&terminal, "done")), "{}", screen_text(&terminal));

    let (images, _) = terminal.take_images();
    assert_eq!(images.len(), 1, "the query must not store an image");
    let first = (0..10).find(|line| image_cell_at(&terminal, *line, 0).is_some()).expect("no image cells");
    let cell = image_cell_at(&terminal, first, 3).unwrap();
    assert_eq!((cell.id, cell.width_cells, cell.height_cells), (images[0].id, 4.0, 2.0));
    assert!(image_cell_at(&terminal, first + 1, 0).is_some());

    // The terminal answered the query, the transmission and the placement.
    // (The replies sit in the line buffer until Enter hands them to `cat`.)
    terminal.write(&b"\n"[..]);
    assert!(
        wait_for(|| {
            let text = screen_text(&terminal).replace(['\n', ' '], "");
            text.contains("Gi=5;OK") && text.matches("Gi=9;OK").count() == 2
        }),
        "{}",
        screen_text(&terminal)
    );

    // Deleting reports the image as removed.
    terminal.write(&b"\x04"[..]);
    terminal.write(format!("{}\n", apc("a=d,d=I,i=9")).into_bytes());
    assert!(wait_for(|| terminal.take_images().1 == [images[0].id]));
}
