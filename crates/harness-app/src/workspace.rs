//! Root view: the v1 three-column skeleton (docs/architecture.md §3.4).
//! Left worktree sidebar · center tab strip / terminal / status bar · right
//! diff-panel placeholder. All data except the terminal is hardcoded mock
//! state (Spike 4 wires the real registry/git/forge sources).

use std::path::PathBuf;

use gpui::{
    App, Context, FocusHandle, Focusable, IntoElement, Render, Rgba, Styled, Window, div,
    prelude::*, px,
};

use crate::model::{AgentStatus, ChangedFile, RepoGroup, mock_changed_files, mock_repos};
use crate::terminal_pane::TerminalPane;
use crate::theme::{HarnessTheme, theme, tint};

pub struct HarnessWorkspace {
    repos: Vec<RepoGroup>,
    changed_files: Vec<ChangedFile>,
    /// (repo index, worktree index)
    selected: (usize, usize),
    terminal_pane: gpui::Entity<TerminalPane>,
    focus_handle: FocusHandle,
}

impl HarnessWorkspace {
    pub fn new(worktree_root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let terminal_pane = cx.new(|cx| TerminalPane::new(worktree_root, window, cx));
        Self {
            repos: mock_repos(),
            changed_files: mock_changed_files(),
            selected: (0, 0),
            terminal_pane,
            focus_handle: cx.focus_handle(),
        }
    }

    fn selected_card(&self) -> &crate::model::WorktreeCard {
        let (repo, wt) = self.selected;
        &self.repos[repo].worktrees[wt]
    }

    fn render_top_bar(&self, hx: &HarnessTheme) -> impl IntoElement {
        let branch = self.selected_card().branch;
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
                            .child(branch),
                    ),
            )
            .child(div().text_xs().text_color(hx.tx2).child("spike 1 · dark"))
    }

    fn render_sidebar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut column = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(260.0))
            .h_full()
            .bg(hx.bg1)
            .border_r_1()
            .border_color(hx.border)
            .overflow_hidden()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("WORKTREES"),
            );

        for (repo_ix, repo) in self.repos.iter().enumerate() {
            column = column.child(
                div()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .text_sm()
                    .text_color(hx.tx1)
                    .child(repo.name),
            );
            // Default-branch entry (read-only workspace, D8).
            column = column.child(
                div()
                    .mx_2()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .font_family(hx.font_mono)
                    .text_color(hx.tx2)
                    .child(format!("◦ {}", repo.default_branch)),
            );
            for (wt_ix, card) in repo.worktrees.iter().enumerate() {
                let is_selected = self.selected == (repo_ix, wt_ix);
                column = column.child(self.render_worktree_card(
                    hx,
                    repo_ix,
                    wt_ix,
                    card,
                    is_selected,
                    cx,
                ));
            }
        }

        column.child(
            div()
                .mx_2()
                .mt_3()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(hx.border)
                .text_sm()
                .text_color(hx.tx2)
                .child("+ new worktree"),
        )
    }

    fn render_worktree_card(
        &self,
        hx: &HarnessTheme,
        repo_ix: usize,
        wt_ix: usize,
        card: &crate::model::WorktreeCard,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (pill_color, _pulse) = status_color(card.status, hx);
        let mut meta = format!("+{} −{}", card.ahead, card.behind);
        if card.dirty_files > 0 {
            meta.push_str(&format!(" · ~{}", card.dirty_files));
        }

        div()
            .id(("worktree-card", repo_ix * 100 + wt_ix))
            .mx_2()
            .mb_1()
            .p_2()
            .rounded_md()
            .bg(if is_selected { hx.surface } else { hx.bg2 })
            .border_1()
            .border_color(if is_selected { hx.accent } else { hx.border })
            .hover(|style| style.bg(hx.surface_hover))
            .cursor_pointer()
            .on_click(cx.listener(move |workspace, _, _, cx| {
                workspace.selected = (repo_ix, wt_ix);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        // Avatar: 2-letter initials chip.
                        div()
                            .flex_none()
                            .size(px(22.0))
                            .rounded_sm()
                            .bg(tint(hx.accent, 0.25))
                            .text_xs()
                            .text_color(hx.tx0)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(card.initials),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(hx.tx0)
                            .child(card.name),
                    )
                    .child(
                        // Agent status pill.
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_1p5()
                            .py_0p5()
                            .rounded_full()
                            .bg(tint(pill_color, 0.18))
                            .child(div().size(px(6.0)).rounded_full().bg(pill_color))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(pill_color)
                                    .child(card.status.label()),
                            ),
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
                            .child(card.branch),
                    )
                    .child(div().text_xs().text_color(hx.tx2).child(meta)),
            )
    }

    fn render_center(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let card = self.selected_card();
        let (pill_color, _) = status_color(card.status, hx);
        let status_text = format!("{} · {}", card.status.label(), card.branch);
        let spawned = self.terminal_pane.read(cx).spawned.clone();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .bg(hx.bg0)
            // Tab strip.
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
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(hx.bg0)
                            .border_1()
                            .border_color(hx.border)
                            .text_sm()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx0)
                            .child("❯_ terminal"),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_sm()
                            .text_color(hx.tx2)
                            .child("+"),
                    ),
            )
            // Terminal content.
            .child(div().flex_1().min_h_0().child(self.terminal_pane.clone()))
            // Status bar: agent state · branch (§3.4).
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
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(7.0)).rounded_full().bg(pill_color))
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(hx.font_mono)
                                    .text_color(hx.tx1)
                                    .child(status_text),
                            ),
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
        let totals = self.changed_files.iter().fold((0, 0), |acc, file| {
            (acc.0 + file.adds, acc.1 + file.dels)
        });

        let mut panel = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(300.0))
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
                            .text_xs()
                            .font_family(hx.font_mono)
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(div().text_color(hx.green).child(format!("+{}", totals.0)))
                                    .child(div().text_color(hx.red).child(format!("−{}", totals.1))),
                            ),
                    ),
            );

        for file in &self.changed_files {
            let status_color = match file.status {
                'A' => hx.green,
                'D' => hx.red,
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
                            .text_color(status_color)
                            .child(file.status.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx1)
                            .overflow_hidden()
                            .child(file.path),
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

        panel.child(
            div()
                .flex_1()
                .m_2()
                .rounded_md()
                .border_1()
                .border_color(hx.border)
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hx.tx2)
                .child("unified diff view — Spike 4"),
        )
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

fn status_color(status: AgentStatus, hx: &HarnessTheme) -> (Rgba, bool) {
    match status {
        AgentStatus::Running => (hx.green, true),
        AgentStatus::Waiting => (hx.amber, false),
        AgentStatus::Idle => (Rgba { a: 1.0, ..hx.tx2 }, false),
    }
}
