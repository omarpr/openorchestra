//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::super::*;

impl HarnessWorkspace {

    /// The sidebar footer: the only way in when nothing is open, so it gets
    /// its own region rather than floating as the last item in a scrolling
    /// list.
    ///
    /// It sits in a padded, top-bordered strip because the sidebar column is
    /// `overflow_hidden` and full height — a button flush against that edge had
    /// its own border clipped off by the container.
    pub(crate) fn add_folder_button(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .p_2()
            .border_t_1()
            .border_color(hx.border)
            .child(
                div()
                    .id("add-folder")
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_1p5()
                    .h(px(30.0))
                    .rounded_md()
                    .bg(hx.surface)
                    .border_1()
                    .border_color(hx.border2)
                    .text_sm()
                    .text_color(hx.tx1)
                    .cursor_pointer()
                    // Carries a tooltip like the app's other actionable controls,
                    // naming the fuller action the short label can't.
                    .tooltip(tip("Add a git repository folder"))
                    .hover(|d| {
                        d.bg(hx.surface_hover)
                            .border_color(hx.accent)
                            .text_color(hx.tx0)
                    })
                    .on_click(cx.listener(|ws, _, window, cx| ws.add_folder(window, cx)))
                    // Icon and label as separate children so the gap is real
                    // spacing rather than a space character in the glyph run.
                    .child(div().text_color(hx.accent).child("＋"))
                    .child("Add folder"),
            )
    }

    pub(crate) fn render_sidebar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut column = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.prefs.sidebar_width))
            .h_full()
            .bg(hx.bg1)
            .border_r_1()
            .border_color(hx.border)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(34.0))
                    .px_3()
                    .border_b_1()
                    .border_color(hx.border)
                    .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx2).child("WORKTREES")),
            );

        if self.data.repos.is_empty() {
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .mt(px(40.0))
                    .px_3()
                    .child(div().text_sm().text_color(hx.tx1).child("No folders yet"))
                    .child(div().text_xs().text_color(hx.tx2).child("Open a git repository to begin.")),
            );
            // Spacer so the footer sits at the bottom here too. Without it the
            // top-bordered strip would float mid-column, reading as a divider
            // across nothing.
            return column
                .child(div().flex_1())
                .child(self.add_folder_button(hx, cx));
        }

        let mut groups = div().flex().flex_col().flex_1().overflow_hidden().pt_1();
        for repo in &self.data.repos {
            groups = groups.child(self.render_repo_group(hx, repo, cx));
        }
        column.child(groups).child(self.add_folder_button(hx, cx))
    }

    pub(crate) fn render_repo_group(
        &self,
        hx: &HarnessTheme,
        repo: &crate::data::RepoView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let collapsed = *self.collapsed.get(&repo.name).unwrap_or(&false);
        let repo_name = repo.name.clone();
        let plus_repo = repo.name.clone();
        let count = repo.worktrees.len();

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .child(
                div()
                    .id(("repo-toggle", repo.name.len()))
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        let e = ws.collapsed.entry(repo_name.clone()).or_insert(false);
                        *e = !*e;
                        cx.notify();
                    }))
                    .child(div().text_xs().text_color(hx.tx2).child(if collapsed { "▸" } else { "▾" }))
                    .child(div().flex_1().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).child(repo.name.clone()))
                    .child(div().text_xs().text_color(hx.tx2).child(format!("{count} {}", if count == 1 { "tree" } else { "trees" }))),
            )
            .child(
                div()
                    .id(("repo-plus", repo.name.len()))
                    .size(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .text_color(hx.tx1)
                    .cursor_pointer()
                    // Icon-only control: the tooltip is its accessible name, the
                    // same affordance the tab ×, steppers, etc. carry.
                    .tooltip(tip("New worktree"))
                    .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.new_target_repo = plus_repo.clone();
                        ws.begin_edit(EditKind::NewName, "", "worktree name…", window, cx);
                    }))
                    .child("+"),
            );

        let mut group = div().flex().flex_col().child(header);

        if !collapsed {
            let mut body = div()
                .flex()
                .flex_col()
                .ml(px(15.0))
                .pl_1()
                .border_l_1()
                .border_color(hx.border);
            body = body.child(self.render_main_entry(hx, repo, cx));
            if matches!(self.edit_kind(), Some(EditKind::NewName))
                && self.new_target_repo == repo.name
                && let Some(editor) = self.edit_editor() {
                    body = body
                        .child(div().mx_1().my_0p5().child(self.render_input(hx, editor, cx)))
                        // The optional syntax is invisible otherwise (M5a).
                        .child(
                            div()
                                .mx_2()
                                .mb_1()
                                .text_color(hx.tx2)
                                .text_size(px(10.0))
                                .child("name  ·  #branch  ·  @base-ref"),
                        );
                }
            for card in &repo.worktrees {
                body = body.child(self.render_card(hx, card, cx));
            }
            group = group.child(body);
        }
        group
    }

    pub(crate) fn render_main_entry(
        &self,
        hx: &HarnessTheme,
        repo: &crate::data::RepoView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selection == Selection::Main(repo.name.clone());
        let repo_name = repo.name.clone();

        // Live agent state on the main checkout — main is first-class (D8
        // revised), so an agent run on it shows the same pill a worktree does,
        // keyed on the repo root the status watcher now watches.
        let agent = self
            .agent_status
            .get(&repo.repo_root)
            .copied()
            .unwrap_or(AgentState::Unknown);

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(tint(hx.accent, 0.15))
                    .text_xs()
                    .text_color(hx.accent)
                    .child("⌂"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().text_color(hx.tx0).child("main"))
                    // The main checkout usually sits on the default branch, but
                    // it can be on a feature branch — show where HEAD actually
                    // is. Only the default branch earns the "default ·" tag; any
                    // other branch is shown by name so it never reads as master.
                    .child(div().text_xs().text_color(hx.tx2).child(if repo.head_branch == repo.default_branch {
                        format!("default · {}", repo.default_branch)
                    } else {
                        repo.head_branch.clone()
                    })),
            )
            .child(agent_pill(hx, agent));

        // Badges mirror the worktree card's, minus the PR badge: a PR is a
        // feature-branch concept, and main is the target, not a PR head. CI is
        // the default branch's checks.
        let fs = self.forge_status.get(&repo.default_branch);
        let mut badges = div().flex().items_center().gap_2().mt_1();
        if repo.ahead > 0 {
            badges = badges.child(div().text_xs().text_color(hx.green).child(format!("↑{}", repo.ahead)));
        }
        if repo.behind > 0 {
            badges = badges.child(div().text_xs().text_color(hx.amber).child(format!("↓{}", repo.behind)));
        }
        if repo.dirty > 0 {
            badges = badges.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(hx.amber)
                    .child(div().size(px(5.0)).rounded_full().bg(hx.amber))
                    .child(format!("~{}", repo.dirty)),
            );
        }
        if let Some((c, label)) = fs.and_then(|s| ci_summary(hx, &s.checks)) {
            badges = badges.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(c)
                    .child(div().size(px(5.0)).rounded_full().bg(c))
                    .child(label),
            );
        }

        div()
            .id(("main-entry", repo.name.len() + 1))
            .flex()
            .flex_col()
            .mx_1()
            .my_0p5()
            .p_2()
            .rounded_md()
            // Same accent left-border a selected worktree card gets, so "which
            // entry is selected" reads the same whether it's main or a worktree.
            .when(selected, |d| d.bg(hx.surface).border_l_1().border_color(hx.accent))
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface))
            .on_click(cx.listener(move |ws, _, window, cx| {
                ws.select(Selection::Main(repo_name.clone()), window, cx);
            }))
            .child(header)
            .child(badges)
    }

    pub(crate) fn render_card(
        &self,
        hx: &HarnessTheme,
        card: &crate::data::WorktreeView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selection == Selection::Worktree(card.id.clone());
        let id = card.id.clone();
        let renaming = matches!(self.edit_kind(), Some(EditKind::Rename(rid)) if rid == &id);
        let avatar_open = matches!(self.edit_kind(), Some(EditKind::Avatar(aid)) if aid == &id);

        let avatar_id = id.clone();
        let avatar = div()
            .id(("avatar", card.id.len()))
            .size(px(26.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(avatar_color(card.color))
            .text_xs()
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(rgb(0xffffff))
            .cursor_pointer()
            .on_click(cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                let cur = ws
                    .manager
                    .registry
                    .get(&avatar_id)
                    .map(|e| e.avatar.initials.clone())
                    .unwrap_or_default();
                ws.begin_edit(EditKind::Avatar(avatar_id.clone()), &cur, "AB", window, cx);
            }))
            .child(card.initials.clone());

        let name_el: AnyElement = if let Some(editor) = renaming.then(|| self.edit_editor()).flatten() {
            self.render_input(hx, editor, cx).into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(hx.tx0)
                .overflow_hidden()
                .child(card.name.clone())
                .into_any_element()
        };

        let agent = self.agent_status.get(&card.path).copied().unwrap_or(AgentState::Unknown);
        let header = div().flex().items_center().gap_2().child(avatar).child(name_el).child(agent_pill(hx, agent));

        let fs = self.forge_status.get(&card.branch);
        let mut badges = div().flex().items_center().gap_2().mt_1();
        if let Some(pr) = fs.and_then(|s| s.pr.as_ref()) {
            let c = match pr.state {
                PrState::Open => hx.green,
                PrState::Draft => hx.tx1,
                PrState::Merged => rgb(0xa877f5),
                PrState::Closed => hx.red,
            };
            badges = badges.child(div().text_xs().font_family(hx.font_mono).text_color(c).child(format!("#{}", pr.number)));
        }
        if card.ahead > 0 {
            badges = badges.child(div().text_xs().text_color(hx.green).child(format!("↑{}", card.ahead)));
        }
        if card.behind > 0 {
            badges = badges.child(div().text_xs().text_color(hx.amber).child(format!("↓{}", card.behind)));
        }
        if card.dirty > 0 {
            badges = badges.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(hx.amber)
                    .child(div().size(px(5.0)).rounded_full().bg(hx.amber))
                    .child(format!("~{}", card.dirty)),
            );
        }
        if let Some((c, label)) = fs.and_then(|s| ci_summary(hx, &s.checks)) {
            badges = badges.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(c)
                    .child(div().size(px(5.0)).rounded_full().bg(c))
                    .child(label),
            );
        }

        let mut card_div = div()
            .id(("card", card.id.len() + 7))
            .relative()
            .flex()
            .flex_col()
            .mx_1()
            .my_0p5()
            .p_2()
            .rounded_md()
            .when(selected, |d| d.bg(hx.surface).border_l_1().border_color(hx.accent))
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface))
            .on_click(cx.listener({
                let id = id.clone();
                move |ws, _, window, cx| ws.select(Selection::Worktree(id.clone()), window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let id = id.clone();
                    move |ws, ev: &MouseDownEvent, _window, cx| {
                        eprintln!("[diag] card on_mouse_down(Right) fired");
                        cx.stop_propagation();
                        ws.pending_delete = None;
                        ws.menu = Some(CardMenu {
                            id: id.clone(),
                            x: ev.position.x,
                            y: ev.position.y,
                        });
                        cx.notify();
                    }
                }),
            )
            // GPUI routes non-primary (right/middle) *clicks* through the
            // aux-click path (div.rs special-cases `has_aux_click_listeners`),
            // which is distinct from the raw right on_mouse_down listener above.
            // Wiring both makes the menu robust to whichever path the platform
            // actually delivers.
            .on_aux_click(cx.listener({
                let id = id.clone();
                move |ws, ev: &gpui::ClickEvent, _window, cx| {
                    eprintln!("[diag] card on_aux_click fired");
                    cx.stop_propagation();
                    ws.pending_delete = None;
                    let pos = ev.position();
                    ws.menu = Some(CardMenu { id: id.clone(), x: pos.x, y: pos.y });
                    cx.notify();
                }
            }))
            .child(header)
            .child(div().mt_1().text_xs().font_family(hx.font_mono).text_color(hx.tx1).overflow_hidden().child(card.branch.clone()))
            .child(badges);

        if avatar_open {
            card_div = card_div.child(self.render_avatar_popover(hx, &id, cx));
        }
        card_div
    }

    pub(crate) fn render_avatar_popover(
        &self,
        hx: &HarnessTheme,
        id: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut swatches = div().flex().flex_wrap().gap_1();
        for ix in 0..8u8 {
            let sid = id.to_string();
            swatches = swatches.child(
                div()
                    .id(("swatch", ix as usize))
                    .size(px(22.0))
                    .rounded_md()
                    .bg(avatar_color(ix))
                    .cursor_pointer()
                    .hover(|d| d.border_1().border_color(hx.tx0))
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.set_avatar_color(&sid, ix, window, cx);
                    })),
            );
        }
        div()
            .absolute()
            .top(px(40.0))
            .left(px(8.0))
            .w(px(212.0))
            .p_2()
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .child(div().text_xs().text_color(hx.tx2).mb_1().child("INITIALS"))
            .children(self.edit_editor().map(|editor| self.render_input(hx, editor, cx)))
            .child(div().text_xs().text_color(hx.tx2).mt_2().mb_1().child("COLOR"))
            .child(swatches)
    }

    /// The chrome around an inline edit's `Editor`. The wrapper carries the
    /// `HarnessInput` key context (so escape→`menu::Cancel` outranks the
    /// editor's clear-selection escape) and the confirm/cancel action handlers;
    /// the editor itself owns caret, selection, and clipboard.
    pub(crate) fn render_input(
        &self,
        hx: &HarnessTheme,
        editor: Entity<Editor>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .key_context("HarnessInput")
            .on_action(cx.listener(Self::confirm_edit))
            .on_action(cx.listener(Self::cancel_edit))
            .flex()
            .items_center()
            .flex_1()
            .min_w_0()
            .h(px(22.0))
            .px_1p5()
            .rounded_md()
            .bg(hx.bg0)
            .border_1()
            .border_color(hx.accent)
            .text_sm()
            .text_color(hx.tx0)
            .child(editor)
    }

    pub(crate) fn render_context_menu(&self, hx: &HarnessTheme, menu: &CardMenu, cx: &mut Context<Self>) -> impl IntoElement {
        let confirming = self.pending_delete.as_deref() == Some(menu.id.as_str());
        let rename_id = menu.id.clone();
        let archive_id = menu.id.clone();
        let del_id = menu.id.clone();

        div()
            .absolute()
            .left(menu.x)
            .top(menu.y)
            .w(px(170.0))
            .p(px(4.0))
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .flex()
            .flex_col()
            .gap_1()
            // Swallow mouse-DOWN inside the menu. The dismiss backdrop listens
            // on mouse-down, and a click is down-then-up: without this the
            // backdrop closed the menu on the way down and the item's on_click
            // never fired on the way up. That is why ＋ → Editor did nothing.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .child(self.menu_item(hx, "Rename", false, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                let name = ws.manager.registry.get(&rename_id).map(|e| e.name.clone()).unwrap_or_default();
                ws.select(Selection::Worktree(rename_id.clone()), window, cx);
                ws.begin_edit(EditKind::Rename(rename_id.clone()), &name, "name…", window, cx);
            })))
            .child(self.menu_item(hx, "Archive", false, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.archive(archive_id.clone(), window, cx);
            })))
            .child(self.menu_item(hx, if confirming { "Confirm delete?" } else { "Delete" }, true, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                if ws.pending_delete.as_deref() == Some(del_id.as_str()) {
                    ws.delete(del_id.clone(), window, cx);
                } else {
                    ws.pending_delete = Some(del_id.clone());
                    cx.notify();
                }
            })))
    }

    pub(crate) fn menu_item(
        &self,
        hx: &HarnessTheme,
        label: &str,
        danger: bool,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(("menu-item", label.len()))
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .text_color(if danger { hx.red } else { hx.tx1 })
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface_hover).text_color(if danger { hx.red } else { hx.tx0 }))
            .on_click(handler)
            .child(label.to_string())
    }
}
