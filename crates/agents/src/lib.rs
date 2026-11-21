//! Coding agents running in the terminal: recognizing them, receiving their
//! status reports and keeping track of what each one is doing.
//!
//! This crate has no user interface. The app feeds it events (a command
//! started, a hook message arrived, the pane printed something) and reads
//! back each session's state to draw.

pub mod archive;
pub mod available;
pub mod channel;
pub mod hooks;
pub mod hooks_claude;
pub mod install;
pub mod session;
pub mod transcript;

pub use archive::{Archive, ArchivedSession};
pub use channel::{HookMessage, Server};
pub use session::{AgentKind, AgentState, BackgroundTask, Change, Session, Subagent};
