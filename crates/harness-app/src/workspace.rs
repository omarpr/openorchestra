//! Root view — the three-column shell (docs/architecture.md §3.4), Spike 4
//! edition: the sidebar and diff panel are now driven by the real worktree
//! registry + live git (`data.rs` / the `worktrees` crate), not mocks.
//!
//! - Sidebar: repos grouped from the registry (D4); each repo shows a read-only
//!   `main` entry (D8) + worktree cards with real avatars and git status
//!   (↑ahead ↓behind ~dirty). Hover-free actions: Archive / Delete (confirm).
//! - Center: per-worktree tab sets (D5) — each selection caches its own
//!   terminal (pi for worktrees, plain shell for `main`) + editor pane.
//! - Diff panel: the selected entry's merge-base diff (D10) — file list with
//!   M/A/D badges + a unified-diff preview.
//! - `+ new worktree`: runs the real create flow (`git worktree add` + registry).

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, Rgba, Styled, Window, div, prelude::*, px, rgb,
};

use worktrees::{Change, CreateRequest, GitWorktrees, Manager, Registry, SystemRunner};

use crate::data::{compute_diff, WorkspaceData};
use crate::editor_pane::EditorPane;
use crate::model::AgentStatus;
use crate::terminal_pane::TerminalPane;
use crate::theme::{theme, tint, HarnessTheme};

#[derive(Clone, Copy, PartialEq)]
enum ActiveTab {
    Terminal,
    Editor,
}

/// The selected sidebar entry: a repo's read-only `main`, or one of its
/// worktrees. Indices refer into `data.repos`.
#[derive(Clone, Copy, PartialEq)]
enum Selection {
    Main(usize),
    Worktree(usize, usize),
}

/// One selection's cached panes + remembered tab (D5).
struct PaneSet {
    terminal: Entity<TerminalPane>,
    editor: Entity<EditorPane>,
    active_tab: ActiveTab,
}

pub struct HarnessWorkspace {
    manager: Manager<SystemRunner>,
    data: WorkspaceData,
    selection: Selection,
    panes: HashMap<String, PaneSet>,
    diff: worktrees::Diff,
    pending_delete: Option<String>,
    next_task: usize,
    note: String,
    focus_handle: FocusHandle,
}

impl HarnessWorkspace {
    pub fn new(launch_dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let git = GitWorktrees::new(SystemRunner);
        let repo_root = git.repo_root(&launch_dir).unwrap_or_else(|_| launch_dir.clone());
        let repo_name = repo_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_string());

        // Harness state lives beside the repo so worktrees don't nest inside it.
        let data_dir = repo_root
            .parent()
            .map(|p| p.join(".omar-harness"))
            .unwrap_or_else(|| repo_root.join(".omar-harness"));
        let reg_path = data_dir.join("registry.json");
        let registry = Registry::load(&reg_path).unwrap_or_else(|e| {
            log::warn!("registry load failed ({e}); starting empty");
            Registry::new_at(&reg_path)
        });
        let manager = Manager::new(registry, git, data_dir.join("worktrees"));

        let data = WorkspaceData::build(manager.git(), &manager.registry, &repo_name, &repo_root);

        let mut this = Self {
            manager,
            data,
            selection: Selection::Main(0),
            panes: HashMap::new(),
            diff: worktrees::Diff::default(),
            pending_delete: None,
            next_task: 1,
            note: format!("repo: {repo_name}"),
            focus_handle: cx.focus_handle(),
        };
        this.ensure_pane(window, cx);
        this.recompute_diff();
        this
    }

    // ---- selection / panes ----------------------------------------------

    fn selection_key(&self) -> String {
        match self.selection {
            Selection::Main(r) => format!("main:{}", self.data.repos[r].name),
            Selection::Worktree(r, w) => self.data.repos[r].worktrees[w].id.clone(),
        }
    }

    /// (path, spawn_agent, compare_ref) for the current selection.
    fn selected(&self) -> (PathBuf, bool, String) {
        match self.selection {
            Selection::Main(r) => {
                let repo = &self.data.repos[r];
                (repo.repo_root.clone(), false, repo.compare_ref.clone())
            }
            Selection::Worktree(r, w) => {
                let repo = &self.data.repos[r];
                (repo.worktrees[w].path.clone(), true, repo.compare_ref.clone())
            }
        }
    }

    fn ensure_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.selection_key();
        if self.panes.contains_key(&key) {
            return;
        }
        let (path, spawn_agent, _cmp) = self.selected();
        let terminal = cx.new(|cx| TerminalPane::new(path.clone(), spawn_agent, window, cx));
        let editor = cx.new(|cx| EditorPane::new(&path, window, cx));
        let active_tab = if spawn_agent {
            ActiveTab::Terminal
        } else {
            ActiveTab::Editor
        };
        self.panes.insert(
            key,
            PaneSet {
                terminal,
                editor,
                active_tab,
            },
        );
    }

    fn recompute_diff(&mut self) {
        let (path, _agent, cmp) = self.selected();
        self.diff = compute_diff(self.manager.git(), &path, &cmp);
    }

    fn select(&mut self, sel: Selection, window: &mut Window, cx: &mut Context<Self>) {
        self.selection = sel;
        self.pending_delete = None;
        self.ensure_pane(window, cx);
        self.recompute_diff();
        cx.notify();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repo_name = self.data.repos[0].name.clone();
        let repo_root = self.data.repos[0].repo_root.clone();
        self.data = WorkspaceData::build(self.manager.git(), &self.manager.registry, &repo_name, &repo_root);
        self.clamp_selection();
        self.ensure_pane(window, cx);
        self.recompute_diff();
        cx.notify();
    }

    fn clamp_selection(&mut self) {
        let valid = match self.selection {
            Selection::Main(r) => r < self.data.repos.len(),
            Selection::Worktree(r, w) => {
                r < self.data.repos.len() && w < self.data.repos[r].worktrees.len()
            }
        };
        if !valid {
            self.selection = Selection::Main(0);
        }
    }

    // ---- lifecycle actions ----------------------------------------------

    fn action_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repo = &self.data.repos[0];
        let slug = format!("task-{}", self.next_task);
        let req = CreateRequest {
            repo: repo.name.clone(),
            repo_root: repo.repo_root.clone(),
            task_slug: slug.clone(),
            branch: format!("feat/{slug}"),
            base_ref: Some(repo.compare_ref.clone()),
            created_at: now_stamp(),
        };
        match self.manager.create(req) {
            Ok(id) => {
                self.next_task += 1;
                self.note = format!("created {id}");
                self.refresh(window, cx);
                self.select_by_id(&id, window, cx);
            }
            Err(e) => {
                self.note = format!("create failed: {e}");
                cx.notify();
            }
        }
    }

    fn action_archive(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        match self.manager.archive(&id) {
            Ok(()) => self.note = format!("archived {id}"),
            Err(e) => self.note = format!("archive failed: {e}"),
        }
        self.selection = Selection::Main(0);
        self.refresh(window, cx);
    }

    fn action_delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        match self.manager.delete(&id) {
            Ok(()) => self.note = format!("deleted {id}"),
            Err(e) => self.note = format!("delete failed: {e}"),
        }
        self.panes.remove(&id);
        self.pending_delete = None;
        self.selection = Selection::Main(0);
        self.refresh(window, cx);
    }

    fn select_by_id(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        for (r, repo) in self.data.repos.iter().enumerate() {
            if let Some(w) = repo.worktrees.iter().position(|wt| wt.id == id) {
                self.select(Selection::Worktree(r, w), window, cx);
                return;
            }
        }
    }

    // ---- rendering -------------------------------------------------------

    fn render_top_bar(&self, hx: &HarnessTheme) -> impl IntoElement {
        let (_, _, branch_ref) = self.selected();
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(px(38.0))
            .px_3()
            .bg(hx.bg1)
            .border_b_1()
            .border_color(hx.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().text_color(hx.tx1).child("harness"))
                    .child(div().text_sm().text_color(hx.tx2).child("·"))
                    .child(
                        div()
                            .text_sm()
                            .font_family(hx.font_mono)
                            .text_color(hx.accent)
                            .child(branch_ref),
                    ),
            )
            .child(div().text_xs().text_color(hx.tx2).child("spike 4 · worktrees"))
    }

    fn render_sidebar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut column = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(280.0))
            .h_full()
            .bg(hx.bg1)
            .border_r_1()
            .border_color(hx.border)
            .overflow_hidden()
            .child(div().px_3().py_2().text_xs().text_color(hx.tx2).child("WORKTREES"));

        for (repo_ix, repo) in self.data.repos.iter().enumerate() {
            column = column.child(
                div()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .text_sm()
                    .text_color(hx.tx1)
                    .child(repo.name.clone()),
            );

            // main entry (read-only workspace, D8).
            let main_selected = self.selection == Selection::Main(repo_ix);
            column = column.child(
                div()
                    .id(("main-entry", repo_ix))
                    .mx_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .when(main_selected, |d| d.bg(hx.surface).border_1().border_color(hx.accent))
                    .text_sm()
                    .font_family(hx.font_mono)
                    .text_color(if main_selected { hx.tx0 } else { hx.tx2 })
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.select(Selection::Main(repo_ix), window, cx);
                    }))
                    .child(format!("◦ {}", repo.default_branch)),
            );

            for (wt_ix, card) in repo.worktrees.iter().enumerate() {
                column = column.child(self.render_worktree_card(hx, repo_ix, wt_ix, card, cx));
            }
        }

        column.child(
            div()
                .id("new-worktree")
                .mx_2()
                .mt_3()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(hx.border)
                .text_sm()
                .text_color(hx.tx1)
                .cursor_pointer()
                .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                .on_click(cx.listener(|ws, _, window, cx| ws.action_create(window, cx)))
                .child("+ new worktree"),
        )
    }

    fn render_worktree_card(
        &self,
        hx: &HarnessTheme,
        repo_ix: usize,
        wt_ix: usize,
        card: &crate::data::WorktreeView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_selected = self.selection == Selection::Worktree(repo_ix, wt_ix);
        let id = card.id.clone();
        let confirming = self.pending_delete.as_deref() == Some(id.as_str());

        let mut meta = format!("↑{} ↓{}", card.ahead, card.behind);
        if card.dirty > 0 {
            meta.push_str(&format!(" · ~{}", card.dirty));
        }
        let pill = status_color(AgentStatus::Idle, hx);

        let mut card_div = div()
            .id(("worktree-card", repo_ix * 1000 + wt_ix))
            .mx_2()
            .mb_1()
            .p_2()
            .rounded_md()
            .bg(if is_selected { hx.surface } else { hx.bg2 })
            .border_1()
            .border_color(if is_selected { hx.accent } else { hx.border })
            .hover(|style| style.bg(hx.surface_hover))
            .cursor_pointer()
            .on_click(cx.listener(move |ws, _, window, cx| {
                ws.select(Selection::Worktree(repo_ix, wt_ix), window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .size(px(22.0))
                            .rounded_sm()
                            .bg(tint(avatar_color(card.color), 0.30))
                            .text_xs()
                            .text_color(hx.tx0)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(card.initials.clone()),
                    )
                    .child(div().flex_1().text_sm().text_color(hx.tx0).child(card.name.clone()))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_1p5()
                            .py_0p5()
                            .rounded_full()
                            .bg(tint(pill, 0.18))
                            .child(div().size(px(6.0)).rounded_full().bg(pill))
                            .child(div().text_xs().text_color(pill).child(AgentStatus::Idle.label())),
                    ),
            )
            .child(
                div()
                    .mt_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx1)
                            .child(card.branch.clone()),
                    )
                    .child(div().text_xs().text_color(hx.tx2).child(meta)),
            );

        // Action row shows on the selected card (archive / delete-with-confirm).
        if is_selected {
            let archive_id = id.clone();
            let del_id = id.clone();
            card_div = card_div.child(
                div()
                    .mt_2()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id(("archive", repo_ix * 1000 + wt_ix))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(hx.border)
                            .text_xs()
                            .text_color(hx.tx1)
                            .cursor_pointer()
                            .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.action_archive(archive_id.clone(), window, cx);
                            }))
                            .child("Archive"),
                    )
                    .child(
                        div()
                            .id(("delete", repo_ix * 1000 + wt_ix))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(if confirming { hx.red } else { hx.border })
                            .text_xs()
                            .text_color(if confirming { hx.red } else { hx.tx1 })
                            .cursor_pointer()
                            .hover(|d| d.text_color(hx.red))
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                if ws.pending_delete.as_deref() == Some(del_id.as_str()) {
                                    ws.action_delete(del_id.clone(), window, cx);
                                } else {
                                    ws.pending_delete = Some(del_id.clone());
                                    cx.notify();
                                }
                            }))
                            .child(if confirming { "Delete — confirm?" } else { "Delete" }),
                    ),
            );
        }

        card_div
    }

    fn render_tab(
        &self,
        hx: &HarnessTheme,
        label: &str,
        tab: ActiveTab,
        active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_current = active;
        div()
            .id(match tab {
                ActiveTab::Terminal => "tab-terminal",
                ActiveTab::Editor => "tab-editor",
            })
            .px_3()
            .py_1()
            .rounded_md()
            .when(is_current, |d| d.bg(hx.bg0).border_1().border_color(hx.border))
            .text_sm()
            .font_family(hx.font_mono)
            .text_color(if is_current { hx.tx0 } else { hx.tx2 })
            .cursor_pointer()
            .hover(|d| d.text_color(hx.tx1))
            .on_click(cx.listener(move |ws, _, window, cx| {
                let key = ws.selection_key();
                if let Some(pane) = ws.panes.get_mut(&key) {
                    pane.active_tab = tab;
                    let handle = match tab {
                        ActiveTab::Terminal => pane.terminal.read(cx).focus_handle(cx),
                        ActiveTab::Editor => pane.editor.read(cx).focus_handle(cx),
                    };
                    window.focus(&handle, cx);
                    cx.notify();
                }
            }))
            .child(label.to_string())
    }

    fn render_center(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let key = self.selection_key();
        let pane = self.panes.get(&key);
        let active_tab = pane.map(|p| p.active_tab).unwrap_or(ActiveTab::Editor);

        let content: AnyElement = match (pane, active_tab) {
            (Some(p), ActiveTab::Terminal) => p.terminal.clone().into_any_element(),
            (Some(p), ActiveTab::Editor) => p.editor.clone().into_any_element(),
            (None, _) => div().into_any_element(),
        };
        let spawned = pane
            .map(|p| p.terminal.read(cx).spawned.clone())
            .unwrap_or_default();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .bg(hx.bg0)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_end()
                    .h(px(34.0))
                    .px_2()
                    .gap_1()
                    .bg(hx.bg1)
                    .border_b_1()
                    .border_color(hx.border)
                    .child(self.render_tab(hx, "❯_ terminal", ActiveTab::Terminal, active_tab == ActiveTab::Terminal, cx))
                    .child(self.render_tab(hx, "{} editor", ActiveTab::Editor, active_tab == ActiveTab::Editor, cx)),
            )
            .child(div().flex_1().min_h_0().child(content))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(26.0))
                    .px_3()
                    .bg(hx.bg1)
                    .border_t_1()
                    .border_color(hx.border)
                    .child(
                        div()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx1)
                            .child(self.note.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx2)
                            .child(format!("pty: {spawned}")),
                    ),
            )
    }

    fn render_diff_panel(&self, hx: &HarnessTheme) -> impl IntoElement {
        let (adds, dels) = self.diff.totals();

        let mut panel = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(320.0))
            .h_full()
            .bg(hx.bg1)
            .border_l_1()
            .border_color(hx.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .child(div().text_xs().text_color(hx.tx2).child("CHANGES"))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .child(div().text_color(hx.green).child(format!("+{adds}")))
                            .child(div().text_color(hx.red).child(format!("−{dels}"))),
                    ),
            );

        if self.diff.files.is_empty() {
            panel = panel.child(
                div()
                    .m_2()
                    .px_2()
                    .py_2()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("no changes vs merge-base"),
            );
        }

        for file in &self.diff.files {
            let badge = file.change.badge();
            let badge_color = match file.change {
                Change::Added => hx.green,
                Change::Deleted => hx.red,
                Change::Renamed => hx.accent,
                _ => hx.amber,
            };
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .mx_2()
                    .mb_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(hx.bg2)
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(badge_color)
                            .child(badge.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx1)
                            .overflow_hidden()
                            .child(file.path.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx2)
                            .child(format!("+{} −{}", file.adds, file.dels)),
                    ),
            );
        }

        // Unified-diff preview (capped so a huge diff doesn't blow up layout).
        let mut preview = div()
            .flex_1()
            .min_h_0()
            .m_2()
            .p_2()
            .rounded_md()
            .bg(hx.bg0)
            .border_1()
            .border_color(hx.border)
            .overflow_hidden()
            .flex()
            .flex_col()
            .text_xs()
            .font_family(hx.font_mono);

        if self.diff.unified.is_empty() {
            preview = preview.child(div().text_color(hx.tx2).child("—"));
        } else {
            for line in self.diff.unified.lines().take(400) {
                let color = if line.starts_with('+') {
                    hx.green
                } else if line.starts_with('-') {
                    hx.red
                } else if line.starts_with("@@") {
                    hx.accent
                } else {
                    hx.tx2
                };
                preview = preview.child(
                    div()
                        .h(px(15.0))
                        .flex_none()
                        .text_color(color)
                        .overflow_hidden()
                        .child(line.to_string()),
                );
            }
        }

        panel.child(preview)
    }
}

impl Focusable for HarnessWorkspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for HarnessWorkspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hx = theme(cx).clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(hx.bg0)
            .font_family(hx.font_ui)
            .text_color(hx.tx0)
            .child(self.render_top_bar(&hx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(&hx, cx))
                    .child(self.render_center(&hx, cx))
                    .child(self.render_diff_panel(&hx)),
            )
    }
}

fn status_color(status: AgentStatus, hx: &HarnessTheme) -> Rgba {
    match status {
        AgentStatus::Running => hx.green,
        AgentStatus::Waiting => hx.amber,
        AgentStatus::Idle => Rgba { a: 1.0, ..hx.tx2 },
    }
}

/// Map a registry avatar palette index (0..8) to a color.
fn avatar_color(ix: u8) -> Rgba {
    const PALETTE: [u32; 8] = [
        0x4f8ff7, 0x3fb950, 0xd29922, 0xf85149, 0xd2a8ff, 0x4fd6be, 0xf0883e, 0xdb61a2,
    ];
    rgb(PALETTE[(ix as usize) % PALETTE.len()])
}

/// Epoch-seconds timestamp for `created_at`. Spike-grade (not full ISO-8601);
/// the registry stores it verbatim and only displays it.
fn now_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}
