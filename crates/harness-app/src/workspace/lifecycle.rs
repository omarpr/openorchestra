//! Ending a task: archive (worktree folder goes, branch stays) or delete
//! (folder and branch go).
//!
//! Both used to run on the render thread straight from the card menu: teardown
//! hooks (arbitrary user scripts), `git worktree remove --force` and, for
//! delete, `git branch -D`, behind nothing more than a second click on
//! "Confirm delete?". `--force` discards uncommitted changes, and `-D` discards
//! commits that never reached a remote — so a misclick could silently destroy
//! work, and a slow teardown script froze the window while it ran.
//!
//! Now:
//! 1. [`HarnessWorkspace::request_end`] measures what would be lost
//!    (uncommitted files, unpushed commits) off-thread.
//! 2. Delete always asks, naming those losses. Archive asks only when it would
//!    discard uncommitted changes; otherwise it just goes.
//! 3. [`HarnessWorkspace::start_end`] runs hooks and git off-thread, then
//!    updates the registry and task store on the UI thread.

use super::*;

use worktrees::{HookTrust, HooksError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndKind {
    Archive,
    Delete,
}

impl EndKind {
    pub(crate) fn verb(self) -> &'static str {
        match self {
            EndKind::Archive => "Archive",
            EndKind::Delete => "Delete",
        }
    }
    /// For the row's pill while it runs.
    pub(crate) fn doing_label(self) -> &'static str {
        match self {
            EndKind::Archive => "ARCHIVING",
            EndKind::Delete => "DELETING",
        }
    }
    pub(crate) fn doing(self) -> &'static str {
        match self {
            EndKind::Archive => "archiving",
            EndKind::Delete => "deleting",
        }
    }
}

/// One member worktree that is going away.
#[derive(Clone, Debug)]
pub(crate) struct EndingMember {
    pub worktree_id: String,
    pub repo: String,
    pub repo_root: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    pub task_slug: String,
    pub task_id: Option<String>,
    /// What "pushed" is measured against when the branch has no upstream.
    pub fallback_ref: String,
}

/// Everything needed to end one task (or lone worktree), captured up front so
/// the background half never touches the workspace.
#[derive(Clone, Debug)]
pub(crate) struct EndPlan {
    pub kind: EndKind,
    /// Task id, or the worktree id for a worktree outside any task.
    pub id: String,
    pub task_id: Option<String>,
    pub title: String,
    pub members: Vec<EndingMember>,
    pub home_worktree: Option<PathBuf>,
    pub task_slug: Option<String>,
    pub member_paths: Vec<(String, PathBuf)>,
}

/// What ending one member would throw away.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Loss {
    pub repo: String,
    pub branch: String,
    pub dirty: usize,
    /// Commits on the branch that are on no remote (only lost by delete).
    pub unpushed: usize,
}

/// A pending end, waiting for its loss check and then the user's answer.
#[derive(Clone, Debug)]
pub(crate) struct EndConfirm {
    pub plan: EndPlan,
    /// `None` while the check is still running; the dialog waits for it.
    pub losses: Option<Vec<Loss>>,
}

/// Whether `losses` means the user must be asked before `kind` proceeds.
pub(crate) fn needs_confirmation(kind: EndKind, losses: &[Loss]) -> bool {
    match kind {
        // Delete is never silent: even a clean branch is gone for good.
        EndKind::Delete => true,
        // Archive keeps the branch, so only uncommitted work is at stake.
        EndKind::Archive => losses.iter().any(|l| l.dirty > 0),
    }
}

/// Measure what ending each member would lose. Blocking: run off-thread.
fn measure_losses(members: &[EndingMember]) -> Vec<Loss> {
    let git = GitWorktrees::new(SystemRunner);
    members
        .iter()
        .filter(|m| m.path.exists())
        .map(|m| Loss {
            repo: m.repo.clone(),
            branch: m.branch.clone(),
            dirty: git
                .status(&m.path, None)
                .map(|s| s.dirty_files)
                .unwrap_or(0),
            unpushed: git.push_state(&m.path, &m.fallback_ref).unpushed,
        })
        .collect()
}

/// What the background half reports back.
struct EndOutcome {
    /// Members whose worktree (and, for delete, branch) is gone.
    done: Vec<String>,
    errors: Vec<String>,
}

/// The repo's hooks if it has teardown scripts to run, `None` if it has none,
/// or the message to show when they can't run.
///
/// Trust covers a repo's whole hooks file, so a repo with untrusted *setup*
/// scripts fails `load_trusted` even when its teardown list is empty. That
/// isn't worth a warning at delete time: nothing was skipped.
fn teardown_hooks(
    repo_root: &Path,
    data_dir: &Path,
    repo: &str,
) -> Result<Option<HooksConfig>, String> {
    match HooksConfig::load_trusted(repo_root, data_dir, repo) {
        Ok(hooks) if hooks.teardown.is_empty() => Ok(None),
        Ok(hooks) => Ok(Some(hooks)),
        // Never approved: skip their scripts, not the removal.
        Err(HooksError::Untrusted { .. }) => {
            let has_teardown = HooksConfig::load(repo_root, data_dir, repo)
                .map(|c| !c.teardown.is_empty())
                .unwrap_or(true);
            if has_teardown {
                Err(format!("{repo}: teardown skipped (hooks not trusted)"))
            } else {
                Ok(None)
            }
        }
        Err(e) => Err(format!("{repo} hooks: {e}")),
    }
}

/// Teardown hooks, then git. Blocking: run off-thread.
fn run_end(plan: &EndPlan, data_dir: &Path) -> EndOutcome {
    let git = GitWorktrees::new(SystemRunner);
    let mut errors = Vec::new();

    if let Some(slug) = &plan.task_slug {
        let task_dir = data_dir.join("tasks").join(slug);
        if let Err(e) = TaskHooks::load(data_dir, slug).run_teardown(
            git.runner(),
            &task_dir,
            &plan.member_paths,
        ) {
            errors.push(format!("task teardown: {e}"));
        }
    }
    for m in &plan.members {
        let env = HookEnv {
            repo_root: m.repo_root.clone(),
            worktree: m.path.clone(),
            branch: m.branch.clone(),
            task_slug: m.task_slug.clone(),
            task_id: m.task_id.clone(),
            home_worktree: plan.home_worktree.clone(),
        };
        match teardown_hooks(&m.repo_root, data_dir, &m.repo) {
            Ok(Some(hooks)) => {
                if let Err(e) = hooks.run_teardown(git.runner(), &env) {
                    errors.push(format!("{} teardown: {e}", m.repo));
                }
            }
            Ok(None) => {}
            Err(e) => errors.push(e),
        }
    }

    let mut done = Vec::new();
    for m in &plan.members {
        // Already gone (removed by hand, or a previous attempt got this far):
        // nothing to remove, and the registry should still let go of it.
        let removed = if m.path.exists() {
            git.remove_worktree(&m.repo_root, &m.path)
        } else {
            Ok(())
        };
        if let Err(e) = removed {
            errors.push(format!("{}: {e}", m.repo));
            continue;
        }
        if plan.kind == EndKind::Delete
            && let Err(e) = git.delete_branch(&m.repo_root, &m.branch)
        {
            errors.push(format!("{}: branch {}: {e}", m.repo, m.branch));
            continue;
        }
        done.push(m.worktree_id.clone());
    }
    EndOutcome { done, errors }
}

impl HarnessWorkspace {
    /// The archive/delete running for this row, if any: keyed by task id, or
    /// by worktree id for a worktree outside any task.
    pub(crate) fn ending_for(&self, task_id: Option<&str>, worktree_id: &str) -> Option<EndKind> {
        task_id
            .and_then(|t| self.ending.get(t))
            .or_else(|| self.ending.get(worktree_id))
            .copied()
    }

    /// Build the plan for ending `id` (a task id, or a worktree id).
    fn end_plan(&self, id: &str, kind: EndKind) -> Option<EndPlan> {
        let task_id = self
            .manager
            .registry
            .get(id)
            .and_then(|e| e.task_id.clone())
            .or_else(|| self.task_store.get(id).map(|t| t.id.clone()));
        let fallback = |root: &Path| {
            self.data
                .repos
                .iter()
                .find(|r| r.repo_root == root)
                .map(|r| r.compare_ref.clone())
                .unwrap_or_else(|| "HEAD".into())
        };
        let member = |wid: &str| -> Option<EndingMember> {
            let e = self.manager.registry.get(wid)?;
            Some(EndingMember {
                worktree_id: e.id.clone(),
                repo: e.repo.clone(),
                repo_root: e.repo_root.clone(),
                path: e.path.clone(),
                branch: e.branch.clone(),
                task_slug: e.task_slug.clone(),
                task_id: e.task_id.clone(),
                fallback_ref: fallback(&e.repo_root),
            })
        };
        if let Some(tid) = task_id {
            let task = self.task_store.get(&tid)?;
            Some(EndPlan {
                kind,
                id: tid.clone(),
                task_id: Some(tid.clone()),
                title: task.name.clone(),
                // Already-archived members have nothing left on disk to end.
                members: task
                    .members
                    .iter()
                    .filter(|m| {
                        self.manager
                            .registry
                            .get(&m.worktree_id)
                            .is_some_and(|e| !e.archived || kind == EndKind::Delete)
                    })
                    .filter_map(|m| member(&m.worktree_id))
                    .collect(),
                home_worktree: task.home_path().map(Path::to_path_buf),
                task_slug: Some(task.slug.clone()),
                member_paths: task.member_paths(),
            })
        } else {
            let m = member(id)?;
            Some(EndPlan {
                kind,
                id: id.to_string(),
                task_id: None,
                title: self
                    .manager
                    .registry
                    .get(id)
                    .map(|e| e.name.clone())
                    .unwrap_or_default(),
                members: vec![m],
                home_worktree: None,
                task_slug: None,
                member_paths: Vec::new(),
            })
        }
    }

    /// The card menu's Archive / Delete: check what would be lost, then ask
    /// (or, for a clean archive, just go).
    pub(crate) fn request_end(
        &mut self,
        id: String,
        kind: EndKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        if self.ending.contains_key(&id) {
            return;
        }
        let Some(plan) = self.end_plan(&id, kind) else {
            self.set_note(format!("{}: nothing to {}", id, kind.verb().to_lowercase()));
            cx.notify();
            return;
        };
        let members = plan.members.clone();
        self.end_confirm = Some(EndConfirm { plan, losses: None });
        cx.notify();
        cx.spawn_in(window, async move |view, cx| {
            let losses = cx
                .background_spawn(async move { measure_losses(&members) })
                .await;
            let _ = view.update_in(cx, |ws, window, cx| {
                // Cancelled, or replaced by another request, meanwhile.
                let Some(pending) = ws
                    .end_confirm
                    .as_mut()
                    .filter(|c| c.plan.id == id && c.plan.kind == kind)
                else {
                    return;
                };
                if needs_confirmation(kind, &losses) {
                    pending.losses = Some(losses);
                    cx.notify();
                } else {
                    let plan = pending.plan.clone();
                    ws.end_confirm = None;
                    ws.start_end(plan, window, cx);
                }
            });
        })
        .detach();
    }

    pub(crate) fn cancel_end(&mut self, cx: &mut Context<Self>) {
        self.end_confirm = None;
        cx.notify();
    }

    /// Tear down the UI side now, run hooks and git off-thread, then let go
    /// of the registry entries that actually went.
    pub(crate) fn start_end(&mut self, plan: EndPlan, window: &mut Window, cx: &mut Context<Self>) {
        self.end_confirm = None;
        self.ending.insert(plan.id.clone(), plan.kind);

        if let Some(tid) = &plan.task_id {
            self.shutdown_task_broker(tid);
            for m in &plan.members {
                let pane_key = a2a_broker::task_member_pane_key(tid, &m.worktree_id);
                self.teardown_sessions(&pane_key);
                self.panes.remove(&pane_key);
                // Queued A2A lines for a pane that is gone would otherwise be
                // injected into a re-created task's agent of the same key.
                self.a2a_pending_inject.remove(&pane_key);
                // Legacy keys from pre multi-context builds.
                self.teardown_sessions(&m.worktree_id);
                self.panes.remove(&m.worktree_id);
            }
            let coordinator = a2a_broker::coordinator_pane_key(tid);
            self.teardown_sessions(&coordinator);
            self.panes.remove(&coordinator);
            self.a2a_pending_inject.remove(&coordinator);
            // The coordinator's status lives in the task folder, which is
            // kept; a re-created same-slug task must not inherit its pill.
            if let Some(slug) = self.task_store.get(tid).map(|t| t.slug.clone()) {
                let dir = clear_coordinator_status(&self.data_dir.join("tasks"), &slug);
                self.conductor.forget_root(&dir);
                self.agent_status.remove(&dir);
            }
            let legacy = format!("task:{tid}");
            self.teardown_sessions(&legacy);
            self.panes.remove(&legacy);
        } else {
            self.teardown_sessions(&plan.id);
            self.panes.remove(&plan.id);
        }

        // Stay in the project you were working in rather than jumping to
        // whichever project happens to be listed first.
        if let Some(repo) = plan
            .members
            .first()
            .and_then(|m| self.data.repos.iter().find(|r| r.repo_root == m.repo_root))
            .or_else(|| self.data.repos.first())
            .map(|r| r.name.clone())
        {
            self.select(Selection::Main(repo), window, cx);
        }
        self.set_note(format!("{} ‘{}’…", plan.kind.doing(), plan.title));
        cx.notify();

        let data_dir = self.data_dir.clone();
        cx.spawn_in(window, async move |view, cx| {
            let job = plan.clone();
            let outcome = cx
                .background_spawn(async move { run_end(&job, &data_dir) })
                .await;
            let _ = view.update_in(cx, |ws, window, cx| {
                ws.finish_end(plan, outcome, window, cx)
            });
        })
        .detach();
    }

    fn finish_end(
        &mut self,
        plan: EndPlan,
        outcome: EndOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ending.remove(&plan.id);
        let mut errors = outcome.errors;
        for wid in &outcome.done {
            let r = match plan.kind {
                EndKind::Archive => self
                    .manager
                    .registry
                    .mark_archived(wid)
                    .map_err(|e| e.to_string()),
                EndKind::Delete => {
                    self.manager.registry.remove(wid);
                    Ok(())
                }
            };
            if let Err(e) = r {
                errors.push(e);
            }
        }
        if let Err(e) = self.manager.registry.save() {
            errors.push(format!("registry: {e}"));
        }
        // The task only ends once every member did; a partial failure leaves
        // it in place so the rest can be retried.
        let all_done = outcome.done.len() == plan.members.len();
        if all_done && let Some(tid) = &plan.task_id {
            let r = match plan.kind {
                EndKind::Archive => self
                    .task_store
                    .mark_archived(tid)
                    .map_err(|e| e.to_string()),
                EndKind::Delete => {
                    self.task_store.remove(tid);
                    Ok(())
                }
            };
            if let Err(e) = r.and_then(|_| self.task_store.save().map_err(|e| e.to_string())) {
                errors.push(e);
            }
        }
        let past = match plan.kind {
            EndKind::Archive => "archived",
            EndKind::Delete => "deleted",
        };
        self.set_note(if errors.is_empty() {
            format!("{past} ‘{}’", plan.title)
        } else if all_done {
            format!("{past} ‘{}’ ({})", plan.title, errors.join("; "))
        } else {
            format!(
                "{} ‘{}’ failed: {}",
                plan.kind.verb().to_lowercase(),
                plan.title,
                errors.join("; ")
            )
        });
        self.refresh(window, cx);
    }

    /// Approve a repo's committed hooks as they are now, then retry setup.
    pub(crate) fn trust_hooks_and_run(
        &mut self,
        roots: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hook_trust = None;
        for root in &roots {
            if let Err(e) = HookTrust::trust(&self.data_dir, root) {
                self.set_note(format!("could not trust hooks in {}: {e}", root.display()));
                cx.notify();
                return;
            }
        }
        self.run_visible_setup_hooks(window, cx);
    }
}

/// Remove a task coordinator's `.harness/status.json` (never the task
/// folder itself) and return the coordinator dir it belonged to.
fn clear_coordinator_status(tasks_root: &std::path::Path, slug: &str) -> PathBuf {
    let dir = worktrees::task_coordinator_dir(tasks_root, slug);
    let status = dir.join(".harness").join("status.json");
    if let Err(e) = std::fs::remove_file(&status)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("could not clear {}: {e}", status.display());
    }
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ending_a_task_clears_the_coordinator_status_but_keeps_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tasks");
        let dir = worktrees::task_coordinator_dir(&root, "ship-it");
        std::fs::create_dir_all(dir.join(".harness")).unwrap();
        std::fs::write(dir.join(".harness/status.json"), "{}").unwrap();
        std::fs::write(dir.join("brief.md"), "keep").unwrap();
        assert_eq!(clear_coordinator_status(&root, "ship-it"), dir);
        assert!(!dir.join(".harness/status.json").exists());
        assert!(dir.join("brief.md").exists(), "task folder untouched");
        // Already gone is fine.
        clear_coordinator_status(&root, "ship-it");
    }

    fn loss(dirty: usize, unpushed: usize) -> Loss {
        Loss {
            repo: "app".into(),
            branch: "feat/x".into(),
            dirty,
            unpushed,
        }
    }

    #[test]
    fn delete_always_asks() {
        assert!(needs_confirmation(EndKind::Delete, &[]));
        assert!(needs_confirmation(EndKind::Delete, &[loss(0, 0)]));
    }

    #[test]
    fn archive_asks_only_when_it_would_drop_uncommitted_work() {
        assert!(!needs_confirmation(EndKind::Archive, &[loss(0, 0)]));
        // Unpushed commits stay on the branch, which archive keeps.
        assert!(!needs_confirmation(EndKind::Archive, &[loss(0, 5)]));
        assert!(needs_confirmation(
            EndKind::Archive,
            &[loss(0, 0), loss(2, 0)]
        ));
    }

    /// A repo whose committed hooks list `setup` and `teardown` scripts, with
    /// nothing approved yet.
    fn untrusted_repo(setup: bool, teardown: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let data = tmp.path().join("data");
        let hooks = repo.join(".openorchestra");
        std::fs::create_dir_all(hooks.join("scripts")).unwrap();
        let list = |on: bool, name: &str| {
            if on {
                format!("[\"scripts/{name}\"]")
            } else {
                "[]".into()
            }
        };
        std::fs::write(
            hooks.join("hooks.toml"),
            format!(
                "[setup]\nscripts = {}\n\n[teardown]\nscripts = {}\n",
                list(setup, "setup.sh"),
                list(teardown, "teardown.sh")
            ),
        )
        .unwrap();
        std::fs::write(hooks.join("scripts/setup.sh"), "echo up\n").unwrap();
        std::fs::write(hooks.join("scripts/teardown.sh"), "echo down\n").unwrap();
        (tmp, repo, data)
    }

    /// Untrusted setup-only hooks have no teardown to skip, so ending a
    /// worktree must not warn about one.
    #[test]
    fn untrusted_setup_only_hooks_skip_nothing_at_teardown() {
        let (_tmp, repo, data) = untrusted_repo(true, false);
        assert!(matches!(teardown_hooks(&repo, &data, "repo"), Ok(None)));
    }

    #[test]
    fn untrusted_teardown_scripts_are_reported_as_skipped() {
        let (_tmp, repo, data) = untrusted_repo(true, true);
        let err = teardown_hooks(&repo, &data, "repo").unwrap_err();
        assert_eq!(err, "repo: teardown skipped (hooks not trusted)");
    }

    #[test]
    fn trusted_teardown_scripts_load() {
        let (_tmp, repo, data) = untrusted_repo(false, true);
        HookTrust::trust(&data, &repo).unwrap();
        let cfg = teardown_hooks(&repo, &data, "repo")
            .unwrap()
            .expect("teardown to run");
        assert_eq!(cfg.teardown.len(), 1);
    }

    /// The losses a real worktree reports: uncommitted files and commits that
    /// exist only on its branch.
    #[test]
    fn losses_count_dirty_files_and_commits_on_no_remote() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(repo)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        git(&["add", "a"]);
        git(&["commit", "-qm", "base"]);
        git(&["switch", "-qc", "feat/x"]);
        std::fs::write(repo.join("b"), "2").unwrap();
        git(&["add", "b"]);
        git(&["commit", "-qm", "work"]);
        std::fs::write(repo.join("c"), "3").unwrap();

        let m = EndingMember {
            worktree_id: "app/x".into(),
            repo: "app".into(),
            repo_root: repo.to_path_buf(),
            path: repo.to_path_buf(),
            branch: "feat/x".into(),
            task_slug: "x".into(),
            task_id: None,
            fallback_ref: "main".into(),
        };
        let l = measure_losses(&[m]);
        assert_eq!(
            l,
            vec![Loss {
                repo: "app".into(),
                branch: "feat/x".into(),
                dirty: 1,
                unpushed: 1
            }]
        );
    }
}
