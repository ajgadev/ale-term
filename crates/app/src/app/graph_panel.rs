//! The agent graph panel: deciding when it shows, gathering what it shows
//! (the focused agent, its subagents and background tasks, and the selected
//! node's recent output), and handling clicks on it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use ale_agents::transcript::{self, Entry};
use ale_agents::{AgentState, Session};

use super::*;
use crate::graph::{self, Model, Node, NodeId, OutputKind};
use crate::sidebar::state_color;

/// How many transcript entries the output view keeps.
const OUTPUT_ENTRIES: usize = 80;

#[derive(Default)]
pub struct GraphState {
    /// The user wants the panel; it shows when the focused pane has an agent.
    pub visible: bool,
    pub selected: NodeId,
    /// The last transcript read, so it is only reread when the file changes.
    cache: Option<(PathBuf, (u64, SystemTime), Vec<Entry>)>,
    /// Whether the panel took up space in the last frame. A change means
    /// the terminals need resizing.
    pub shown: bool,
    /// When the panel is next redrawn to pick up new output.
    refresh: Option<Instant>,
    /// How big each conversation is, by transcript, and the state of the
    /// file when that was read.
    usage: HashMap<PathBuf, ((u64, SystemTime), Option<transcript::Usage>)>,
}

/// Only conversation records under the agent's own folder are read. The
/// path comes from a hook message, which any local process could forge.
///
/// Both sides are resolved first, so neither `..` nor a link inside the
/// folder can lead out of it.
fn readable(path: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME") else { return false };
    inside(path, &Path::new(&home).join(".claude"))
}

/// Whether the existing file `path` really lies within `folder`.
fn inside(path: &Path, folder: &Path) -> bool {
    match (path.canonicalize(), folder.canonicalize()) {
        (Ok(path), Ok(folder)) => path.starts_with(folder),
        _ => false,
    }
}

impl App {
    /// The pane whose agent the panel shows, if the panel is to be shown.
    fn graph_pane(&self) -> Option<PaneId> {
        if !self.graph.visible {
            return None;
        }
        let id = self.focused_pane_id()?;
        self.panes.get(&id)?.agent.as_ref().map(|_| id)
    }

    /// Width the panel takes from the terminal area, or 0 when hidden.
    pub(super) fn graph_width(&self) -> f32 {
        if self.graph_pane().is_none() {
            return 0.0;
        }
        let scale = self.scale();
        let available = self.window_size().0 - self.sidebar_width();
        let width = (available * 0.36).clamp(300.0 * scale, 520.0 * scale);
        // Leave the terminal a usable width, or do not show at all.
        if available - width < 320.0 * scale { 0.0 } else { width.round() }
    }

    pub(super) fn graph_rect(&self) -> Option<Rect> {
        let width = self.graph_width();
        if width <= 0.0 {
            return None;
        }
        let (window_width, window_height) = self.window_size();
        let top = self.content_rect().y;
        Some(Rect::new(window_width - width, top, width, (window_height - top).max(0.0)))
    }

    /// Resizes the terminals when the panel appears or disappears.
    pub(super) fn sync_graph_layout(&mut self) {
        let shown = self.graph_width() > 0.0;
        if shown != self.graph.shown {
            self.graph.shown = shown;
            self.relayout();
        }
    }

    /// Opens the panel for an agent if it has anything to show beyond
    /// itself. Used when the user picks an agent in the sidebar.
    pub(super) fn reveal_graph(&mut self, pane: PaneId, subagent: Option<usize>) {
        let Some(session) = self.panes.get(&pane).and_then(|pane| pane.agent.as_ref()) else { return };
        if session.subagents.is_empty() && session.background_tasks.is_empty() {
            return;
        }
        self.graph.selected = subagent
            .and_then(|index| session.subagents.get(index))
            .map_or(NodeId::Main, |subagent| NodeId::Subagent(subagent.id.clone()));
        self.graph.visible = true;
    }

    /// The agent, its subagents and its background tasks, in tree order,
    /// with the record each one's conversation is kept in.
    fn graph_nodes(&self, pane: &Pane, session: &Session) -> (Vec<Node>, Vec<Option<PathBuf>>) {
        let now = Instant::now();
        let elapsed = |since: Instant, until: Instant| graph::long_elapsed(until.duration_since(since).as_secs());
        let palette = &self.palette;
        let mut transcripts = vec![session.transcript.clone()];
        let mut nodes = vec![Node {
            id: NodeId::Main,
            depth: 0,
            title: session.kind.name().to_owned(),
            detail: self.agent_title(pane, session),
            status: Self::agent_status(session),
            color: state_color(session.state, palette),
            elapsed: elapsed(session.since, now),
            running: true,
            model: None,
            tokens: None,
        }];

        let position = |id: &str| session.subagents.iter().position(|subagent| subagent.id == id);
        let parents: Vec<Option<usize>> =
            session.subagents.iter().map(|subagent| subagent.parent.as_deref().and_then(position)).collect();
        for (index, depth) in graph::tree_order(&parents) {
            let subagent = &session.subagents[index];
            let running = subagent.finished.is_none();
            let status = match (&subagent.activity, &subagent.result, running) {
                (Some(activity), _, true) => activity.clone(),
                (None, _, true) => "Working".to_owned(),
                (_, Some(result), false) => result.clone(),
                (_, None, false) => "Finished".to_owned(),
            };
            let state = if running { AgentState::Working } else { AgentState::Done };
            nodes.push(Node {
                id: NodeId::Subagent(subagent.id.clone()),
                depth,
                title: subagent.kind.clone(),
                detail: subagent.description.clone(),
                status,
                color: state_color(state, palette),
                elapsed: elapsed(subagent.started, subagent.finished.unwrap_or(now)),
                running,
                model: None,
                tokens: None,
            });
            transcripts.push(subagent.transcript.clone());
        }
        for task in &session.background_tasks {
            nodes.push(Node {
                id: NodeId::Task(task.id.clone()),
                depth: 1,
                title: "Background task".to_owned(),
                detail: task.label.clone(),
                status: "Running".to_owned(),
                color: state_color(AgentState::Waiting, palette),
                elapsed: String::new(),
                running: true,
                model: None,
                tokens: None,
            });
            transcripts.push(None);
        }
        (nodes, transcripts)
    }

    /// The tree for the agent in a pane, each node with its model and how
    /// big its conversation is.
    pub(super) fn agent_nodes(&mut self, pane_id: PaneId) -> Option<Vec<Node>> {
        let pane = self.panes.get(&pane_id)?;
        let (mut nodes, transcripts) = self.graph_nodes(pane, pane.agent.as_ref()?);
        for (node, path) in nodes.iter_mut().zip(transcripts) {
            if let Some(usage) = path.and_then(|path| self.transcript_usage(&path)) {
                node.model = usage.model.as_deref().map(crate::usage::model_name);
                node.tokens = Some(usage.context);
            }
        }
        Some(nodes)
    }

    /// What a conversation record says about its size. Read again only
    /// when the file has changed.
    fn transcript_usage(&mut self, path: &Path) -> Option<transcript::Usage> {
        let stamp = transcript::stamp(path)?;
        if let Some((seen, usage)) = self.graph.usage.get(path)
            && *seen == stamp
        {
            return usage.clone();
        }
        let usage = if readable(path) { transcript::usage(path) } else { None };
        // Records of sessions long gone are not kept forever.
        if self.graph.usage.len() >= 64 {
            self.graph.usage.clear();
        }
        self.graph.usage.insert(path.to_owned(), (stamp, usage.clone()));
        usage
    }

    /// Gathers everything the panel draws. Rereads the selected node's
    /// transcript only if the file has changed since the last frame.
    pub(super) fn graph_model(&mut self) -> Option<Model> {
        let pane_id = self.graph_pane()?;
        let nodes = self.agent_nodes(pane_id)?;
        let session = self.panes.get(&pane_id)?.agent.as_ref()?;

        // A selection that no longer exists (its turn ended) falls back to
        // the agent itself.
        let mut selected = self.graph.selected.clone();
        if !nodes.iter().any(|node| node.id == selected) {
            selected = NodeId::Main;
        }
        let path = match &selected {
            NodeId::Main => session.transcript.clone(),
            NodeId::Subagent(id) => {
                session.subagents.iter().find(|subagent| &subagent.id == id).and_then(|s| s.transcript.clone())
            }
            NodeId::Task(_) => None,
        };
        let hooked = session.hooked;
        let is_task = matches!(selected, NodeId::Task(_));

        let mut entries: &[Entry] = &[];
        if let Some(path) = path.filter(|path| readable(path)) {
            let stamp = transcript::stamp(&path);
            let fresh = matches!((&self.graph.cache, stamp), (Some((cached, old, _)), Some(new)) if cached == &path && *old == new);
            if !fresh {
                self.graph.cache = stamp.map(|stamp| {
                    let entries = transcript::tail(&path, OUTPUT_ENTRIES);
                    (path, stamp, entries)
                });
            }
            if let Some((_, _, cached)) = &self.graph.cache {
                entries = cached;
            }
        }
        let output = entries
            .iter()
            .map(|entry| match entry {
                Entry::Prompt(text) => (OutputKind::Prompt, text.clone()),
                Entry::Text(text) => (OutputKind::Text, text.clone()),
                Entry::Tool(text) => (OutputKind::Tool, text.clone()),
            })
            .collect();
        let note = if is_task {
            "Background tasks have no output to show."
        } else if !hooked {
            "Needs the agent's status hooks."
        } else {
            "Nothing recorded yet."
        };
        self.graph.selected = selected.clone();
        Some(Model { nodes, selected, output, note: note.to_owned() })
    }

    /// Handles a click inside the panel.
    pub(super) fn graph_click(&mut self, x: f32, y: f32) {
        let Some(rect) = self.graph_rect() else { return };
        let Some(metrics) = self.renderer.as_ref().map(|r| r.metrics()) else { return };
        let scale = self.scale();
        let Some(model) = self.graph_model() else { return };
        if let Some(id) = graph::hit(&model, rect, (metrics.width, metrics.height), scale, x, y) {
            self.graph.selected = id;
            self.redraw();
        }
    }

    /// While the panel or the map shows an agent that is busy, redraws it
    /// every so often so new output, times and tokens appear without
    /// waiting for the next status report. Returns when it wants to run
    /// next.
    pub(super) fn refresh_graph(&mut self) -> Option<Instant> {
        let busy = self
            .graph_pane()
            .and_then(|id| self.panes.get(&id)?.agent.as_ref())
            .is_some_and(|session| session.state == AgentState::Working || session.running_subagents() > 0);
        let busy = (busy && self.graph_width() > 0.0) || self.map_busy();
        if !busy {
            self.graph.refresh = None;
            return None;
        }
        let now = Instant::now();
        if self.graph.refresh.is_some_and(|at| now >= at) {
            self.graph.refresh = None;
            self.redraw();
        }
        Some(*self.graph.refresh.get_or_insert(now + Duration::from_millis(1500)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_outside_the_folder_are_not_read() {
        let base = std::env::temp_dir().join(format!("ale-term-readable-{}", std::process::id()));
        let folder = base.join("agent");
        std::fs::create_dir_all(folder.join("projects")).unwrap();
        std::fs::write(folder.join("projects/a.jsonl"), "").unwrap();
        std::fs::write(base.join("secret"), "").unwrap();
        std::os::unix::fs::symlink(base.join("secret"), folder.join("link.jsonl")).unwrap();

        assert!(inside(&folder.join("projects/a.jsonl"), &folder));
        // Climbing out with `..`, a link pointing out, and a missing file.
        assert!(!inside(&folder.join("../secret"), &folder));
        assert!(!inside(&folder.join("projects/../../secret"), &folder));
        assert!(!inside(&folder.join("link.jsonl"), &folder));
        assert!(!inside(&folder.join("missing.jsonl"), &folder));
        std::fs::remove_dir_all(&base).unwrap();
    }
}
