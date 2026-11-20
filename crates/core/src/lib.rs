//! Terminal sessions: a shell running in a pseudo-terminal (PTY) plus the
//! emulator state that interprets its output.
//!
//! The heavy lifting (escape-sequence parsing, the cell grid, scrollback) is
//! done by the `alacritty_terminal` crate. This crate wraps it in a small API
//! so the rest of the app never deals with threads or file descriptors, and
//! knows nothing about windows or rendering.

mod cwd;

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::ops::DerefMut;
use std::sync::{Arc, Mutex, OnceLock};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::Rgb;

/// Re-exported so the app uses exactly the same version of the emulator
/// types (cells, colors, modes) as this crate.
pub use alacritty_terminal as vt;

/// The emulator state for one session. Lock it through [`Terminal::lock`].
pub type TermState = Term<Listener>;

/// Things a session tells the app about. Delivered on a background thread.
pub enum TermEvent {
    /// New output arrived; the pane should be redrawn.
    Wakeup,
    Title(String),
    ResetTitle,
    Bell,
    /// The shell exited or the PTY closed.
    Exit,
    /// The program asked to put text on the clipboard (OSC 52).
    ClipboardStore(String),
    /// The program asked for the clipboard. Pass the clipboard text to the
    /// formatter and write the result back with [`Terminal::write`].
    ClipboardLoad(Arc<dyn Fn(&str) -> String + Send + Sync>),
    /// The program asked for the RGB value of a color (by index). Format the
    /// answer with the closure and write it back.
    ColorRequest(usize, Arc<dyn Fn(Rgb) -> String + Send + Sync>),
}

/// Called for every [`TermEvent`], with the id of the session it came from.
pub type EventCallback = Arc<dyn Fn(u64, TermEvent) + Send + Sync>;

/// How to start a session.
#[derive(Default)]
pub struct SpawnOptions {
    /// Program to run; `None` starts the user's login shell.
    pub shell: Option<(String, Vec<String>)>,
    pub working_directory: Option<PathBuf>,
    pub scrollback: usize,
    pub size: GridSize,
}

/// Size of a pane in cells, plus the pixel size of one cell (programs can ask
/// for it, e.g. to scale images).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub columns: u16,
    pub lines: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Default for GridSize {
    fn default() -> Self {
        Self { columns: 80, lines: 24, cell_width: 8, cell_height: 16 }
    }
}

impl GridSize {
    fn window_size(self) -> WindowSize {
        WindowSize {
            num_lines: self.lines,
            num_cols: self.columns,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines as usize
    }
    fn screen_lines(&self) -> usize {
        self.lines as usize
    }
    fn columns(&self) -> usize {
        self.columns as usize
    }
}

/// Receives events from the emulator. Some are answered right here (they
/// only need bytes written back to the PTY); the rest go to the app.
#[derive(Clone)]
pub struct Listener {
    id: u64,
    callback: EventCallback,
    /// Set once the I/O thread exists; needed to write replies to the PTY.
    sender: Arc<OnceLock<EventLoopSender>>,
    size: Arc<Mutex<GridSize>>,
}

impl Listener {
    fn write(&self, bytes: Vec<u8>) {
        if let Some(sender) = self.sender.get() {
            let _ = sender.send(Msg::Input(Cow::Owned(bytes)));
        }
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let forward = match event {
            Event::Wakeup => TermEvent::Wakeup,
            Event::Title(title) => TermEvent::Title(title),
            Event::ResetTitle => TermEvent::ResetTitle,
            Event::Bell => TermEvent::Bell,
            Event::Exit | Event::ChildExit(_) => TermEvent::Exit,
            Event::ClipboardStore(_, text) => TermEvent::ClipboardStore(text),
            Event::ClipboardLoad(_, format) => TermEvent::ClipboardLoad(format),
            Event::ColorRequest(index, format) => TermEvent::ColorRequest(index, format),
            Event::PtyWrite(text) => return self.write(text.into_bytes()),
            Event::TextAreaSizeRequest(format) => {
                let size = self.size.lock().unwrap().window_size();
                return self.write(format(size).into_bytes());
            }
            Event::MouseCursorDirty | Event::CursorBlinkingChange => return,
        };
        (self.callback)(self.id, forward);
    }
}

/// One running session.
///
/// Dropping it shuts down the I/O thread, which hangs up the PTY and so ends
/// the shell.
pub struct Terminal {
    id: u64,
    term: Arc<FairMutex<TermState>>,
    sender: EventLoopSender,
    /// Our own handle to the PTY, used to ask which process is in front.
    pty: File,
    shell_pid: u32,
    /// Where the session started; the answer of last resort for
    /// [`Terminal::working_directory`].
    start_directory: Option<PathBuf>,
    size: Arc<Mutex<GridSize>>,
}

impl Terminal {
    /// Starts a shell. `id` is echoed back in every event so the app can tell
    /// sessions apart.
    pub fn spawn(id: u64, options: SpawnOptions, callback: EventCallback) -> io::Result<Terminal> {
        let size = Arc::new(Mutex::new(options.size));
        let sender_slot = Arc::new(OnceLock::new());
        let listener = Listener { id, callback, sender: sender_slot.clone(), size: size.clone() };

        let config = TermConfig { scrolling_history: options.scrollback, ..TermConfig::default() };
        let term = Term::new(config, &options.size, listener.clone());
        let term = Arc::new(FairMutex::new(term));

        let mut env = HashMap::new();
        env.insert("TERM".to_owned(), "xterm-256color".to_owned());
        env.insert("COLORTERM".to_owned(), "truecolor".to_owned());
        env.insert("TERM_PROGRAM".to_owned(), "ale-term".to_owned());
        env.insert("TERM_PROGRAM_VERSION".to_owned(), env!("CARGO_PKG_VERSION").to_owned());

        let start_directory = options.working_directory.clone();
        let pty_options = tty::Options {
            shell: options.shell.map(|(program, args)| tty::Shell::new(program, args)),
            working_directory: options.working_directory,
            drain_on_exit: true,
            env,
        };
        let pty = tty::new(&pty_options, options.size.window_size(), id)?;
        let pty_file = pty.file().try_clone()?;
        let shell_pid = pty.child().id();

        // The I/O thread reads the PTY, feeds the parser and writes our input.
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = event_loop.channel();
        let _ = sender_slot.set(sender.clone());
        event_loop.spawn();

        Ok(Terminal { id, term, sender, pty: pty_file, shell_pid, start_directory, size })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// Locks the emulator state for reading the grid or changing the
    /// selection. Hold it briefly: the I/O thread needs it to process output.
    pub fn lock(&self) -> impl DerefMut<Target = TermState> + '_ {
        self.term.lock()
    }

    /// Sends bytes to the shell as if they were typed.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            let _ = self.sender.send(Msg::Input(bytes));
        }
    }

    pub fn size(&self) -> GridSize {
        *self.size.lock().unwrap()
    }

    /// Resizes both the emulator grid and the PTY (which notifies the shell).
    pub fn resize(&self, size: GridSize) {
        {
            let mut current = self.size.lock().unwrap();
            if *current == size {
                return;
            }
            *current = size;
        }
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.window_size()));
    }

    /// Working directory of the program currently in the foreground of this
    /// session, falling back to the shell's own, then to where the session
    /// started (the OS refuses to answer for the brief moment a login shell
    /// is still starting up).
    pub fn working_directory(&self) -> Option<PathBuf> {
        cwd::foreground_pid(&self.pty)
            .and_then(cwd::of_pid)
            .or_else(|| cwd::of_pid(self.shell_pid as i32))
            .or_else(|| self.start_directory.clone())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}
