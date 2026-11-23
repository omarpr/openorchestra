//! The hosted zed workspaces behind the tools panel and the Editor tab: which
//! one exists per worktree, the Files/Git/Search panels, and keeping file tabs
//! in step with zed's panes.

use super::*;

impl HarnessWorkspace {
    /// Show or hide the right-hand tools panel (Files / Git / Review).
    ///
    /// Always available so Files/Git stay reachable even when Review lives in a
    /// center tab.
    pub(crate) fn toggle_diff(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let was_visible = self.ports_visible();
        self.prefs.diff_open = !self.prefs.diff_open;
        // Reopening onto Ports: rows from before the panel closed may be
        // stale, so sample now rather than at the next poll.
        if !was_visible && self.ports_visible() {
            self.refresh_ports(cx);
        }
        self.save_prefs(cx);
        cx.notify();
    }

    /// Select a tools-panel tab, open the panel if needed, and focus the surface.
    pub(crate) fn set_tool_tab(
        &mut self,
        tab: ToolTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prefs.tool_tab = tab;
        self.prefs.diff_open = true;
        match tab {
            ToolTab::Files => {
                let _ = self.ensure_service_workspace(window, cx);
                match self.files_sub_tab {
                    FilesSubTab::Explorer => {
                        self.activate_hosted_tool_panel(ToolTab::Files, window, cx);
                    }
                    FilesSubTab::Search => {
                        self.ensure_project_search_ui(window, cx);
                        if let Some(ui) = self.project_search.as_ref() {
                            window.focus(&ui.query.focus_handle(cx), cx);
                        }
                    }
                }
            }
            ToolTab::Git => {
                // Keep the current center tab (Terminal/Chat/…); tools host the
                // service Workspace without promoting an Editor chrome tab.
                let _ = self.ensure_service_workspace(window, cx);
                self.activate_hosted_tool_panel(tab, window, cx);
            }
            ToolTab::Review if self.prefs.diff_location == DiffLocation::Tab => {
                self.open_tab(TabKind::Diff, window, cx);
            }
            ToolTab::Review => {
                window.focus(&self.diff_surface_focus, cx);
            }
            ToolTab::Ports => self.refresh_ports(cx),
            ToolTab::Device => {}
        }
        self.save_prefs(cx);
        cx.notify();
    }

    /// ⌘⇧E / ⌘E — open tools on Files → Explorer and focus the tree.
    pub(crate) fn focus_files_tool(
        &mut self,
        _: &crate::FocusFilesTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.files_sub_tab = FilesSubTab::Explorer;
        self.set_tool_tab(ToolTab::Files, window, cx);
    }

    /// ⌘⇧F — open tools on Files → Search and focus the query field.
    pub(crate) fn focus_project_search(
        &mut self,
        _: &crate::FocusProjectSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.resolve().is_none() {
            self.set_note("select a project first".into());
            cx.notify();
            return;
        }
        self.files_sub_tab = FilesSubTab::Search;
        self.prefs.tool_tab = ToolTab::Files;
        self.prefs.diff_open = true;
        let _ = self.ensure_service_workspace(window, cx);
        self.ensure_project_search_ui(window, cx);
        if let Some(ui) = self.project_search.as_ref() {
            window.focus(&ui.query.focus_handle(cx), cx);
        }
        self.save_prefs(cx);
        cx.notify();
    }

    /// ⌘⇧G — open tools on Git and focus the panel.
    pub(crate) fn focus_git_tool(
        &mut self,
        _: &crate::FocusGitTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_tool_tab(ToolTab::Git, window, cx);
    }

    /// Open tools on Review (and focus the diff surface).
    pub(crate) fn focus_review_tool(
        &mut self,
        _: &crate::FocusReviewTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_tool_tab(ToolTab::Review, window, cx);
    }

    /// Mark the hosted Files/Git panel active and focus it; keep docks closed.
    pub(crate) fn activate_hosted_tool_panel(
        &mut self,
        tab: ToolTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui::Focusable as _;
        use workspace::Panel as _;

        let Some(hosted) = self.editor_workspace() else {
            return;
        };
        hosted.update(cx, |ws, cx| {
            for dock in ws.all_docks() {
                if dock.read(cx).is_open() {
                    dock.update(cx, |dock, cx| dock.set_open(false, window, cx));
                }
            }
        });
        match tab {
            ToolTab::Files => {
                if let Some(panel) = hosted.read(cx).panel::<project_panel::ProjectPanel>(cx) {
                    panel.update(cx, |p, cx| {
                        p.set_active(true, window, cx);
                        window.focus(&p.focus_handle(cx), cx);
                    });
                }
            }
            ToolTab::Git => {
                if let Some(panel) = hosted.read(cx).panel::<git_ui::git_panel::GitPanel>(cx) {
                    panel.update(cx, |p, cx| {
                        p.set_active(true, window, cx);
                        window.focus(&p.focus_handle(cx), cx);
                    });
                }
            }
            ToolTab::Review | ToolTab::Ports | ToolTab::Device => {}
        }
    }

    /// Service Workspace for the active selection, if already created.
    /// The full-editor workspace for the selected worktree, created on first
    /// use over the *service* workspace's project (see
    /// `workspace_host::ide_workspace_for` for why they share it).
    pub(crate) fn ensure_ide_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<::workspace::Workspace>> {
        let key = self.resolve()?.key;
        if let Some(ws) = self.ide_workspaces.get(&key) {
            return Some(ws.clone());
        }
        let service = self.ensure_service_workspace(window, cx)?;
        let project = service.read(cx).project().clone();
        match crate::workspace_host::ide_workspace_for(project, window, cx) {
            Ok(ws) => {
                cx.subscribe_in(
                    &ws,
                    window,
                    |this, hosted, event: &workspace::Event, window, cx| {
                        if matches!(event, workspace::Event::ModalOpened) {
                            this.watch_finder_dismiss(hosted, window, cx);
                        }
                    },
                )
                .detach();
                self.ide_workspaces.insert(key, ws.clone());
                Some(ws)
            }
            Err(e) => {
                log::warn!("could not open the editor workspace: {e}");
                None
            }
        }
    }

    /// Hand focus back when the ⌘P finder closes without going anywhere.
    ///
    /// zed's `ModalLayer` restores the pre-modal focus only if its own
    /// wrapper element (which tracks a private focus handle) holds focus.
    /// Harness draws the finder itself (`render_zed_modal`) — zed v1.2x
    /// leaves that to a `MultiWorkspace` we don't host — so that wrapper
    /// never exists and the restore never fires: Escape left focus on a
    /// finder that was no longer on screen, and the editor stopped getting
    /// keys. If focus is still inside the finder once it is dismissed,
    /// return it to the active tab. A confirm that opened a file has
    /// already moved focus to that file, so this leaves it alone.
    pub(crate) fn watch_finder_dismiss(
        &mut self,
        hosted: &Entity<::workspace::Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(finder) = hosted.read(cx).active_modal::<file_finder::FileFinder>(cx) else {
            return;
        };
        self.finder_dismiss_sub = Some(cx.subscribe_in(
            &finder,
            window,
            |_, finder, _: &gpui::DismissEvent, window, cx| {
                let finder_focus = finder.focus_handle(cx);
                // Deferred so zed's own dismissal (and any file open that
                // moves focus) has run first.
                cx.defer_in(window, move |this, window, cx| {
                    if finder_focus.contains_focused(window, cx) {
                        this.focus_active_pane(window, cx);
                    }
                });
            },
        ));
    }

    /// The selected worktree's active Editor tab workspace, if that tab is
    /// the one showing.
    pub(crate) fn active_ide_workspace(&self) -> Option<Entity<::workspace::Workspace>> {
        let r = self.resolve()?;
        match &self.panes.get(&r.key)?.active_tab()?.content {
            TabContent::Editor(ws) => Some(ws.clone()),
            _ => None,
        }
    }

    pub(crate) fn editor_workspace(&self) -> Option<Entity<::workspace::Workspace>> {
        let key = self.resolve()?.key;
        self.service_workspaces.get(&key).cloned()
    }

    /// Create or return the per-worktree service Workspace (Files/Git + file tabs).
    pub(crate) fn ensure_service_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<::workspace::Workspace>> {
        let r = self.resolve()?;
        let key = r.key.clone();
        if let Some(ws) = self.service_workspaces.get(&key) {
            return Some(ws.clone());
        }
        match crate::workspace_host::workspace_for(&r.editor_path, window, cx) {
            Ok(hosted) => {
                // Hide Zed's nested tab bar — harness owns the strip.
                self.wire_service_workspace(&hosted, window, cx);
                self.service_workspaces.insert(key, hosted.clone());
                // Ensure a pane set exists so file tabs have somewhere to land.
                self.pane_clock = self.pane_clock.wrapping_add(1);
                self.panes
                    .entry(r.key.clone())
                    .or_insert_with(|| PaneSet {
                        tabs: Vec::new(),
                        active: 0,
                        last_used: 0,
                    })
                    .last_used = self.pane_clock;
                cx.notify();
                Some(hosted)
            }
            Err(e) => {
                log::warn!(
                    "could not open project for {}: {e}",
                    r.editor_path.display()
                );
                self.set_error_note(format!("could not open project: {e}"));
                cx.notify();
                None
            }
        }
    }

    /// Alias used by the tools empty-state button.
    pub(crate) fn ensure_editor_workspace_quiet(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<::workspace::Workspace>> {
        self.ensure_service_workspace(window, cx)
    }

    /// Subscribe to a hosted Zed Workspace so harness file tabs track preview
    /// ↔ permanent the same way Zed's own tab bar does (edit / double-click).
    pub(crate) fn wire_service_workspace(
        &mut self,
        hosted: &Entity<::workspace::Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        hosted.update(cx, |ws, cx| {
            for pane in ws.panes() {
                pane.update(cx, |pane, _| {
                    pane.set_should_display_tab_bar(|_, _| false);
                });
            }
        });
        let panes: Vec<_> = hosted.read(cx).panes().to_vec();
        for pane in &panes {
            self.observe_service_pane_preview(pane, cx);
        }
        cx.observe(hosted, |this, _, cx| {
            this.sync_file_preview_flags(cx);
            cx.notify();
        })
        .detach();
        cx.subscribe_in(hosted, window, {
            move |this, hosted, event: &workspace::Event, window, cx| match event {
                workspace::Event::ItemAdded { item } => {
                    this.on_service_item_added(hosted, item.as_ref(), window, cx);
                }
                workspace::Event::ItemRemoved { item_id } => {
                    this.on_service_item_removed(*item_id, window, cx);
                }
                workspace::Event::ActiveItemChanged => {
                    this.on_service_active_item_changed(hosted, window, cx);
                }
                workspace::Event::PaneAdded(pane) => {
                    pane.update(cx, |pane, _| {
                        pane.set_should_display_tab_bar(|_, _| false);
                    });
                    this.observe_service_pane_preview(pane, cx);
                }
                workspace::Event::ModalOpened => {
                    this.watch_finder_dismiss(hosted, window, cx);
                }
                _ => {}
            }
        })
        .detach();
    }

    pub(crate) fn observe_service_pane_preview(
        &mut self,
        pane: &Entity<::workspace::Pane>,
        cx: &mut Context<Self>,
    ) {
        // Edits clear `preview_item_id` on the pane; observe so italics drop
        // without waiting for ActiveItemChanged.
        cx.observe(pane, |this, _, cx| {
            this.sync_file_preview_flags(cx);
            cx.notify();
        })
        .detach();
    }

    /// Mirror Zed's `preview_item_id` onto harness `TabContent::File.preview`.
    pub(crate) fn sync_file_preview_flags(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else {
            return;
        };
        let Some(hosted) = self.service_workspaces.get(&r.key).cloned() else {
            return;
        };
        let Some(pane_set) = self.panes.get_mut(&r.key) else {
            return;
        };
        let mut changed = false;
        for tab in &mut pane_set.tabs {
            let TabContent::File { item_id, preview } = &mut tab.content else {
                continue;
            };
            let is_preview = hosted
                .read(cx)
                .panes()
                .iter()
                .any(|pane| pane.read(cx).is_active_preview_item(*item_id));
            if *preview != is_preview {
                *preview = is_preview;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// Double-click a harness file tab → pin it (Zed `unpreview_item_if_preview`).
    pub(crate) fn pin_file_tab(
        &mut self,
        tab_id: u64,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = self.resolve() else {
            return;
        };
        let item_id = {
            let Some(pane) = self.panes.get(&r.key) else {
                return;
            };
            let Some(tab) = pane.tabs.iter().find(|t| t.id == tab_id) else {
                return;
            };
            match &tab.content {
                TabContent::File {
                    item_id,
                    preview: true,
                } => *item_id,
                _ => return,
            }
        };
        if let Some(hosted) = self.service_workspaces.get(&r.key).cloned() {
            hosted.update(cx, |ws, cx| {
                for pane in ws.panes() {
                    pane.update(cx, |pane, _| {
                        pane.unpreview_item_if_preview(item_id);
                    });
                }
            });
        }
        self.sync_file_preview_flags(cx);
        cx.notify();
    }

    pub(crate) fn on_service_item_added(
        &mut self,
        hosted: &Entity<::workspace::Workspace>,
        item: &dyn workspace::ItemHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = self.resolve() else { return };
        let key = r.key.clone();
        let item_id = item.item_id();
        let title = item.tab_content_text(0, cx).to_string();
        let preview = hosted
            .read(cx)
            .active_pane()
            .read(cx)
            .is_active_preview_item(item_id);

        // Allocate id before borrowing panes mutably.
        let existing_id = self.panes.get(&key).and_then(|pane| {
            pane.tabs.iter().find_map(|t| match &t.content {
                TabContent::File { item_id: id, .. } if *id == item_id => Some(t.id),
                _ => None,
            })
        });
        let new_id = existing_id.unwrap_or_else(|| self.next_tab_id());
        let key_for_slot = key.clone();

        let pane = self.panes.entry(key).or_insert_with(|| PaneSet {
            tabs: Vec::new(),
            active: 0,
            last_used: 0,
        });
        // A new preview replaces the old one *in place*. It used to drop the
        // old preview tab and append the new one, so browsing files from the
        // tree made the italic tab jump to the end of the strip each time.
        let mut replace_at = None;
        if preview {
            let is_other_preview = |t: &Tab| matches!(&t.content, TabContent::File { preview: true, item_id: id } if *id != item_id);
            replace_at = pane.tabs.iter().position(&is_other_preview);
            pane.tabs.retain(|t| !is_other_preview(t));
        }
        if let Some(existing) = pane
            .tabs
            .iter_mut()
            .find(|t| matches!(&t.content, TabContent::File { item_id: id, .. } if *id == item_id))
        {
            existing.title = title;
            existing.content = TabContent::File { item_id, preview };
            pane.active = existing.id;
        } else {
            let tab = Tab {
                id: new_id,
                title,
                content: TabContent::File { item_id, preview },
                agent: None,
            };
            // The slot of a preview zed already closed (see `preview_slot`).
            let freed = self
                .preview_slot
                .take()
                .filter(|(k, _, at)| {
                    preview
                        && *k == key_for_slot
                        && at.elapsed() < std::time::Duration::from_millis(500)
                })
                .map(|(_, ix, _)| ix);
            match replace_at.or(freed) {
                Some(ix) if ix <= pane.tabs.len() => pane.tabs.insert(ix, tab),
                _ => pane.tabs.push(tab),
            }
            pane.active = new_id;
        }
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    pub(crate) fn on_service_item_removed(
        &mut self,
        item_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = self.resolve() else { return };
        let key = r.key.clone();
        let Some(pane) = self.panes.get_mut(&key) else {
            return;
        };
        let closed_index = pane.tabs.iter().position(
            |t| matches!(&t.content, TabContent::File { item_id: id, .. } if *id == item_id),
        );
        // Any closed file tab's slot, not just a flagged preview's: zed's
        // `close_current_preview_item` clears the preview id *before* removing
        // the item, so our flag sync has already marked the tab non-preview by
        // the time this runs. The timestamp below limits reuse to a preview
        // that arrives in the same replacement.
        self.preview_slot = closed_index.map(|ix| (key, ix, std::time::Instant::now()));
        let Some(ix) = closed_index else { return };
        pane.remove_tab_at(ix);
        if pane.tabs.is_empty() {
            self.open_tab(TabKind::Shell, window, cx);
        } else {
            self.focus_active_pane(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn on_service_active_item_changed(
        &mut self,
        hosted: &Entity<::workspace::Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_file_preview_flags(cx);
        let Some(r) = self.resolve() else { return };
        if self.service_workspaces.get(&r.key) != Some(hosted) {
            return;
        }
        let key = r.key.clone();
        // A close writes this slot, then Zed emits a follow-up activation
        // after focus has left the editor. An Explorer click on a file that
        // is already open is also unfocused, and that one must select the tab.
        let follows_close = self.preview_slot.as_ref().is_some_and(|(slot_key, _, at)| {
            activation_follows_close(Some(slot_key), &key, at.elapsed())
        });
        let Some(pane) = self.panes.get_mut(&key) else {
            return;
        };
        let service = hosted.read(cx);
        // Zed can activate its own fallback before emitting ItemRemoved. Let
        // the removal handler choose the neighbor from the visible tab order.
        if let Some(TabContent::File { item_id, .. }) = pane.active_tab().map(|tab| &tab.content)
            && !service
                .panes()
                .iter()
                .any(|pane| pane.read(cx).items().any(|item| item.item_id() == *item_id))
        {
            return;
        }
        let Some(item) = service.active_item(cx) else {
            return;
        };
        // A close emits a follow-up activation after focus has moved to the
        // neighbor. Ignore that one. Any other unfocused activation is
        // Explorer selecting a file that is already open.
        let focus = item.item_focus_handle(cx);
        if !focus.is_focused(window) && !focus.contains_focused(window, cx) && follows_close {
            return;
        }
        let item_id = item.item_id();
        if let Some(tab) = pane
            .tabs
            .iter()
            .find(|t| matches!(&t.content, TabContent::File { item_id: id, .. } if *id == item_id))
            && pane.active != tab.id
        {
            pane.active = tab.id;
            self.focus_active_pane(window, cx);
            cx.notify();
        }
    }

    /// Switch bodies between unified and side-by-side.
    ///
    /// Costs nothing beyond a repaint: both layouts are derived from the same
    /// parsed rows, so the cache survives and no diff is re-fetched.
    pub(crate) fn set_diff_view(&mut self, view: DiffView, cx: &mut Context<Self>) {
        if self.prefs.diff_view == view {
            return;
        }
        self.prefs.diff_view = view;
        save_prefs_to(&self.prefs_path, &self.prefs);
        cx.notify();
    }

    /// Move the diff surface between the dock and the tab strip.
    ///
    /// Switching to `Dock` closes every open Diff tab across all live panes: a
    /// tab drawing a surface that has moved elsewhere is a tab that renders
    /// nothing, and leaving one behind would look like the setting half-worked.
    pub(crate) fn set_diff_location(
        &mut self,
        location: DiffLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.prefs.diff_location == location {
            return;
        }
        self.prefs.diff_location = location;
        if location == DiffLocation::Dock {
            for pane in self.panes.values_mut() {
                for ix in (0..pane.tabs.len()).rev() {
                    if matches!(pane.tabs[ix].content, TabContent::Diff) {
                        pane.remove_tab_at(ix);
                    }
                }
            }
            // A worktree left with no tabs is a dead end; give it a shell back,
            // the same guarantee close_tab makes.
            if self
                .resolve()
                .and_then(|r| self.panes.get(&r.key))
                .is_some_and(|p| p.tabs.is_empty())
            {
                self.open_tab(TabKind::Shell, window, cx);
            }
            // Moving the diff back to the dock should also show it there.
            self.prefs.diff_open = true;
            self.save_panes(cx);
        }
        self.save_prefs(cx);
        self.focus_active_pane(window, cx);
        cx.notify();
    }
}

/// True when an unfocused `ActiveItemChanged` is the follow-up Zed emits
/// while replacing a preview, not an Explorer click on an open file.
fn activation_follows_close(slot_key: Option<&str>, key: &str, age: std::time::Duration) -> bool {
    slot_key.is_some_and(|slot_key| slot_key == key) && age < std::time::Duration::from_millis(500)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_fresh_close_in_the_same_worktree_suppresses_an_unfocused_activation() {
        let fresh = std::time::Duration::from_millis(10);
        let stale = std::time::Duration::from_millis(500);
        assert!(activation_follows_close(Some("work"), "work", fresh));
        assert!(!activation_follows_close(Some("other"), "work", fresh));
        assert!(!activation_follows_close(Some("work"), "work", stale));
        assert!(!activation_follows_close(None, "work", fresh));
    }
}
