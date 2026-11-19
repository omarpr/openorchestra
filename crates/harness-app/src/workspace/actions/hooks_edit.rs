//! Editing a project's setup/teardown hook scripts in the hosted editor.

use super::*;

impl HarnessWorkspace {
    /// Leave hooks-edit mode and restore a normal project editor on Main.
    ///
    /// Opening a Local script used to call `open_abs_path` on a path under
    /// `~/.openorchestra/hooks/…`, which poisons the hosted zed Project (empty
    /// file tree). Exiting always rebuilds the Main editor against the repo
    /// root so the project panel works again.
    pub(crate) fn exit_hooks_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((repo, _, _)) = self.hooks_edit.take() else {
            return;
        };
        if matches!(&self.selection, Selection::Main(n) if n == &repo)
            && let Some(repo_root) = self.repo_root_for_name(&repo)
        {
            let _ = self.replace_editor_workspace(&repo_root, window, cx);
        }
        cx.notify();
    }

    /// Replace (or create) the service Workspace rooted at `root`.
    pub(crate) fn replace_editor_workspace(
        &mut self,
        root: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<::workspace::Workspace>> {
        let key = self.resolve()?.key;
        // Drop file tabs — they pointed at the previous Workspace's items.
        if let Some(pane) = self.panes.get_mut(&key) {
            // Editor tabs go too: they share the old workspace's project.
            pane.tabs
                .retain(|t| !matches!(t.content, TabContent::File { .. } | TabContent::Editor(_)));
            if pane.tabs.iter().all(|t| t.id != pane.active) {
                pane.active = pane.tabs.last().map(|t| t.id).unwrap_or(0);
            }
        }
        self.service_workspaces.remove(&key);
        self.ide_workspaces.remove(&key);
        match crate::workspace_host::workspace_for(root, window, cx) {
            Ok(ws) => {
                self.wire_service_workspace(&ws, window, cx);
                self.service_workspaces.insert(key, ws.clone());
                Some(ws)
            }
            Err(e) => {
                self.set_note(format!("could not open project: {e}"));
                cx.notify();
                None
            }
        }
    }

    /// Ensure a project hooks script exists and open it in the editor.
    ///
    /// Generates the layer's TOML if needed; the user only edits the shell
    /// script. Repo scripts open in a workspace rooted at the main checkout;
    /// Local scripts open in a workspace rooted at the local hooks dir so the
    /// project panel is not pointed at an out-of-tree abs path.
    pub(crate) fn open_project_hooks(
        &mut self,
        repo: &str,
        phase: HookScriptPhase,
        layer: HookLayer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo_root) = self.repo_root_for_name(repo) else {
            self.set_note(format!("project ‘{repo}’ not found"));
            cx.notify();
            return;
        };
        let script = match ensure_hook_script(layer, phase, &repo_root, &self.data_dir, repo) {
            Ok(p) => p,
            Err(e) => {
                self.set_note(format!("hooks: {e}"));
                cx.notify();
                return;
            }
        };
        self.hooks_edit = Some((repo.to_string(), layer, phase));
        if !matches!(&self.selection, Selection::Main(n) if n == repo) {
            // select() clears hooks_edit — restore after.
            self.select(Selection::Main(repo.to_string()), window, cx);
            self.hooks_edit = Some((repo.to_string(), layer, phase));
        }
        let editor_root = match layer {
            HookLayer::Repo => repo_root.clone(),
            HookLayer::Local => local_hooks_base(&self.data_dir, repo),
        };
        let rel = match layer {
            HookLayer::Repo => format!(".openorchestra/scripts/{}", phase.file_name()),
            HookLayer::Local => format!("scripts/{}", phase.file_name()),
        };
        let Some(hosted) = self.replace_editor_workspace(&editor_root, window, cx) else {
            return;
        };
        crate::workspace_host::open_file_at(&hosted, &editor_root, &rel, None, false, window, cx);
        let layer_label = match layer {
            HookLayer::Repo => "repo",
            HookLayer::Local => "local",
        };
        self.set_note(format!(
            "editing {} {} ({})",
            phase.label(),
            layer_label,
            script.display()
        ));
        cx.notify();
    }

    /// Drop the machine-local hooks override for `repo` and reopen the repo
    /// setup script (committed layer) so editing continues without local files.
    pub(crate) fn clear_project_local_hooks(
        &mut self,
        repo: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match clear_local_hooks(&self.data_dir, repo) {
            Ok(()) => {
                self.set_note(format!("cleared local hooks for ‘{repo}’"));
                self.open_project_hooks(repo, HookScriptPhase::Setup, HookLayer::Repo, window, cx);
            }
            Err(e) => {
                self.set_note(format!("clear local hooks: {e}"));
                cx.notify();
            }
        }
    }
}
