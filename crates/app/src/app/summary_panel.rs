//! The summary card in the app: noticing that the pointer rests on an
//! agent (its tab, sidebar row or board card), and gathering what the card
//! says about it.

use std::path::Path;
use std::time::SystemTime;

use super::*;
use crate::summary::Summary;

/// How long the pointer rests on an agent before its summary shows.
const DELAY: Duration = Duration::from_millis(450);

#[derive(Default)]
pub struct SummaryState {
    /// The agent's pane under the pointer, and since when.
    hover: Option<(PaneId, Instant)>,
    /// A click dismissed the card for this pane until the pointer leaves.
    dismissed: Option<PaneId>,
    /// The card is on screen (so leaving needs a redraw).
    shown: bool,
    /// The last message read from a transcript: the file, its size and
    /// time when read, and the message.
    message: Option<(PathBuf, (u64, SystemTime), Option<String>)>,
}

impl App {
    /// The pane of the agent whose tab, sidebar row or board card is under
    /// a window position.
    fn summary_target(&self, x: f32, y: f32) -> Option<PaneId> {
        if self.overlay.is_modal() || !matches!(self.mouse.drag, Drag::None) || self.renaming.is_some() {
            return None;
        }
        let pane = if let Some(TabBarHit::Tab(index)) = self.tab_bar().hit(x, y) {
            let tab = self.visible_tabs().get(index).and_then(|&i| self.tabs.get(i))?;
            tab.focused
        } else if let Some(crate::sidebar::Hit::Row(target)) = self.sidebar_hit(x, y) {
            target.pane
        } else if let Some(crate::dock::Hit::Card(target)) = self.dock_hit(x, y) {
            target.pane
        } else {
            return None;
        };
        self.panes.get(&pane)?.agent.as_ref().map(|_| pane)
    }

    /// Follows the pointer: called whenever what is under it may have
    /// changed.
    pub(super) fn update_summary_hover(&mut self) {
        let target = self.summary_target(self.mouse.x, self.mouse.y);
        if target != self.summary.dismissed {
            self.summary.dismissed = None;
        }
        let target = target.filter(|pane| self.summary.dismissed != Some(*pane));
        if target != self.summary.hover.map(|(pane, _)| pane) {
            self.summary.hover = target.map(|pane| (pane, Instant::now()));
            if self.summary.shown {
                self.redraw();
            }
        }
    }

    /// Test hook: as if the pointer had rested on tab `index` a while.
    pub(super) fn script_summary(&mut self, index: usize) {
        let Some(rect) = self.tab_bar().tabs.get(index).copied() else { return };
        (self.mouse.x, self.mouse.y) = (rect.x + rect.w * 0.4, rect.y + rect.h * 0.6);
        let pane = self.visible_tabs().get(index).and_then(|&i| self.tabs.get(i)).map(|tab| tab.focused);
        self.summary.hover = pane.map(|pane| (pane, Instant::now() - DELAY * 2));
    }

    /// A click hides the card until the pointer moves on to something else.
    pub(super) fn dismiss_summary(&mut self) {
        if let Some((pane, _)) = self.summary.hover.take() {
            self.summary.dismissed = Some(pane);
            if self.summary.shown {
                self.redraw();
            }
        }
    }

    /// The pointer left the window.
    pub(super) fn clear_summary(&mut self) {
        self.summary.hover = None;
        if self.summary.shown {
            self.redraw();
        }
    }

    /// Redraws when a resting pointer's card is due. Returns when that is.
    pub(super) fn tick_summary(&mut self) -> Option<Instant> {
        let (_, since) = self.summary.hover?;
        if self.summary.shown {
            return None;
        }
        let due = since + DELAY;
        if Instant::now() >= due {
            self.redraw();
            return None;
        }
        Some(due)
    }

    /// The card to draw now, if the pointer has rested on an agent long
    /// enough.
    pub(super) fn summary_model(&mut self) -> Option<Summary> {
        let shown = self.build_summary();
        self.summary.shown = shown.is_some();
        shown
    }

    fn build_summary(&mut self) -> Option<Summary> {
        let (pane_id, since) = self.summary.hover?;
        if since.elapsed() < DELAY || self.overlay.is_modal() {
            return None;
        }
        let pane = self.panes.get(&pane_id)?;
        let session = pane.agent.as_ref()?;
        let now = Instant::now();
        let ago = |at: Instant| crate::sidebar::format_elapsed(now.duration_since(at).as_secs());
        let actions = session.recent.iter().rev().map(|(at, action)| (action.clone(), ago(*at))).collect();
        let mut summary = Summary {
            icon: crate::icons::key(session.kind),
            agent: session.kind.name().to_owned(),
            tab: self.tabs.iter().find(|tab| tab.root.panes().contains(&pane_id)).and_then(|tab| tab.custom_title.clone()),
            title: self.agent_title(pane, session),
            state: session.state,
            status: Self::agent_status(session),
            elapsed: super::agents::shown_elapsed(session, now),
            message: None,
            actions,
            hooked: session.hooked,
            plan: session.plan.clone(),
        };
        let transcript = session.transcript.clone();
        summary.message = transcript.and_then(|path| self.last_message(&path));
        Some(summary)
    }

    /// The last thing said in a transcript, read again only when the file
    /// has changed.
    fn last_message(&mut self, path: &Path) -> Option<String> {
        let stamp = ale_agents::transcript::stamp(path)?;
        match &self.summary.message {
            Some((cached, at, message)) if cached == path && *at == stamp => message.clone(),
            _ => {
                let message = ale_agents::transcript::last_message(path);
                self.summary.message = Some((path.to_path_buf(), stamp, message.clone()));
                message
            }
        }
    }
}
