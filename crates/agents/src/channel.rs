//! How agents report their status to the terminal: a Unix socket on this
//! machine.
//!
//! The terminal listens on a socket in its own state folder and tells every
//! shell it starts where that is (environment variables). An agent's hook
//! runs `ale-term hook <agent>`, which connects, sends one line of JSON and
//! exits. Nothing leaves the machine and nothing listens on the network.
//!
//! Some agents run hooks with a stripped-down environment, so the variables
//! may be missing. The client then falls back to trying every socket in
//! the folder, identifying its pane by the terminal device it is attached
//! to; only the window that owns that terminal acts on the message.
//!
//! Anything running as the same user could connect and send messages, so
//! messages are treated as untrusted: they are size-limited, parsed
//! defensively, and can only change what the status display shows.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Environment variables given to every shell the terminal starts.
pub const ENV_SOCKET: &str = "ALE_TERM_SOCKET";
pub const ENV_PANE: &str = "ALE_TERM_PANE";
/// Path of the terminal's own executable, which doubles as the hook client.
pub const ENV_BIN: &str = "ALE_TERM_BIN";

/// Largest message accepted. Hook payloads can include a whole prompt or a
/// file being written, so this is generous, but bounded.
const MAX_MESSAGE: u64 = 1024 * 1024;

/// One status report from an agent hook.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct HookMessage {
    /// The pane the agent runs in (from `ALE_TERM_PANE`); 0 when unknown.
    #[serde(default)]
    pub pane: u64,
    /// Device number of the sender's terminal: a second way to find the
    /// pane, for when the environment variable did not survive.
    #[serde(default)]
    pub tty: Option<u64>,
    /// Which agent sent it: "claude", "codex", ...
    pub agent: String,
    /// The hook event name. May be empty when the payload carries it.
    #[serde(default)]
    pub event: String,
    /// The hook's own JSON, passed through untouched.
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// Sends one message to the terminal. Used by the hook client.
pub fn send(socket: &Path, message: &HookMessage) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let mut line = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)
}

/// The listening side. Dropping it removes the socket file.
pub struct Server {
    path: PathBuf,
}

impl Server {
    /// Starts listening at `path` and calls `on_message` (on a background
    /// thread) for every valid message.
    pub fn start(
        path: PathBuf,
        on_message: impl Fn(HookMessage) + Send + 'static,
    ) -> std::io::Result<Server> {
        // A leftover file from a crashed run would block binding.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        std::thread::Builder::new().name("agent-status".into()).spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                if let Some(message) = read_message(stream) {
                    on_message(message);
                }
            }
        })?;
        Ok(Server { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Reads one line of JSON from a connection. A client that is slow, sends
/// too much or sends nonsense is simply dropped.
fn read_message(stream: UnixStream) -> Option<HookMessage> {
    // Setting the timeout fails on macOS when the client has already sent
    // its message and hung up, which is the normal case; the data is still
    // there to read, so carry on.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let mut line = Vec::new();
    BufReader::new(stream.take(MAX_MESSAGE)).read_until(b'\n', &mut line).ok()?;
    match serde_json::from_slice(&line) {
        Ok(message) => Some(message),
        Err(e) => {
            log::debug!("ignored a malformed status message: {e}");
            None
        }
    }
}

/// Folder holding the sockets of all running windows.
pub fn socket_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("run")
}

/// Where the socket for this process goes. Named after the process so
/// several windows do not collide.
pub fn socket_path(state_dir: &Path) -> PathBuf {
    socket_dir(state_dir).join(format!("{}.sock", std::process::id()))
}

/// Removes sockets left behind by processes that no longer exist.
pub fn remove_stale_sockets(state_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(socket_dir(state_dir)) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "sock") && UnixStream::connect(&path).is_err() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The hook client: what `ale-term hook <agent> [event]` runs.
///
/// Reads the hook's JSON from `input`, wraps it and sends it to the
/// terminal. `tty` is the device number of the caller's terminal and
/// `state_dir` the terminal's state folder, used when the environment does
/// not say where to send.
///
/// Always succeeds from the caller's point of view and prints nothing: a
/// hook must never disturb the agent, whether or not the terminal is there
/// to listen.
pub fn run_hook_client(agent: &str, event: &str, tty: Option<u64>, state_dir: Option<&Path>, input: impl Read) {
    let pane = std::env::var(ENV_PANE).ok().and_then(|pane| pane.parse::<u64>().ok());
    let socket = std::env::var_os(ENV_SOCKET).map(PathBuf::from);
    if pane.is_none() && tty.is_none() {
        // Not inside one of our panes as far as we can tell.
        return;
    }
    let mut text = String::new();
    if input.take(MAX_MESSAGE - 4096).read_to_string(&mut text).is_err() {
        return;
    }
    let mut payload = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    // Kiro hands a hook the prompt in the environment, not in its data.
    if agent == "kiro"
        && payload.get("prompt").is_none()
        && let Ok(prompt) = std::env::var("USER_PROMPT")
    {
        if !payload.is_object() {
            payload = serde_json::json!({});
        }
        if let Some(object) = payload.as_object_mut() {
            object.insert("prompt".into(), prompt.into());
        }
    }
    let message = HookMessage {
        pane: pane.unwrap_or(0),
        tty,
        agent: agent.to_owned(),
        event: event.to_owned(),
        payload,
    };
    match (socket, pane) {
        (Some(socket), Some(_)) => {
            let _ = send(&socket, &message);
        }
        // No address: offer the message to every window; the one that owns
        // our terminal will recognize it.
        _ => {
            let Some(dir) = state_dir.map(socket_dir) else { return };
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let _ = send(&entry.path(), &message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn test_socket(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ale-term-test-{name}-{}.sock", std::process::id()))
    }

    #[test]
    fn messages_arrive_and_bad_ones_are_ignored() {
        let path = test_socket("channel");
        let (tx, rx) = mpsc::channel();
        let server = Server::start(path.clone(), move |message| tx.send(message).unwrap()).unwrap();

        // Garbage, then an oversized line, then a valid message.
        UnixStream::connect(&path).unwrap().write_all(b"not json\n").unwrap();
        let huge = vec![b'x'; MAX_MESSAGE as usize + 10];
        let _ = UnixStream::connect(&path).unwrap().write_all(&huge);
        let message = HookMessage {
            pane: 7,
            tty: Some(42),
            agent: "claude".into(),
            event: "Stop".into(),
            payload: serde_json::json!({ "session_id": "abc" }),
        };
        send(&path, &message).unwrap();

        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), message);
        assert!(rx.try_recv().is_err(), "invalid messages must not be delivered");

        // Only the owner may connect, and the file goes away with the server.
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        drop(server);
        assert!(!path.exists());
    }

    #[test]
    fn missing_fields_default() {
        let message: HookMessage = serde_json::from_str(r#"{"pane":1,"agent":"codex"}"#).unwrap();
        assert_eq!(message.event, "");
        assert_eq!((message.pane, message.tty), (1, None));
        assert!(message.payload.is_null());
    }
}
