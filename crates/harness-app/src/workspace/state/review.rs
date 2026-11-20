//! The diff/review surface: computing the diff, row selection, review
//! comments, and the markdown preview toggle.

use super::*;
use crate::review_overview::{ReviewRepo, ReviewRequest};

impl OverviewTab {
    pub(super) fn set_report(
        &mut self,
        saved: Arc<StoredReviewOverview>,
        cx: &mut Context<HarnessWorkspace>,
    ) {
        self.preferences = saved.preferences.clone();
        let parsed = crate::review_overview::reading_order(
            &saved.report.markdown,
            &saved.report.files,
            saved.report.scope.len() > 1,
        );
        let text = parsed
            .as_ref()
            .map(|order| order.markdown.clone())
            .unwrap_or_else(|| saved.report.markdown.clone());
        self.reading_steps = parsed.map(|order| order.steps).unwrap_or_default();
        self.last_reading_step = None;
        let markdown = cx.new(|cx| markdown::Markdown::new(text.into(), None, None, cx));
        self.last_report = Some(Arc::clone(&saved));
        self.status = OverviewStatus::Ready { markdown, saved };
    }
}

impl HarnessWorkspace {
    pub(crate) fn ensure_review_model_catalog(&mut self, cx: &mut Context<Self>) {
        let cli = self.prefs.review_overview.cli;
        if self.review_model_catalogs.contains_key(&cli) {
            return;
        }
        // Opening Diff settings must not spawn `codex debug models`. That
        // command can reach the network when Codex's own cache is cold.
        // Reload models is the path that asks the CLI, and it stores the answer.
        let Some(models) = crate::review_overview::models::read_cache(&self.state_dir, cli) else {
            return;
        };
        self.review_model_catalogs
            .insert(cli, ModelCatalog::Ready(models));
        cx.notify();
    }

    pub(crate) fn load_review_model_catalog(&mut self, cli: ReviewCli, cx: &mut Context<Self>) {
        if matches!(
            self.review_model_catalogs.get(&cli),
            Some(ModelCatalog::Loading)
        ) {
            return;
        }
        let cwd = self
            .resolve()
            .map(|r| r.path)
            .unwrap_or_else(|| self.data_dir.clone());
        let state_dir = self.state_dir.clone();
        self.review_model_catalogs
            .insert(cli, ModelCatalog::Loading);
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    let result = crate::review_overview::models::discover(cli, &cwd);
                    if let Ok(models) = &result {
                        let _ =
                            crate::review_overview::models::write_cache(&state_dir, cli, models);
                    }
                    result
                })
                .await;
            let _ = view.update(cx, |ws, cx| {
                ws.review_model_catalogs.insert(
                    cli,
                    match result {
                        Ok(models) => ModelCatalog::Ready(models),
                        Err(error) => ModelCatalog::Failed(error),
                    },
                );
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn run_review_overview(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let repos = if let Some(task) = r.task_id.as_deref().and_then(|id| self.task_store.get(id))
        {
            task.members
                .iter()
                .map(|member| ReviewRepo {
                    name: member.repo.clone(),
                    path: member.path.clone(),
                    compare_ref: self
                        .data
                        .repos
                        .iter()
                        .find(|repo| repo.name == member.repo)
                        .map(|repo| repo.compare_ref.clone())
                        .unwrap_or_else(|| "HEAD".into()),
                })
                .collect()
        } else {
            let project = self.data.repos.iter().find(|repo| {
                repo.repo_root == r.path || repo.worktrees.iter().any(|entry| entry.path == r.path)
            });
            vec![ReviewRepo {
                name: project
                    .map(|repo| repo.name.clone())
                    .unwrap_or_else(|| r.branch.clone()),
                path: r.path.clone(),
                compare_ref: r.compare_ref,
            }]
        };
        let request = ReviewRequest {
            cwd: r.path,
            repos,
            preferences: self.prefs.review_overview.clone(),
        };
        let Some(pane) = self.panes.get_mut(&r.key) else {
            return;
        };
        let id = pane.active;
        let Some(overview) = review_overview_tab(pane, id) else {
            return;
        };
        if matches!(overview.status, OverviewStatus::Running(_)) {
            return;
        }

        let run = ReviewRun::new();
        let worker = run.worker_flag();
        overview.preferences = request.preferences.clone();
        overview.status = OverviewStatus::Running(run);
        cx.notify();
        cx.spawn(async move |view, cx| {
            let cancelled = Arc::clone(&worker);
            let result = cx
                .background_spawn(async move { request.run(cancelled) })
                .await;
            let _ = view.update(cx, |ws, cx| {
                let Some(pane) = ws.panes.get_mut(&r.key) else {
                    return;
                };
                let Some(overview) = review_overview_tab(pane, id) else {
                    return;
                };
                if !matches!(&overview.status, OverviewStatus::Running(run) if run.owns(&worker)) {
                    return;
                }
                match result {
                    Ok(report) => {
                        let saved = Arc::new(StoredReviewOverview {
                            preferences: overview.preferences.clone(),
                            report,
                        });
                        overview.set_report(saved, cx);
                        ws.save_panes(cx);
                    }
                    Err(error) => overview.status = OverviewStatus::Failed(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn cancel_review_overview(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let Some(pane) = self.panes.get_mut(&r.key) else {
            return;
        };
        let Some(overview) = review_overview_tab(pane, pane.active) else {
            return;
        };
        if matches!(overview.status, OverviewStatus::Running(_)) {
            overview.status = OverviewStatus::Cancelled;
            cx.notify();
        }
    }

    /// Open a reading-order file the same way a path in the writeup does, and
    /// leave that step marked so the path shows where the review is.
    pub(crate) fn open_reading_order_file(
        &mut self,
        step: usize,
        root: PathBuf,
        relative: String,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(resolved) = self.resolve()
            && let Some(pane) = self.panes.get_mut(&resolved.key)
            && let Some(overview) = review_overview_tab(pane, pane.active)
        {
            overview.last_reading_step = Some(step);
        }
        if !root.join(&relative).is_file() {
            self.set_note(format!("{relative} is no longer in the working tree"));
            cx.notify();
            return;
        }
        self.exit_hooks_edit(window, cx);
        self.open_workspace_file(&root, &relative, line, window, cx);
    }

    pub(crate) fn chat_about_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let Some(tab) = self.panes.get(&r.key).and_then(PaneSet::active_tab) else {
            return;
        };
        let TabContent::ReviewOverview(overview) = &tab.content else {
            return;
        };
        let OverviewStatus::Ready { saved, .. } = &overview.status else {
            return;
        };
        let report = &saved.report;
        if !report.generated || saved.preferences.cli.agent_kind().is_none() {
            return;
        }
        let followup = ReviewFollowup::new(
            &saved.preferences,
            &report.markdown,
            &report.scope,
            &report.limitations,
        );
        self.open_review_chat(followup, window, cx);
    }

    /// Recompute the merge-base diff **off the UI thread** (M4).
    ///
    /// `compute_diff` shells out to `git diff`, which on a large repo takes
    /// long enough to visibly stall the window if run inline on selection.
    /// Results are tagged with the worktree they were computed for and the
    /// generation that requested them, so a slow diff for a worktree the user
    /// has already navigated away from is dropped rather than painted over the
    /// current one.
    /// Copy the selected diff rows to the clipboard.
    ///
    /// Copies the *code*, not the diff decoration: no gutter numbers and no
    /// +/- markers, so a pasted selection compiles.
    pub(crate) fn copy_diff_selection(&mut self, cx: &mut Context<Self>) {
        let Some((file, from, to)) = self.diff_selection.clone() else {
            return;
        };
        // Reads the rows the selection was made against rather than re-parsing:
        // you cannot have selected lines in a body that was never drawn.
        let Some(rows) = self.cached_rows(&file) else {
            return;
        };
        let text: Vec<String> = rows
            .iter()
            .enumerate()
            .filter(|(i, _)| *i >= from && *i <= to)
            .filter(|(_, r)| r.number.is_some())
            .map(|(_, r)| r.text.clone())
            .collect();
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.join("\n")));
        self.set_note(format!("copied {} line(s)", text.len()));
        cx.notify();
    }

    /// Begin, extend, or clear a row selection within one file's diff.
    pub(crate) fn select_diff_row(
        &mut self,
        file: &str,
        row: usize,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        self.diff_selection = match (self.diff_selection.take(), extend) {
            // Shift-click extends, but only within the same file — a range
            // spanning two bodies would have no meaning.
            (Some((f, from, _)), true) if f == file => Some((f, from.min(row), from.max(row))),
            _ => Some((file.to_string(), row, row)),
        };
        cx.notify();
    }

    pub(crate) fn extend_diff_selection(&mut self, file: &str, row: usize, cx: &mut Context<Self>) {
        if let Some((f, from, _)) = self.diff_selection.clone()
            && f == file
        {
            self.diff_selection = Some((f, from.min(row), from.max(row)));
            cx.notify();
        }
    }

    /// Open the comment editor for a diff line, seeded with any existing note.
    pub(crate) fn begin_comment(
        &mut self,
        file: String,
        line: u32,
        at: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Land on Review so the comment popover isn't drawn over Files/Git.
        if self.prefs.diff_location == DiffLocation::Dock {
            self.prefs.tool_tab = ToolTab::Review;
            self.prefs.diff_open = true;
            self.save_prefs(cx);
        }
        let existing = self
            .resolve()
            .and_then(|r| self.review.get(&r.key, &file, line))
            .map(|c| c.body.clone())
            .unwrap_or_default();
        let editor = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_placeholder_text("What should the agent change here?", window, cx);
            if !existing.is_empty() {
                e.set_text(existing.as_str(), window, cx);
            }
            e
        });
        window.focus(&editor.focus_handle(cx), cx);
        self.comment_draft = Some(CommentDraft {
            file,
            line,
            editor,
            at,
        });
        cx.notify();
    }

    /// Enter in the comment popover (routed from `menu::Confirm`).
    pub(crate) fn confirm_comment(
        &mut self,
        _: &menu::Confirm,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_comment(cx);
    }

    /// Escape in the comment popover (routed from `menu::Cancel`): abandon it.
    pub(crate) fn cancel_comment(
        &mut self,
        _: &menu::Cancel,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.comment_draft = None;
        cx.notify();
    }

    pub(crate) fn commit_comment(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.comment_draft.take() else {
            return;
        };
        let body = draft.editor.read(cx).text(cx);
        if let Some(r) = self.resolve() {
            self.review.set(&r.key, &draft.file, draft.line, &body);
            self.review.save(&self.review_path);
        }
        cx.notify();
    }

    /// Hand the review to the agent: copy it formatted, addressed by file and
    /// line, ready to paste into the agent's terminal or chat.
    pub(crate) fn copy_review(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let review = self.review.as_review(&r.key);
        if review.is_empty() {
            self.set_note("no review comments yet".to_string());
        } else {
            let n = self.review.count(&r.key);
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(review));
            self.set_note(format!("copied {n} comment(s) — paste into the agent"));
        }
        cx.notify();
    }

    /// Write the review to a markdown file next to the harness state.
    ///
    /// Not into the repo: a review is the user's working notes, and dropping an
    /// untracked file into their project is the thing the git-exclude work went
    /// out of its way to avoid.
    pub(crate) fn save_review_markdown(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let md = self.review.as_markdown(&r.key, &r.branch);
        if md.is_empty() {
            self.set_note("no comments to save".to_string());
            cx.notify();
            return;
        }
        let file = crate::diff_review::ReviewNotes::markdown_path_in(&self.state_dir, &r.key);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&file, md) {
            Ok(()) => self.set_note(format!("saved {}", file.display())),
            Err(e) => self.set_error_note(format!("could not save review: {e}")),
        }
        cx.notify();
    }

    /// Discard every comment for this worktree, behind a two-click confirm.
    pub(crate) fn clear_review(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        if !self.clearing_review {
            self.clearing_review = true;
            self.set_note("click again to discard all comments".to_string());
        } else {
            self.review.clear(&r.key);
            self.review.save(&self.review_path);
            self.clearing_review = false;
            self.set_note("comments discarded".to_string());
        }
        cx.notify();
    }

    /// Toggle between the raw diff and a rendered preview of the selected
    /// markdown file.
    pub(crate) fn toggle_md_preview(&mut self, file: &str, cx: &mut Context<Self>) {
        if self.md_preview.as_ref().is_some_and(|(f, _)| f == file) {
            self.md_preview = None;
            cx.notify();
            return;
        }
        let Some(r) = self.resolve() else { return };
        let abs = if let Some(uf) = self
            .unified_diff
            .files
            .iter()
            .find(|f| format!("{}/{}", f.repo, f.file.path) == file)
        {
            let Some(member) = self.manager.registry.get(&uf.member_id) else {
                self.set_note(format!("worktree {} not found", uf.member_id));
                cx.notify();
                return;
            };
            member.path.join(&uf.file.path)
        } else {
            r.editor_path.join(file)
        };
        self.md_preview = Some((
            file.to_string(),
            cx.new(|cx| crate::markdown_preview::MarkdownPreview::new(abs, cx)),
        ));
        cx.notify();
    }

    /// Recompute the diff for a *new* selection: this resets the review view
    /// (collapses files, drops the markdown preview) because the user has
    /// switched to a different set of changes, and retargets the live file
    /// watcher at the newly-selected root.
    pub(crate) fn recompute_diff(&mut self, cx: &mut Context<Self>) {
        self.expanded.clear();
        self.diff_focus = None;
        self.md_preview = None;
        if let Some(r) = self.resolve() {
            // Keep the live watch pointed at whatever the diff panel shows: a
            // task's diff spans all its members (never the task folder).
            let task = r.task_id.as_deref().and_then(|id| self.task_store.get(id));
            let roots = crate::workspace::coordinator::diff_watch_roots(task, &r.path);
            self.diff_watch.watch_all(&roots);
        }
        // A push offer belongs to one branch. Carrying an armed control — or a
        // count — across a selection change would let a click aimed at one
        // branch push another.
        self.push_armed = false;
        self.push_state = None;
        self.refresh_push_state(cx);
        self.spawn_diff_compute(cx);
    }

    /// Recompute the diff for the *same* selection, in response to a live file
    /// change. Deliberately does **not** clear `expanded`/`md_preview`: the user
    /// is reviewing the same worktree, and resetting their expand/preview state
    /// on every agent edit would make review impossible.
    pub(crate) fn refresh_diff_live(&mut self, cx: &mut Context<Self>) {
        self.spawn_diff_compute(cx);
    }

    /// Spawn the off-thread git diff, discarding a result whose selection the
    /// user has since left (the `diff_gen` staleness guard).
    pub(crate) fn spawn_diff_compute(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else {
            self.diff = worktrees::Diff::default();
            self.unified_diff = UnifiedDiff::default();
            self.row_cache.borrow_mut().clear();
            // Bumped here too, not only on the path that spawns: a per-file
            // body fetch may be in flight, and clearing the cache without
            // invalidating it would let that fetch land afterwards and
            // repopulate rows for a selection that no longer exists.
            self.diff_gen = self.diff_gen.wrapping_add(1);
            self.diff_pending = false;
            cx.notify();
            return;
        };

        self.diff_gen = self.diff_gen.wrapping_add(1);
        let generation = self.diff_gen;
        self.diff_pending = true;

        // Copied out before the closures below: they run on a background
        // thread and cannot borrow `self`.
        let diff_base = self.prefs.diff_base;
        if let Some(task_id) = r.task_id.clone() {
            let members: Vec<(String, String, PathBuf, String)> = self
                .task_store
                .get(&task_id)
                .map(|t| {
                    t.members
                        .iter()
                        .map(|m| {
                            let compare = self
                                .data
                                .repos
                                .iter()
                                .find(|r| r.name == m.repo)
                                .map(|r| r.compare_ref.clone())
                                .unwrap_or_else(|| "HEAD".into());
                            // Per member, so a multi-repo task honours the
                            // setting in every one of its repos.
                            let compare = diff_base_ref(diff_base, &compare).to_string();
                            (
                                m.worktree_id.clone(),
                                m.repo.clone(),
                                m.path.clone(),
                                compare,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            cx.spawn(async move |view, cx| {
                let computed = cx
                    .background_spawn(async move {
                        UnifiedDiff::from_members(&GitWorktrees::new(SystemRunner), &members)
                    })
                    .await;
                let _ = view.update(cx, |ws, cx| {
                    if ws.diff_gen != generation {
                        return;
                    }
                    // Flatten into Diff for existing panel machinery. Keys stay
                    // `repo/path` so multi-repo files remain distinct; the panel
                    // groups by repo and shows the relative path under each.
                    let mut files = Vec::new();
                    for f in &computed.files {
                        files.push(worktrees::DiffFile {
                            path: format!("{}/{}", f.repo, f.file.path),
                            change: f.file.change,
                            adds: f.file.adds,
                            dels: f.file.dels,
                            untracked: f.file.untracked,
                            renamed_from: f.file.renamed_from.clone(),
                        });
                    }
                    // `base` stays empty here: a unified diff spans members
                    // with different merge-bases, so there is no single one.
                    // Per-file fetches read the base off `unified_diff`, which
                    // records each member's own.
                    ws.diff = worktrees::Diff {
                        base: String::new(),
                        files,
                        untracked_elided: computed.untracked_elided,
                    };
                    ws.unified_diff = computed;
                    ws.row_cache.borrow_mut().clear();
                    ws.diff_pending = false;
                    cx.notify();
                });
            })
            .detach();
            return;
        }

        let (path, compare_ref) = (
            r.path.clone(),
            diff_base_ref(diff_base, &r.compare_ref).to_string(),
        );
        cx.spawn(async move |view, cx| {
            let computed = cx
                .background_spawn(async move {
                    compute_diff(&GitWorktrees::new(SystemRunner), &path, &compare_ref)
                })
                .await;
            let _ = view.update(cx, |ws, cx| {
                if ws.diff_gen != generation {
                    return;
                }
                ws.diff = computed;
                ws.unified_diff = UnifiedDiff::default();
                // The parsed rows describe the diff that just went away.
                ws.row_cache.borrow_mut().clear();
                ws.diff_pending = false;
                cx.notify();
            });
        })
        .detach();
    }
}

fn review_overview_tab(pane: &mut PaneSet, id: u64) -> Option<&mut OverviewTab> {
    let tab = pane.tabs.iter_mut().find(|tab| tab.id == id)?;
    match &mut tab.content {
        TabContent::ReviewOverview(overview) => Some(overview),
        _ => None,
    }
}
