//! Live git numbers off the render thread: card badges, push state and the
//! push action, plus the requirements (preflight) re-check.

use super::*;

impl HarnessWorkspace {
    /// Recompute every card's ahead/behind/dirty off the UI thread and merge the
    /// answers back in.
    ///
    /// Separate from `refresh()`, which rebuilds the whole view model — registry
    /// included — with synchronous git calls on the render thread. That is fine
    /// for a mutation the user just asked for, and unacceptable on a watcher
    /// signal: `git status` on a large tree takes long enough to drop frames,
    /// and a build can fire the watcher repeatedly. This path touches only the
    /// numbers on the badges, so the cards, the selection and the registry are
    /// left exactly as they are.
    pub(crate) fn refresh_status_live(&mut self, cx: &mut Context<Self>) {
        if self.status_pending {
            self.status_rerun = true;
            return;
        }
        self.status_rerun = false;
        // Every card, main rows included — each with the compare ref its own
        // repo resolved, so a worktree is never measured against another repo's
        // upstream.
        let mut targets: Vec<(PathBuf, String)> = Vec::new();
        for repo in &self.data.repos {
            targets.push((repo.repo_root.clone(), repo.compare_ref.clone()));
            for wt in &repo.worktrees {
                targets.push((wt.path.clone(), repo.compare_ref.clone()));
            }
        }
        if targets.is_empty() {
            return;
        }
        self.status_pending = true;

        cx.spawn(async move |view, cx| {
            let computed = cx
                .background_spawn(async move {
                    let git = GitWorktrees::new(SystemRunner);
                    targets
                        .into_iter()
                        // A failed probe is skipped, not zeroed: `git status`
                        // racing a commit's `index.lock` fails transiently, and
                        // `unwrap_or_default` turned that into a clean `~0` card
                        // until the next tick.
                        .filter_map(|(path, compare_ref)| {
                            let s = git.status(&path, Some(&compare_ref)).ok()?;
                            // The branch is re-read too. It used to come only
                            // from `refresh()`, i.e. structural changes, so a
                            // `git switch` in a worktree — or an agent renaming
                            // its branch — left the old name on the card for
                            // the rest of the session.
                            let branch = git.current_branch(&path).ok();
                            Some(LiveStatus {
                                path,
                                branch,
                                ahead: s.ahead,
                                behind: s.behind,
                                dirty: s.dirty_files,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            view.update(cx, |ws, cx| {
                ws.status_pending = false;
                // Only re-render when a number actually moved. The backstop tick
                // runs forever on an idle app, and an unconditional notify would
                // repaint the window every few seconds for nothing.
                if apply_live_status(&mut ws.data, computed) {
                    ws.sync_forge_branches();
                    ws.remote_refresh(cx);
                    cx.notify();
                }
                if ws.status_rerun {
                    ws.refresh_status_live(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-probe the tool requirements, off the UI thread.
    ///
    /// Off-thread because probing spawns up to nine processes to ask each tool
    /// its version, which is a visible stall on the render thread — and this
    /// runs from a button the user presses right after installing something.
    pub(crate) fn rerun_preflight(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            let checks = cx
                .background_spawn(async move {
                    crate::preflight::probe(crate::preflight::version_on_path)
                })
                .await;
            view.update(cx, |ws, cx| {
                // Only the result changes here. Closing on a clean re-check
                // would snatch the screen away at the exact moment it finally
                // says everything passes — the one moment worth reading. It
                // closes when the user says so, via Continue.
                ws.preflight = checks;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Close the requirements screen and remember that it has been seen, so a
    /// healthy machine is never shown it again unprompted.
    pub(crate) fn dismiss_preflight(&mut self, cx: &mut Context<Self>) {
        if crate::preflight::blocked(&self.preflight) {
            return;
        }
        self.preflight_open = false;
        if !self.prefs.seen_preflight {
            self.prefs.seen_preflight = true;
            self.save_prefs(cx);
        }
        cx.notify();
    }

    /// Probe the selected root's standing against its own tracking branch, off
    /// the UI thread.
    ///
    /// Separate from `refresh_status_live` rather than folded into it, for two
    /// reasons: it asks about one worktree (the one whose diff is on screen)
    /// where that asks about every card, and it must run on selection change,
    /// when the other pass may legitimately be mid-flight for the *previous*
    /// selection and would skip this one on its `status_pending` guard.
    pub(crate) fn refresh_push_state(&mut self, cx: &mut Context<Self>) {
        if self.push_probe_pending {
            return;
        }
        let Some(r) = self.resolve() else {
            self.push_state = None;
            return;
        };
        // A worktree's ahead/behind compares against the repo's default branch;
        // that is the right fallback for a branch with no upstream, because it
        // is what publishing would send.
        let fallback = self
            .data
            .repos
            .iter()
            .find(|repo| r.path.starts_with(&repo.repo_root) || repo.repo_root == r.path)
            .map(|repo| repo.compare_ref.clone())
            .unwrap_or_else(|| "origin/HEAD".to_string());
        let path = r.path.clone();
        self.push_probe_pending = true;

        cx.spawn(async move |view, cx| {
            let probed = cx
                .background_spawn(async move {
                    let git = GitWorktrees::new(SystemRunner);
                    let state = git.push_state(&path, &fallback);
                    (path, state)
                })
                .await;
            view.update(cx, |ws, cx| {
                ws.push_probe_pending = false;
                // Drop an answer for a selection the user has already left.
                log::debug!(
                    "push state {}: upstream={:?} unpushed={}",
                    probed.0.display(),
                    probed.1.upstream,
                    probed.1.unpushed
                );
                if ws.resolve().is_some_and(|r| r.path == probed.0) && ws.push_state.as_ref() != Some(&probed) {
                    ws.push_state = Some(probed);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The push control's two steps. The first click arms it — the label then
    /// states the remote and the count, because "push" alone does not say where
    /// — and the second runs git.
    ///
    /// A branch with no upstream is *published*: `push -u <remote> <branch>`,
    /// which creates the remote branch and records the tracking ref so the next
    /// push is an ordinary one.
    pub(crate) fn push_branch(&mut self, cx: &mut Context<Self>) {
        if self.pushing {
            return;
        }
        let Some((_, state)) = self.push_state.clone() else { return };
        let Some(r) = self.resolve() else { return };
        if state.unpushed == 0 {
            return;
        }
        if !self.push_armed {
            self.push_armed = true;
            cx.notify();
            return;
        }

        // Publishing needs a remote to publish to; without one there is nothing
        // honest to do, and git's own error would arrive seconds later.
        let publish = match &state.upstream {
            Some(_) => None,
            None => match GitWorktrees::new(SystemRunner).default_remote(&r.path) {
                Some(remote) => Some((remote, r.branch.clone())),
                None => {
                    self.push_armed = false;
                    self.set_note("no remote to publish to".to_string());
                    cx.notify();
                    return;
                }
            },
        };

        self.push_armed = false;
        self.pushing = true;
        self.set_note(String::new());
        cx.notify();

        let (path, count) = (r.path.clone(), state.unpushed);
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    let git = GitWorktrees::new(SystemRunner);
                    let publish = publish.as_ref().map(|(r, b)| (r.as_str(), b.as_str()));
                    git.push(&path, publish)
                })
                .await;
            view.update(cx, |ws, cx| {
                ws.pushing = false;
                ws.set_note(match result {
                    // git says what it did on stderr; its own words beat ours,
                    // and "Everything up-to-date" is worth seeing verbatim.
                    Ok(out) => {
                        let last = out.lines().last().unwrap_or("").trim().to_string();
                        if last.is_empty() {
                            format!("pushed {count} commit{}", if count == 1 { "" } else { "s" })
                        } else {
                            last
                        }
                    }
                    Err(e) => format!("push failed: {e}"),
                });
                // Whatever happened, the counts on screen are now stale.
                ws.refresh_push_state(cx);
                ws.refresh_status_live(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// One card's freshly probed git state, off the UI thread.
#[derive(Debug, Clone)]
pub(crate) struct LiveStatus {
    pub path: PathBuf,
    /// `None` when the branch probe failed; the card keeps the name it had.
    pub branch: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub dirty: usize,
}

/// Merge recomputed status into the cards, keyed by path. Returns whether
/// anything changed.
///
/// A path that no longer matches a card is dropped: the worktree was deleted
/// while its status was in flight, and `refresh()` has already rebuilt the
/// list without it.
pub(crate) fn apply_live_status(data: &mut crate::data::WorkspaceData, computed: Vec<LiveStatus>) -> bool {
    fn set<T: PartialEq>(slot: &mut T, value: T, changed: &mut bool) {
        if *slot != value {
            *slot = value;
            *changed = true;
        }
    }
    let mut changed = false;
    for st in computed {
        log::trace!("status {}: {:?} ↑{} ↓{} ~{}", st.path.display(), st.branch, st.ahead, st.behind, st.dirty);
        for repo in &mut data.repos {
            if repo.repo_root == st.path {
                set(&mut repo.ahead, st.ahead, &mut changed);
                set(&mut repo.behind, st.behind, &mut changed);
                set(&mut repo.dirty, st.dirty, &mut changed);
                if let Some(b) = &st.branch {
                    set(&mut repo.head_branch, b.clone(), &mut changed);
                }
            }
            for wt in &mut repo.worktrees {
                if wt.path == st.path
                    && let Some(b) = &st.branch
                {
                    set(&mut wt.branch, b.clone(), &mut changed);
                }
            }
        }
        for task in &mut data.tasks {
            for m in &mut task.members {
                if m.path == st.path {
                    set(&mut m.ahead, st.ahead, &mut changed);
                    set(&mut m.behind, st.behind, &mut changed);
                    set(&mut m.dirty, st.dirty, &mut changed);
                    if let Some(b) = &st.branch {
                        set(&mut m.branch, b.clone(), &mut changed);
                    }
                }
            }
            task.dirty = task.members.iter().map(|m| m.dirty).sum();
        }
    }
    changed
}

#[cfg(test)]
mod live_status_tests {
    use super::{apply_live_status, LiveStatus};
    use crate::data::{RepoView, TaskMemberView, TaskView, WorkspaceData, WorktreeView};
    use std::path::PathBuf;

    fn data() -> WorkspaceData {
        WorkspaceData {
            repos: vec![RepoView {
                name: "app".into(),
                repo_root: PathBuf::from("/r/app"),
                head_branch: "main".into(),
                compare_ref: "origin/main".into(),
                ahead: 0,
                behind: 0,
                dirty: 0,
                worktrees: vec![WorktreeView {
                    id: "wt1".into(),
                    name: "wt1".into(),
                    branch: "feat/old".into(),
                    path: PathBuf::from("/r/app-wt1"),
                    task_id: Some("t1".into()),
                }],
            }],
            tasks: vec![TaskView {
                id: "t1".into(),
                name: "t".into(),
                initials: "T".into(),
                color: 0,
                members: vec![TaskMemberView {
                    worktree_id: "wt1".into(),
                    repo: "app".into(),
                    branch: "feat/old".into(),
                    path: PathBuf::from("/r/app-wt1"),
                    ahead: 0,
                    behind: 0,
                    dirty: 0,
                    is_home: true,
                }],
                dirty: 0,
            }],
        }
    }

    fn st(path: &str, branch: Option<&str>, dirty: usize) -> LiveStatus {
        LiveStatus { path: PathBuf::from(path), branch: branch.map(Into::into), ahead: 1, behind: 0, dirty }
    }

    #[test]
    fn a_branch_switch_reaches_every_card_that_shows_it() {
        let mut d = data();
        assert!(apply_live_status(&mut d, vec![st("/r/app-wt1", Some("feat/new"), 2), st("/r/app", Some("dev"), 0)]));
        assert_eq!(d.repos[0].head_branch, "dev");
        assert_eq!(d.repos[0].worktrees[0].branch, "feat/new");
        assert_eq!(d.tasks[0].members[0].branch, "feat/new");
        assert_eq!(d.tasks[0].members[0].dirty, 2);
        assert_eq!(d.tasks[0].dirty, 2);
        assert_eq!(
            d.forge_branches().iter().map(|key| key.branch.as_str()).collect::<Vec<_>>(),
            ["dev", "feat/new"],
        );
    }

    #[test]
    fn forge_polling_keeps_the_same_branch_in_each_project() {
        let mut first = data();
        first.repos[0].head_branch = "feat/old".into();
        let mut second = data();
        second.repos[0].repo_root = PathBuf::from("/r/api");
        second.repos[0].head_branch = "feat/old".into();
        first.repos.extend(second.repos);

        let keys = first.forge_branches();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].branch, keys[1].branch);
        assert_eq!(keys[0].repo_dir, PathBuf::from("/r/app"));
        assert_eq!(keys[1].repo_dir, PathBuf::from("/r/api"));
    }

    #[test]
    fn a_failed_branch_probe_keeps_the_known_name() {
        let mut d = data();
        apply_live_status(&mut d, vec![st("/r/app-wt1", None, 0)]);
        assert_eq!(d.tasks[0].members[0].branch, "feat/old");
    }

    #[test]
    fn an_unchanged_answer_reports_no_change() {
        let mut d = data();
        apply_live_status(&mut d, vec![st("/r/app-wt1", Some("feat/new"), 2)]);
        assert!(!apply_live_status(&mut d, vec![st("/r/app-wt1", Some("feat/new"), 2)]));
    }
}
