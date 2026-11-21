//! Building each frame: titles, panes, the tab bar and overlays, in the
//! order they are painted.

use super::*;

impl App {
    /// Queues text to be typed into a pane when its shell is ready.
    pub(super) fn type_when_ready(&mut self, id: PaneId, bytes: Vec<u8>) {
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.pending_input = Some(bytes);
        }
    }

    pub(super) fn flush_pending_input(&mut self, id: PaneId) {
        if let Some(pane) = self.panes.get_mut(&id)
            && let Some(bytes) = pane.pending_input.take()
        {
            pane.terminal.write(bytes);
        }
    }

    /// Shells without integration never announce their prompt; give them a
    /// few seconds to start and then type anyway. Returns when the next
    /// queued input falls due, so the caller can wake up for it.
    pub(super) fn flush_overdue_input(&mut self) -> Option<Instant> {
        const GRACE: Duration = Duration::from_secs(3);
        let now = Instant::now();
        let mut overdue = Vec::new();
        let mut next: Option<Instant> = None;
        for (id, pane) in &self.panes {
            if pane.pending_input.is_none() {
                continue;
            }
            let due = pane.created + GRACE;
            if now >= due {
                overdue.push(*id);
            } else {
                next = Some(next.map_or(due, |n| n.min(due)));
            }
        }
        for id in overdue {
            self.flush_pending_input(id);
        }
        next
    }

    /// Refreshes the folder-name titles of panes without shell integration.
    /// At most once a second per pane, since it asks the OS each time.
    /// (Integrated shells report their folder themselves.)
    pub(super) fn refresh_fallback_titles(&mut self) {
        let mut stale = Vec::new();
        for tab in &self.tabs {
            let Some(pane) = self.panes.get_mut(&tab.focused) else { continue };
            if pane.fallback_checked.elapsed() < Duration::from_secs(1) {
                continue;
            }
            pane.fallback_checked = Instant::now();
            if let Some(directory) = pane.terminal.working_directory() {
                pane.fallback_title = folder_name(&directory);
            }
            if !pane.terminal.shell().integrated {
                stale.push(tab.id);
            }
        }
        // Without shell integration nothing tells us when the folder
        // changes, so regroup these tabs on the same slow schedule.
        for id in stale {
            if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                self.sync_workspace(index);
            }
        }
    }

    /// The label for a tab, from its focused pane.
    ///
    /// With shell integration we know what is going on: at a prompt the tab
    /// shows the folder; while a command runs it shows the title that
    /// program set, or else the command itself. Without integration we can
    /// only show the last title set, or the folder.
    pub(super) fn tab_title(&self, tab: &Tab) -> String {
        if tab.start.is_some() {
            return "New session".to_owned();
        }
        if let Some(title) = &tab.custom_title {
            return title.clone();
        }
        let Some(pane) = self.panes.get(&tab.focused) else { return String::new() };
        if let Some(session) = &pane.agent {
            return self.agent_title(pane, session);
        }
        let shell = pane.terminal.shell();
        if !shell.integrated {
            return pane.title.clone().unwrap_or_else(|| pane.fallback_title.clone());
        }
        let folder = shell.cwd.as_deref().map(folder_name).unwrap_or_else(|| pane.fallback_title.clone());
        let Some(command) = shell.command else { return folder };
        match Self::program_title(pane) {
            Some(title) => title,
            None if command.is_empty() => folder,
            None => format!("{command} — {folder}"),
        }
    }

    /// The title set by the program running in a pane, as opposed to one
    /// the shell set while launching it.
    ///
    /// Shell themes often put the command line in the title just as a
    /// command starts. A title only counts as the program's own if it was
    /// set a moment after the command began.
    pub(super) fn program_title(pane: &Pane) -> Option<String> {
        let started = pane.terminal.shell().command_started?;
        let by_program = pane.title_set > started + Duration::from_millis(500);
        pane.title.clone().filter(|_| by_program)
    }

    /// Describes the whole window to the renderer.
    pub(super) fn build_frame(&mut self) {
        self.refresh_fallback_titles();
        // The graph panel takes space from the terminals when it shows.
        self.sync_graph_layout();
        let graph = self.graph_rect().and_then(|rect| Some((rect, self.graph_model()?)));
        let layout = self.active_layout();
        let tab_bar = self.tab_bar();
        let visible = self.visible_tabs();
        let titles: Vec<crate::chrome::TabLabel> = visible
            .iter()
            .map(|i| {
                let tab = &self.tabs[*i];
                // Tabs with an agent get a dot in its state color and,
                // where there is room, what it is doing.
                let session = self.panes.get(&tab.focused).and_then(|pane| pane.agent.as_ref());
                crate::chrome::TabLabel {
                    text: self.tab_title(tab),
                    detail: session
                        .filter(|session| session.state == ale_agents::AgentState::Working)
                        .and_then(|session| session.activity.clone()),
                    dot: self.tab_state(tab).map(|state| crate::sidebar::state_color(state, &self.palette)),
                    icon: session.map(|session| crate::icons::key(session.kind)),
                }
            })
            .collect();
        let sidebar = self.sidebar_rect().map(|rect| (rect, self.sidebar_model(), self.sidebar.scroll));
        let board = matches!(self.overlay, Overlay::Board { .. }).then(|| (self.content_rect(), self.board_model()));
        let active_position = visible.iter().position(|i| *i == self.active).unwrap_or(0);
        let workspace_name = self.workspace_name().to_owned();
        // Rows of an open list overlay, computed before the renderer is
        // borrowed.
        let list_rows: Vec<(String, String)> = match &self.overlay {
            Overlay::Palette { kind, query, .. } => {
                self.list_items(*kind, query).into_iter().map(|item| (item.title, item.detail)).collect()
            }
            _ => Vec::new(),
        };
        let focused_pane = self.focused_pane_id();
        let scale = self.scale();
        let padding = self.padding();
        let window = self.window_size();
        let content = self.content_rect();
        let Some(renderer) = self.renderer.as_mut() else { return };
        let palette = &self.palette;
        let metrics = renderer.metrics();

        // Move newly decoded images to the GPU (for every pane, so images
        // printed in background tabs do not pile up in memory), then keep
        // total image memory within the configured budget.
        for (id, pane) in &self.panes {
            let (added, removed) = pane.terminal.take_images();
            for image in added {
                renderer.upload_image(image_key(*id, image.id), image.width, image.height, &image.rgba);
            }
            for image_id in removed {
                renderer.remove_image(image_key(*id, image_id));
            }
        }
        renderer.limit_image_memory(self.config.terminal.image_memory_mb as usize * 1024 * 1024);
        // Agent icons, drawn at the text's size. Redone when that changes,
        // or if the image budget above pushed one out.
        let icon_size = renderer.metrics().height.round().max(8.0) as u32;
        for kind in ale_agents::AgentKind::ALL {
            let key = crate::icons::key(kind);
            if self.icon_size != icon_size || !renderer.has_image(key) {
                renderer.upload_image(key, icon_size, icon_size, &crate::icons::render(kind, icon_size));
            }
        }
        self.icon_size = icon_size;

        renderer.begin(color(palette.background));
        for (id, rect) in &layout.panes {
            let Some(pane) = self.panes.get(id) else { continue };
            let is_focused = Some(*id) == focused_pane;
            let link = self.hover_link.as_ref().filter(|l| l.pane == *id).map(|l| (l.start, l.end));
            let style =
                PaneStyle {
                palette,
                metrics,
                padding,
                focused: is_focused && self.focused,
                link,
                ligatures: self.config.font.ligatures,
                image_key_base: image_key(*id, 0),
            };
            draw::pane(renderer, &pane.terminal.lock(), *rect, &style);
            if !is_focused {
                // Fade panes that are not receiving input.
                let [r, g, b, _] = color(palette.background);
                renderer.rect(rect.x, rect.y, rect.w, rect.h, [r, g, b, 70]);
            }
        }
        // A tab that has not started anything yet shows the start page.
        if let Some(page) = self.tabs.get(self.active).and_then(|tab| tab.start.as_ref()) {
            crate::start::draw(renderer, page, &workspace_name, content, scale, palette);
        }
        for divider in &layout.dividers {
            let Rect { x, y, w, h } = divider.rect;
            renderer.rect(x, y, w, h, color(palette.border));
        }
        let view = crate::chrome::TabBarView {
            workspace: &workspace_name,
            titles: &titles,
            active: active_position,
            hovered: self.mouse.hover_tab,
        };
        tab_bar.draw(renderer, &view, scale, palette);
        if let Some((rect, model)) = &graph {
            crate::graph::draw(renderer, model, *rect, scale, palette);
        }
        if let Some((rect, model, scroll)) = &sidebar {
            crate::sidebar::draw(renderer, model, *rect, scale, *scroll, palette);
        }

        match &self.overlay {
            Overlay::Palette { kind, query, selected } => {
                let placeholder = kind.placeholder();
                overlay::draw_palette(renderer, placeholder, query, *selected, &list_rows, window, scale, palette);
            }
            Overlay::Input { kind, query } => {
                overlay::draw_palette(renderer, kind.placeholder(), query, 0, &[], window, scale, palette);
            }
            Overlay::Confirm(confirm) => overlay::draw_confirm(renderer, confirm, window, scale, palette),
            Overlay::Search { pane, query, failed } => {
                if let Some(rect) = layout.rect_of(*pane) {
                    overlay::draw_search(renderer, query, *failed, rect, scale, palette);
                }
            }
            Overlay::Settings { selected, editing } => {
                crate::settings::draw(renderer, &self.config, *selected, editing.as_deref(), window, scale, palette);
            }
            Overlay::Board { .. } => {
                if let Some((rect, model)) = &board {
                    crate::board::draw(renderer, model, *rect, scale, palette);
                }
            }
            Overlay::None => {}
        }
    }

    pub(super) fn draw(&mut self) {
        self.build_frame();
        let again = self.renderer.as_mut().is_some_and(|r| r.present());
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            log::info!("first frame {:?} after launch", self.launched.elapsed());
        }
        if again {
            self.redraw();
        }
    }

    /// When scripted test steps run: 1.5 seconds after launch (once the
    /// first shell is up), but always before the snapshot.
    pub(super) fn script_time(&self, snapshot_at: Instant) -> Instant {
        (self.launched + Duration::from_millis(1500)).min(snapshot_at - Duration::from_millis(600))
    }

    /// Saves a picture of the window (for automated visual checks).
    pub(super) fn take_snapshot(&mut self) {
        let Some(path) = self.options.snapshot.clone() else { return };
        // First, what the window itself is showing after a normal draw.
        if let Some(shown_path) = self.options.snapshot_shown.clone() {
            self.build_frame();
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.present();
                if let Some((width, height, rgba)) = renderer.capture_shown()
                    && let Err(e) = write_bmp(&shown_path, width, height, &rgba)
                {
                    eprintln!("ale-term: cannot write {}: {e}", shown_path.display());
                }
            }
        }
        // Twice: the first pass may fill the glyph atlas and ask for a redo.
        self.build_frame();
        self.build_frame();
        if let Some(renderer) = self.renderer.as_mut() {
            let (width, height, rgba) = renderer.capture();
            if let Err(e) = write_bmp(&path, width, height, &rgba) {
                eprintln!("ale-term: cannot write {}: {e}", path.display());
            }
        }
    }
}
