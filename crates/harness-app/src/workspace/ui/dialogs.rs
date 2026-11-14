//! Centered confirmation dialogs: quit, remove project, archive/delete, and
//! approving a project's hook scripts. They share one card, one backdrop and
//! one button style, so each dialog only states what it asks.

use super::super::lifecycle::{EndConfirm, EndKind, Loss};
use super::super::*;
use gpui::{Div, SharedString, relative};

/// How a dialog button reads.
#[derive(Clone, Copy, PartialEq)]
enum Tone {
    /// The safe, highlighted choice (usually Cancel).
    Primary,
    /// A plain alternative.
    Plain,
    /// Destroys something: red, never the highlighted choice.
    Danger,
}

fn dialog_button(hx: &HarnessTheme, id: &'static str, label: impl Into<SharedString>, tone: Tone) -> gpui::Stateful<Div> {
    let (border, bg, fg, hover) = match tone {
        Tone::Primary => (hx.accent, tint(hx.accent, 0.16), hx.accent, hx.accent),
        Tone::Plain => (hx.border2, hx.surface, hx.tx0, hx.red),
        Tone::Danger => (tint(hx.red, 0.6), tint(hx.red, 0.12), hx.red, hx.red),
    };
    div()
        .id(id)
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .h(px(34.0))
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_sm()
        .text_color(fg)
        .cursor_pointer()
        .hover(move |d| d.border_color(hover).text_color(hover))
        .child(label.into())
}

impl HarnessWorkspace {
    /// The card every dialog sits in. Clicks inside it never reach the
    /// backdrop's dismiss handler.
    fn dialog_card(&self, hx: &HarnessTheme, title: impl Into<SharedString>, cx: &mut Context<Self>) -> Div {
        div()
            .w(px(440.0))
            .max_w(relative(0.92))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hx.tx0)
                    .child(title.into()),
            )
    }

    /// Dim the window behind `card`; a click outside it runs `dismiss`.
    fn dialog_backdrop(
        &self,
        id: &'static str,
        card: Div,
        dismiss: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            // Occlude, not just catch clicks: without it the elements under
            // an overlay still hover, and their tooltips show through it.
            .occlude()
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000099))
            .on_mouse_down(MouseButton::Left, cx.listener(move |ws, _, window, cx| dismiss(ws, window, cx)))
            .child(card)
            .into_any_element()
    }

    /// The keep-or-close-terminals confirmation shown on quit. Clicking the
    /// backdrop cancels the quit — the safe default.
    pub(crate) fn render_close_prompt(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let Some(prompt) = self.close_prompt.as_ref() else {
            return div().into_any_element();
        };
        let n = prompt.session_count;
        let remember = prompt.remember;
        let count_label = format!("{n} terminal session{} still running.", if n == 1 { " is" } else { "s are" });

        // Checkbox: a small square that fills with the accent when ticked.
        let checkbox = div()
            .id("close-remember")
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .on_click(cx.listener(|ws, _, _, cx| {
                if let Some(p) = ws.close_prompt.as_mut() {
                    p.remember = !p.remember;
                }
                cx.notify();
            }))
            .child(
                div()
                    .size(px(16.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(if remember { hx.accent } else { hx.border2 })
                    .when(remember, |d| d.bg(hx.accent))
                    .when(remember, |d| d.child(div().text_xs().text_color(rgb(0xffffff)).child("✓"))),
            )
            .child(div().text_sm().text_color(hx.tx1).child("Remember my choice"));

        let card = self
            .dialog_card(hx, "Quit openOrchestra?", cx)
            .child(div().text_sm().text_color(hx.tx1).child(format!(
                "{count_label} Keep them running in the background, or close them?"
            )))
            .child(checkbox)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_1()
                    .child(
                        dialog_button(hx, "close-keep", "Keep running", Tone::Primary)
                            .on_click(cx.listener(|ws, _, _, cx| ws.resolve_quit(true, cx))),
                    )
                    .child(
                        dialog_button(hx, "close-kill", "Close terminals", Tone::Plain)
                            .on_click(cx.listener(|ws, _, _, cx| ws.resolve_quit(false, cx))),
                    ),
            );
        self.dialog_backdrop("close-backdrop", card, |ws, _, cx| ws.cancel_quit(cx), cx)
    }

    /// "Remove ‘x’?" — the confirm step for removing a project or an archived
    /// entry. Cancel is the highlighted, safe choice; clicking outside cancels.
    pub(crate) fn render_removal_prompt(&self, hx: &HarnessTheme, removal: Removal, cx: &mut Context<Self>) -> AnyElement {
        let (name, body) = match &removal {
            Removal::Project(name) => (
                name.clone(),
                "It leaves the sidebar. Its files, branches and worktrees stay on disk — add the folder again to bring it back.",
            ),
            Removal::Archived(root) => (
                root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                "It leaves the archived list. Its files stay on disk — add the folder again to bring it back.",
            ),
        };
        let card = self
            .dialog_card(hx, format!("Remove ‘{name}’?"), cx)
            .child(div().text_sm().text_color(hx.tx1).child(body))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_1()
                    .child(dialog_button(hx, "remove-cancel", "Cancel", Tone::Primary).on_click(cx.listener(
                        |ws, _, _, cx| {
                            ws.confirm_removal = None;
                            cx.notify();
                        },
                    )))
                    .child(dialog_button(hx, "remove-confirm", "Remove", Tone::Plain).on_click(cx.listener(
                        move |ws, _, window, cx| {
                            ws.confirm_removal = None;
                            match &removal {
                                Removal::Project(name) => ws.remove_repo(name.clone(), window, cx),
                                Removal::Archived(root) => ws.forget_archived(root, cx),
                            }
                        },
                    ))),
            );
        self.dialog_backdrop(
            "remove-backdrop",
            card,
            |ws, _, cx| {
                ws.confirm_removal = None;
                cx.notify();
            },
            cx,
        )
    }

    /// Archive / delete confirmation, naming exactly what would be lost.
    /// Rendered only once the loss check has answered.
    pub(crate) fn render_end_confirm(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let EndConfirm { plan, losses } = self.end_confirm.clone()?;
        let losses = losses?;
        let kind = plan.kind;
        let branches: Vec<&str> = plan.members.iter().map(|m| m.branch.as_str()).collect();
        let mut branches_u = branches.clone();
        branches_u.dedup();
        let branch_list = branches_u.join(", ");
        let summary = match kind {
            EndKind::Delete => format!(
                "Removes the worktree folder{} and deletes branch {branch_list}. This can't be undone.",
                if plan.members.len() > 1 { "s" } else { "" }
            ),
            EndKind::Archive => format!(
                "Removes the worktree folder{}. Branch {branch_list} is kept, so committed work stays.",
                if plan.members.len() > 1 { "s" } else { "" }
            ),
        };

        let lost: Vec<&Loss> = losses
            .iter()
            .filter(|l| l.dirty > 0 || (kind == EndKind::Delete && l.unpushed > 0))
            .collect();
        let mut details = div().flex().flex_col().gap_1();
        if lost.is_empty() {
            details = details.child(
                div()
                    .text_sm()
                    .text_color(hx.green)
                    .child("Nothing unsaved: every change is committed and pushed."),
            );
        } else {
            details = details.child(div().text_sm().text_color(hx.tx1).child("This will be lost:"));
            for l in lost {
                let mut parts = Vec::new();
                if l.dirty > 0 {
                    parts.push(format!("{} uncommitted file{}", l.dirty, if l.dirty == 1 { "" } else { "s" }));
                }
                if kind == EndKind::Delete && l.unpushed > 0 {
                    parts.push(format!(
                        "{} commit{} not on any remote",
                        l.unpushed,
                        if l.unpushed == 1 { "" } else { "s" }
                    ));
                }
                details = details.child(
                    div()
                        .flex()
                        .gap_2()
                        .text_sm()
                        .child(div().text_color(hx.red).child("•"))
                        .child(div().font_family(hx.font_mono).text_color(hx.tx0).child(l.repo.clone()))
                        .child(div().text_color(hx.red).child(parts.join(", "))),
                );
            }
        }

        let card = self
            .dialog_card(hx, format!("{} ‘{}’?", kind.verb(), plan.title), cx)
            .child(div().text_sm().text_color(hx.tx1).child(summary))
            .child(details)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_1()
                    .child(
                        dialog_button(hx, "end-cancel", "Cancel", Tone::Primary)
                            .on_click(cx.listener(|ws, _, _, cx| ws.cancel_end(cx))),
                    )
                    .child(dialog_button(hx, "end-confirm", kind.verb(), Tone::Danger).on_click(cx.listener(
                        move |ws, _, window, cx| ws.start_end(plan.clone(), window, cx),
                    ))),
            );
        Some(self.dialog_backdrop("end-backdrop", card, |ws, _, cx| ws.cancel_end(cx), cx))
    }

    /// "Run this project's setup scripts?" — committed hooks arrive with the
    /// repository, so they run only after someone has looked at them.
    pub(crate) fn render_hook_trust(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prompt = self.hook_trust.clone()?;
        let names: Vec<String> = prompt.repos.iter().map(|(n, _, _)| n.clone()).collect();
        let roots: Vec<PathBuf> = prompt.repos.iter().map(|(_, r, _)| r.clone()).collect();

        let mut list = div().flex().flex_col().gap_1();
        for (name, root, scripts) in &prompt.repos {
            list = list.child(div().text_sm().font_family(hx.font_mono).text_color(hx.tx0).child(name.clone()));
            for s in scripts {
                let shown = s.strip_prefix(root).unwrap_or(s).to_string_lossy().into_owned();
                list = list.child(
                    div()
                        .pl_3()
                        .text_xs()
                        .font_family(hx.font_mono)
                        .text_color(hx.tx1)
                        .child(shown),
                );
            }
        }

        let card = self
            .dialog_card(hx, format!("Run setup scripts from {}?", names.join(", ")), cx)
            .child(div().text_sm().text_color(hx.tx1).child(
                "These scripts are committed in the repository and run on your machine with your permissions. \
                 Only allow them if you trust where this project came from. You'll be asked again if they change.",
            ))
            .child(list)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_1()
                    .child(dialog_button(hx, "trust-skip", "Don't run", Tone::Primary).on_click(cx.listener(
                        |ws, _, _, cx| {
                            ws.hook_trust = None;
                            ws.set_note("setup skipped: hooks not approved".into());
                            cx.notify();
                        },
                    )))
                    .child(dialog_button(hx, "trust-run", "Trust and run", Tone::Plain).on_click(cx.listener(
                        move |ws, _, window, cx| ws.trust_hooks_and_run(roots.clone(), window, cx),
                    ))),
            );
        Some(self.dialog_backdrop(
            "trust-backdrop",
            card,
            |ws, _, cx| {
                ws.hook_trust = None;
                ws.set_note("setup skipped: hooks not approved".into());
                cx.notify();
            },
            cx,
        ))
    }
}
