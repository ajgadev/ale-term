//! The I/O thread of a session: moves bytes between the shell and the
//! terminal emulator.
//!
//! One thread per session waits (using `poll`) for any of: output from the
//! shell, room to write pending input, or a command from the app. Output is
//! run through the [`Scanner`]; ordinary bytes go to the emulator's parser,
//! and the sequences we handle ourselves are acted on in between, in order.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use alacritty_terminal::event::OnResize;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::tty::{ChildEvent, EventedPty, Pty};
use alacritty_terminal::vte::ansi::Processor;

use alacritty_terminal::grid::Dimensions;

use crate::image::{self, ImageCell, KittyCommand};
use crate::scanner::{Chunk, Scanner};
use crate::shell::{self, Mark};
use crate::{GridSize, Listener, Shared, TermEvent, TermState};

pub enum Command {
    /// Bytes to send to the shell.
    Input(Cow<'static, [u8]>),
    Resize(GridSize),
    Shutdown,
}

/// The app's end of the connection to an I/O thread.
#[derive(Clone)]
pub struct IoHandle {
    commands: Sender<Command>,
    /// Writing a byte here interrupts the thread's `poll`.
    wake: Arc<File>,
}

impl IoHandle {
    pub fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            // A full pipe already guarantees a wake-up, so errors are fine.
            let _ = (&*self.wake).write(&[1]);
        }
    }
}

/// How much output to process before checking for commands again.
const READ_BUDGET: usize = 1024 * 1024;

/// The hyperlink id used to tag prompt cells; see `handle_mark`.
pub const PROMPT_TAG: &str = "ale-prompt";

pub struct IoThread {
    pty: Pty,
    /// A second handle to the PTY for reading and writing.
    file: File,
    term: Arc<FairMutex<TermState>>,
    listener: Listener,
    shared: Arc<Shared>,
    commands: Receiver<Command>,
    wake: File,
    parser: Processor,
    scanner: Scanner,
    /// Input waiting for the PTY to accept it, and how much of the first
    /// item is already written.
    outbox: VecDeque<Cow<'static, [u8]>>,
    written: usize,
}

/// Creates the I/O thread state and the handle to talk to it.
pub fn create(
    pty: Pty,
    term: Arc<FairMutex<TermState>>,
    listener: Listener,
    shared: Arc<Shared>,
) -> std::io::Result<(IoThread, IoHandle)> {
    let file = pty.file().try_clone()?;
    let (wake_read, wake_write) = pipe()?;
    let (sender, receiver) = mpsc::channel();
    let thread = IoThread {
        pty,
        file,
        term,
        listener,
        shared,
        commands: receiver,
        wake: wake_read,
        parser: Processor::new(),
        scanner: Scanner::new(),
        outbox: VecDeque::new(),
        written: 0,
    };
    Ok((thread, IoHandle { commands: sender, wake: Arc::new(wake_write) }))
}

/// A non-blocking pipe: (read end, write end).
fn pipe() -> std::io::Result<(File, File)> {
    let mut fds = [0; 2];
    // SAFETY: `pipe` fills the two-element array we pass; on success both
    // descriptors are new and owned by nobody else.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        for fd in fds {
            libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        Ok((File::from(OwnedFd::from_raw_fd(fds[0])), File::from(OwnedFd::from_raw_fd(fds[1]))))
    }
}

impl IoThread {
    pub fn run(mut self) {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            // Wake at the latest when a synchronized update times out, and
            // otherwise twice a second to notice a shell that exited while
            // something else keeps the PTY open.
            let sync_deadline = self.parser.sync_timeout().sync_timeout();
            let timeout_ms = match sync_deadline {
                Some(deadline) => deadline.saturating_duration_since(Instant::now()).as_millis() as i32,
                None => 500,
            };
            let mut fds = [
                libc::pollfd {
                    fd: self.file.as_raw_fd(),
                    events: libc::POLLIN | if self.outbox.is_empty() { 0 } else { libc::POLLOUT },
                    revents: 0,
                },
                libc::pollfd { fd: self.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            // SAFETY: `fds` is a valid array of two pollfd structs that
            // lives across the call.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms.clamp(0, 500)) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                    continue;
                }
                break;
            }

            if sync_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                // The program started a synchronized update and never ended
                // it; show what we have.
                self.parser.stop_sync(&mut *self.term.lock());
                self.listener.emit(TermEvent::Wakeup);
            }

            if fds[1].revents != 0 {
                let mut drain = [0u8; 64];
                while matches!((&self.wake).read(&mut drain), Ok(n) if n > 0) {}
            }
            if !self.handle_commands() {
                return;
            }

            let pty_events = fds[0].revents;
            let mut alive = true;
            if pty_events & libc::POLLIN != 0 {
                alive = self.read(&mut buffer);
            } else if pty_events & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                alive = false;
            }
            if alive && pty_events & libc::POLLOUT != 0 {
                alive = self.write();
            }

            let exited = matches!(self.pty.next_child_event(), Some(ChildEvent::Exited(_)));
            if exited || !alive {
                // Show whatever the shell printed on its way out.
                self.read(&mut buffer);
                break;
            }
        }
        // Marks the terminal as finished and tells the app.
        self.term.lock().exit();
    }

    /// Applies queued commands. Returns false on shutdown.
    fn handle_commands(&mut self) -> bool {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::Input(bytes) => self.outbox.push_back(bytes),
                Command::Resize(size) => self.pty.on_resize(size.window_size()),
                Command::Shutdown => return false,
            }
        }
        if !self.outbox.is_empty() {
            return self.write();
        }
        true
    }

    /// Reads and processes available output. Returns false when the PTY is
    /// closed.
    fn read(&mut self, buffer: &mut [u8]) -> bool {
        let mut total = 0;
        while total < READ_BUDGET {
            match (&self.file).read(buffer) {
                Ok(0) => return false,
                Ok(count) => {
                    self.process(&buffer[..count]);
                    total += count;
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                // EIO is how a closed PTY reports itself.
                Err(_) => return false,
            }
        }
        true
    }

    /// Writes as much pending input as the PTY accepts. Returns false when
    /// the PTY is closed.
    fn write(&mut self) -> bool {
        while let Some(front) = self.outbox.front() {
            match (&self.file).write(&front[self.written..]) {
                Ok(0) => break,
                Ok(count) => {
                    self.written += count;
                    if self.written == front.len() {
                        self.outbox.pop_front();
                        self.written = 0;
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(_) => return false,
            }
        }
        true
    }

    /// Feeds output to the emulator, acting on our own sequences in order.
    fn process(&mut self, bytes: &[u8]) {
        let Self { scanner, parser, term, listener, shared, .. } = self;
        let synced_before = parser.sync_bytes_count();
        scanner.feed(bytes, |chunk| match chunk {
            Chunk::Output(output) => parser.advance(&mut *term.lock(), output),
            Chunk::Osc(payload) => handle_osc(payload, parser, term, listener, shared),
            Chunk::Apc(payload) => handle_kitty(payload, parser, term, listener, shared),
        });
        // Bytes inside a synchronized update are held back by the parser;
        // only ask for a redraw if something actually reached the grid.
        let held_back = parser.sync_bytes_count().saturating_sub(synced_before);
        if held_back < bytes.len() {
            listener.emit(TermEvent::Wakeup);
        }
    }
}

fn handle_osc(
    payload: &[u8],
    parser: &mut Processor,
    term: &FairMutex<TermState>,
    listener: &Listener,
    shared: &Shared,
) {
    if let Some(cwd) = shell::parse_cwd(payload) {
        let mut state = shared.shell.lock().unwrap();
        state.integrated = true;
        if state.cwd.as_ref() != Some(&cwd) {
            state.cwd = Some(cwd);
            drop(state);
            listener.emit(TermEvent::ShellChanged);
        }
        return;
    }
    if payload.starts_with(b"1337;") {
        return handle_iterm(payload, parser, term, shared);
    }
    let Some(mark) = shell::parse_mark(payload) else { return };
    let mut state = shared.shell.lock().unwrap();
    state.integrated = true;
    match mark {
        // Prompt cells are tagged with a private hyperlink id. The grid then
        // carries the mark through scrolling and resizing for us, and
        // "jump to previous prompt" only has to look for tagged cells.
        Mark::PromptStart => {
            let open = format!("\x1b]8;id={PROMPT_TAG};{PROMPT_TAG}:\x1b\\");
            parser.advance(&mut *term.lock(), open.as_bytes());
            return;
        }
        Mark::PromptEnd => {
            parser.advance(&mut *term.lock(), b"\x1b]8;;\x1b\\");
            return;
        }
        Mark::CommandStart(command) => {
            state.command = Some(command.unwrap_or_default());
            state.command_started = Some(Instant::now());
        }
        Mark::CommandEnd(status) => {
            // A prompt with no command run also reports "end"; keep the
            // previous exit status in that case.
            if state.command.take().is_some() {
                state.last_exit = status;
            }
            state.command_started = None;
        }
    }
    drop(state);
    listener.emit(TermEvent::ShellChanged);
}

/// Shows an image at the cursor.
///
/// The emulator library knows nothing about images, so the image's area is
/// filled with blank cells carrying a private hyperlink tag that names the
/// image and the row within it. From then on the grid does the bookkeeping:
/// the image scrolls, goes into history, and is erased or overwritten
/// exactly like text, and the renderer simply draws image rows wherever it
/// finds tagged cells.
///
/// `display` is the wanted size in pixels; it is shrunk to fit the space
/// right of the cursor. `end_on_new_line` leaves the cursor at the start of
/// the line below the image (iTerm2 behaviour) instead of after its last
/// cell (kitty); `keep_cursor` puts the cursor back where it was.
fn place_image(
    id: u32,
    display: (f32, f32),
    end_on_new_line: bool,
    keep_cursor: bool,
    parser: &mut Processor,
    term: &FairMutex<TermState>,
    shared: &Shared,
) {
    let size = *shared.size.lock().unwrap();
    let (cell_width, cell_height) = (size.cell_width.max(1) as f32, size.cell_height.max(1) as f32);
    let mut term = term.lock();
    let start_column = term.grid().cursor.point.column.0;
    let available = term.columns().saturating_sub(start_column).max(1);

    let (mut width, mut height) = (display.0.max(1.0), display.1.max(1.0));
    let max_width = available as f32 * cell_width;
    if width > max_width {
        height *= max_width / width;
        width = max_width;
    }
    let width_cells = width / cell_width;
    let height_cells = height / cell_height;
    let columns = (width_cells.ceil() as usize).clamp(1, available);
    let rows = (height_cells.ceil() as usize).clamp(1, 2000);

    let mut bytes = Vec::new();
    if keep_cursor {
        bytes.extend_from_slice(b"\x1b7");
    }
    let blanks = " ".repeat(columns);
    for row in 0..rows {
        let tag = ImageCell { id, row: row as u32, width_cells, height_cells }.encode();
        bytes.extend_from_slice(format!("\x1b]8;id={tag};{tag}\x1b\\{blanks}\x1b]8;;\x1b\\").as_bytes());
        if row + 1 < rows {
            // Next line, back under the image's left edge.
            bytes.extend_from_slice(format!("\r\n\x1b[{}G", start_column + 1).as_bytes());
        }
    }
    if keep_cursor {
        bytes.extend_from_slice(b"\x1b8");
    } else if end_on_new_line {
        bytes.extend_from_slice(b"\r\n");
    }
    parser.advance(&mut *term, &bytes);
}

/// `OSC 1337 ; File=...`: the iTerm2 inline image protocol.
fn handle_iterm(payload: &[u8], parser: &mut Processor, term: &FairMutex<TermState>, shared: &Shared) {
    let Some(file) = image::parse_iterm(payload) else { return };
    let (width, height, rgba) = match image::decode_file(&file.data) {
        Ok(decoded) => decoded,
        Err(e) => return log::warn!("inline image not shown: {e}"),
    };
    let size = *shared.size.lock().unwrap();
    let cell = (size.cell_width.max(1) as u32, size.cell_height.max(1) as u32);
    let terminal = (cell.0 * size.columns as u32, cell.1 * size.lines as u32);
    let display = image::iterm_display_size(&file, (width, height), cell, terminal);
    let id = shared.images.lock().unwrap().add(width, height, rgba);
    place_image(id, display, true, false, parser, term, shared);
}

/// `APC G ...`: the kitty graphics protocol.
fn handle_kitty(
    apc: &[u8],
    parser: &mut Processor,
    term: &FairMutex<TermState>,
    listener: &Listener,
    shared: &Shared,
) {
    let Some((mut command, data)) = KittyCommand::parse(apc) else { return };
    let more_chunks = command.number(b'm') == Some(1);

    // Large images arrive in chunks; only the first carries the settings.
    let mut data = data.to_vec();
    {
        let mut images = shared.images.lock().unwrap();
        if let Some((first, mut collected)) = images.partial.take() {
            collected.extend_from_slice(&data);
            command = first;
            data = collected;
        }
        if more_chunks {
            images.partial = Some((command, data));
            return;
        }
    }

    let action = command.letter(b'a', b't');
    let number = command.number(b'i');
    let size = *shared.size.lock().unwrap();
    let (cell_width, cell_height) = (size.cell_width.max(1) as f32, size.cell_height.max(1) as f32);
    // The size to show an image at: the requested columns/rows if given,
    // otherwise its natural pixel size.
    let display_size = |width: u32, height: u32| {
        let (natural_w, natural_h) = (width as f32, height as f32);
        match (command.number(b'c'), command.number(b'r')) {
            (Some(c), Some(r)) => (c as f32 * cell_width, r as f32 * cell_height),
            (Some(c), None) => (c as f32 * cell_width, c as f32 * cell_width * natural_h / natural_w),
            (None, Some(r)) => (r as f32 * cell_height * natural_w / natural_h, r as f32 * cell_height),
            (None, None) => (natural_w, natural_h),
        }
    };
    let keep_cursor = command.number(b'C') == Some(1);

    let result: Result<(), String> = match action {
        // Query: can you handle this? Decode and discard.
        b'q' => image::decode_kitty(&command, &data).map(|_| ()),
        // Transmit, and for `T` also display.
        b't' | b'T' => image::decode_kitty(&command, &data).map(|(width, height, rgba)| {
            let id = {
                let mut images = shared.images.lock().unwrap();
                let id = images.add(width, height, rgba);
                // Reusing a number replaces the earlier image.
                let previous = images.kitty.insert(number.unwrap_or(0), (id, width, height));
                if let Some((old, ..)) = previous {
                    images.remove(old);
                }
                id
            };
            if action == b'T' {
                place_image(id, display_size(width, height), false, keep_cursor, parser, term, shared);
            }
        }),
        // Display an image transmitted earlier.
        b'p' => {
            let known = shared.images.lock().unwrap().kitty.get(&number.unwrap_or(0)).copied();
            match known {
                Some((id, width, height)) => {
                    place_image(id, display_size(width, height), false, keep_cursor, parser, term, shared);
                    Ok(())
                }
                None => Err("ENOENT:no such image".into()),
            }
        }
        // Delete: one image by number, or all of them.
        b'd' => {
            let mut images = shared.images.lock().unwrap();
            let by_number = matches!(command.letter(b'd', b'a'), b'i' | b'I');
            let numbers: Vec<u32> = match number {
                Some(number) if by_number => vec![number],
                _ => images.kitty.keys().copied().collect(),
            };
            for number in numbers {
                if let Some((id, ..)) = images.kitty.remove(&number) {
                    images.remove(id);
                }
            }
            Ok(())
        }
        _ => Err("ENOTSUP:unsupported action".into()),
    };

    // Reply only when the program gave the image a number (it uses the
    // number to match replies), honouring its "quiet" setting.
    let quiet = command.number(b'q').unwrap_or(0);
    if let Some(number) = number {
        let message = match &result {
            Ok(()) if quiet == 0 && action != b'd' => Some("OK"),
            Err(error) if quiet < 2 => Some(error.as_str()),
            _ => None,
        };
        if let Some(message) = message {
            listener.write(format!("\x1b_Gi={number};{message}\x1b\\").into_bytes());
        }
    }
}
