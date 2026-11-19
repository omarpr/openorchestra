//! Keyboard navigation: switching tabs, moving between worktrees in sidebar
//! order, and jumping to the next agent that is waiting on you.

use super::*;

impl HarnessWorkspace {
    /// Make `id` the active tab of the selected worktree and focus it — what a
    /// click on the tab does.
    pub(crate) fn activate_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let leaving_hooks = self.hooks_edit.is_some()
            && self
                .panes
                .get(&r.key)
                .and_then(|p| p.tabs.iter().find(|t| t.id == id))
                .is_some_and(|t| !matches!(t.content, TabContent::File { .. }));
        if let Some(pane) = self.panes.get_mut(&r.key) {
            pane.active = id;
        }
        if leaving_hooks {
            self.exit_hooks_edit(window, cx);
        }
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn tab_ids(&self) -> Vec<u64> {
        self.resolve()
            .and_then(|r| self.panes.get(&r.key))
            .map(|p| p.tabs.iter().map(|t| t.id).collect())
            .unwrap_or_default()
    }

    /// ⌘1…⌘8 pick that tab; `None` (⌘9) picks the last, as browsers do.
    pub(crate) fn activate_tab_at(
        &mut self,
        ix: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids = self.tab_ids();
        let pick = match ix {
            Some(i) => ids.get(i).copied(),
            None => ids.last().copied(),
        };
        if let Some(id) = pick {
            self.activate_tab(id, window, cx);
        }
    }

    pub(crate) fn cycle_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.tab_ids();
        let Some(active) = self
            .resolve()
            .and_then(|r| self.panes.get(&r.key))
            .map(|p| p.active)
        else {
            return;
        };
        if let Some(id) = step(&ids, ids.iter().position(|&i| i == active), forward) {
            self.activate_tab(id, window, cx);
        }
    }

    /// Every selectable entry in sidebar (PROJECTS) order: each project's main
    /// checkout, then its worktrees. Paired with the path it resolves to.
    ///
    /// A multi-project task's coordinator agent is a target too (its path is
    /// the task folder, where its status lives), placed just before the first
    /// of its members, so cycling and "next waiting agent" both reach it.
    fn navigation_targets(&self) -> Vec<(Selection, PathBuf)> {
        let tasks_root = self.data_dir.join("tasks");
        let coordinator = |task_id: &str| {
            self.task_store
                .get(task_id)
                .filter(|t| !t.archived && super::coordinator::has_coordinator(t))
                .map(|t| {
                    (
                        Selection::Task {
                            task_id: t.id.clone(),
                            member_id: None,
                        },
                        worktrees::task_coordinator_dir(&tasks_root, &t.slug),
                    )
                })
        };
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out = Vec::new();
        for repo in &self.data.repos {
            out.push((Selection::Main(repo.name.clone()), repo.repo_root.clone()));
            for wt in &repo.worktrees {
                let sel = match &wt.task_id {
                    Some(t) => {
                        if seen.insert(t.clone())
                            && let Some(c) = coordinator(t)
                        {
                            out.push(c);
                        }
                        Selection::Task {
                            task_id: t.clone(),
                            member_id: Some(wt.id.clone()),
                        }
                    }
                    None => Selection::Worktree(wt.id.clone()),
                };
                out.push((sel, wt.path.clone()));
            }
        }
        // A coordinator whose members are not listed under any project.
        for task in self.task_store.active() {
            if !seen.contains(&task.id)
                && let Some(c) = coordinator(&task.id)
            {
                out.push(c);
            }
        }
        out
    }

    pub(crate) fn cycle_worktree(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets = self.navigation_targets();
        let here = self.resolve().map(|r| r.path);
        let at = targets.iter().position(|(_, p)| Some(p) == here.as_ref());
        let paths: Vec<usize> = (0..targets.len()).collect();
        if let Some(ix) = step(&paths, at, forward) {
            let sel = targets[ix].0.clone();
            self.select(sel, window, cx);
        }
    }

    /// Go to the next agent (in sidebar order, after the current one) that is
    /// waiting on you, and focus its tab.
    pub(crate) fn next_waiting_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.navigation_targets();
        let here = self.resolve().map(|r| r.path);
        let at = targets
            .iter()
            .position(|(_, p)| Some(p) == here.as_ref())
            .unwrap_or(targets.len());
        let waiting = |p: &PathBuf| self.agent_status.get(p) == Some(&AgentState::Waiting);
        // Strictly after the current entry first, wrapping round to it last,
        // so pressing it repeatedly walks every waiting agent in turn.
        let order = (at + 1..targets.len()).chain(0..=at.min(targets.len().saturating_sub(1)));
        let Some(path) = order
            .filter_map(|i| targets.get(i))
            .map(|(_, p)| p)
            .find(|p| waiting(p))
            .cloned()
        else {
            self.set_note("No agent is waiting on you".into());
            cx.notify();
            return;
        };
        let (_, _, id) = self.alert_target(&path);
        let kind = self
            .manager
            .registry
            .entries()
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.agent_kind.clone())
            // A coordinator has no registry entry: it runs the task's agent.
            .or_else(|| {
                self.coordinator_task_for_path(&path)
                    .map(|t| t.agent_kind.clone())
            })
            .unwrap_or_default();
        // Root state is Waiting when any tab is. Kind-only navigation would
        // open the first tab of that kind and dismiss that one, leaving the
        // tab that is actually waiting.
        let nav = super::state::alert_nav_id(&id, &kind, self.conductor.waiting_tab(&path));
        self.navigate_to_alert(&nav, window, cx);
        self.dismiss_selected_agent();
    }

    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.prefs.sidebar_open = !self.prefs.sidebar_open;
        self.save_prefs(cx);
        cx.notify();
    }
}

/// The element after (or before) `at` in `items`, wrapping. With no current
/// position, forward starts at the first and backward at the last.
fn step<T: Copy>(items: &[T], at: Option<usize>, forward: bool) -> Option<T> {
    if items.is_empty() {
        return None;
    }
    let n = items.len();
    let ix = match (at, forward) {
        (Some(i), true) => (i + 1) % n,
        (Some(i), false) => (i + n - 1) % n,
        (None, true) => 0,
        (None, false) => n - 1,
    };
    items.get(ix).copied()
}

#[cfg(test)]
mod tests {
    use super::step;

    #[test]
    fn stepping_wraps_both_ways() {
        let v = [10, 20, 30];
        assert_eq!(step(&v, Some(0), true), Some(20));
        assert_eq!(step(&v, Some(2), true), Some(10));
        assert_eq!(step(&v, Some(0), false), Some(30));
        assert_eq!(step(&v, None, true), Some(10));
        assert_eq!(step(&v, None, false), Some(30));
        assert_eq!(step::<i32>(&[], Some(0), true), None);
    }
}
