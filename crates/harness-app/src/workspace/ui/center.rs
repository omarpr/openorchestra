//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::super::*;
use crate::workspace::remote::{mark_note, remote_menu_item};

/// The label that follows the cursor while a tab (or project) is being dragged.
pub(crate) struct DragGhost {
    pub(crate) label: String,
    pub(crate) hx: HarnessTheme,
}

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_0p5()
            .rounded_md()
            .bg(self.hx.surface2)
            .border_1()
            .border_color(self.hx.accent)
            .text_xs()
            .font_family(self.hx.font_mono)
            .text_color(self.hx.tx0)
            .child(self.label.clone())
    }
}

impl HarnessWorkspace {

    pub(crate) fn render_center(&self, hx: &HarnessTheme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if crate::probe_paint() {
            log::info!(
                "paint: center selection={:?} pane={:?}",
                self.selection,
                self.resolve().map(|r| {
                    let p = self.panes.get(&r.key);
                    (r.key.clone(), p.map(|p| p.tabs.len()), p.and_then(|p| p.active_tab()).map(|t| t.title.clone()))
                }),
            );
        }
        let Some(r) = self.resolve() else {
            return self.render_empty_center(hx, cx).into_any_element();
        };
        let pane = self.panes.get(&r.key);

        let content: AnyElement = match pane.and_then(|p| p.active_tab()) {
            Some(tab) => match &tab.content {
                TabContent::Terminal(t) => t.clone().into_any_element(),
                // The full editor. Its own actions are attached here (zed v1.2x
                // moved that out of `Workspace::render`), and it carries both
                // zed's `Workspace` key context and our `IdeWorkspace` one, so
                // the IDE-only dock shortcuts apply here and nowhere else.
                TabContent::Editor(ws) => div()
                    .key_context("IdeWorkspace")
                    .flex()
                    .size_full()
                    .child(
                        ws.update(cx, |workspace, cx| workspace.actions(div(), window, cx))
                            .key_context("Workspace")
                            .flex()
                            .size_full()
                            .child(ws.clone()),
                    )
                    .into_any_element(),
                TabContent::File { .. } => {
                    let Some(ws) = self.editor_workspace() else {
                        return div()
                            .flex()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .text_color(hx.tx2)
                            .child("Open a file from Files or Git")
                            .into_any_element();
                    };
                    if crate::probe_paint() {
                        log::info!("paint: center renders File tab");
                    }
                    let editor = ws
                        .update(cx, |workspace, cx| workspace.actions(div(), window, cx))
                        .key_context("Workspace")
                        .track_focus(&self.editor_host_focus)
                        .flex()
                        .size_full()
                        .child(ws.clone());
                    if self.hooks_edit.is_some() {
                        div()
                            .flex()
                            .flex_col()
                            .size_full()
                            .child(self.render_hooks_bar(hx, cx))
                            .child(div().flex_1().min_h_0().child(editor))
                            .into_any_element()
                    } else {
                        editor.into_any_element()
                    }
                }
                TabContent::Chat(c) => c.clone().into_any_element(),
                TabContent::Diff => self.render_diff_surface(hx, None, true, cx),
            },
            None => div().into_any_element(),
        };

        // The tabs scroll; the `+` does not. The scrolling row used to have no
        // width limit, so it grew to fit every tab and spilled under the tools
        // panel — the last tabs, and `+`, became unreachable. It now shrinks
        // (`min_w_0`) but never grows: `+` sits directly after the last tab,
        // and once the tabs overflow they scroll while `+` stays in view.
        let mut tabs = div()
            .id("tab-strip")
            .flex()
            .flex_shrink(1.0)
            .min_w_0()
            .items_center()
            .gap_1()
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll);
        if let Some(p) = pane {
            for tab in &p.tabs {
                tabs = tabs.child(
                    div().flex_none().child(self.render_tab(hx, tab, tab.id == p.active, cx)),
                );
            }
            // Bring a newly active tab into view (only on change).
            if p.active != self.tab_scrolled_to.get()
                && let Some(ix) = p.tabs.iter().position(|t| t.id == p.active)
            {
                self.tab_scroll.scroll_to_item(ix);
                self.tab_scrolled_to.set(p.active);
            }
        }
        let strip = div()
            .flex()
            .flex_none()
            .w_full()
            .min_w_0()
            .items_center()
            .h(px(34.0))
            .px_2()
            .gap_1()
            .bg(hx.bg1)
            .border_b_1()
            .border_color(hx.border)
            .child(tabs)
            .child(div().flex_none().child(self.render_new_tab_button(hx, cx)));

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .bg(hx.bg0)
            .child(strip)
            .child(div().flex_1().h_full().min_h_0().child(content))
            .into_any_element()
    }

    /// Chrome above the editor when editing a project hook script: pick
    /// Repo vs Local and Setup vs Teardown without exposing TOML.
    fn render_hooks_bar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((repo, layer, phase)) = self.hooks_edit.clone() else {
            return div();
        };
        let chip = |id: &'static str,
                    label: &'static str,
                    active: bool,
                    repo: String,
                    next_phase: HookScriptPhase,
                    next_layer: HookLayer| {
            div()
                .id(id)
                .px_1p5()
                .py_0p5()
                .rounded_md()
                .text_xs()
                .text_color(if active { hx.tx0 } else { hx.tx2 })
                .bg(if active { hx.surface } else { hx.bg1 })
                .border_1()
                .border_color(if active { hx.accent } else { hx.border })
                .cursor_pointer()
                .hover(|d| d.bg(hx.surface_hover))
                .child(label)
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.open_project_hooks(&repo, next_phase, next_layer, window, cx);
                }))
        };

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(32.0))
            .px_3()
            .bg(hx.bg1)
            .border_b_1()
            .border_color(hx.border)
            .child(
                div()
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(hx.tx2)
                    .child(format!("{repo} hooks")),
            )
            .child(div().flex_1())
            .child(chip(
                "hooks-repo",
                "Repo",
                layer == HookLayer::Repo,
                repo.clone(),
                phase,
                HookLayer::Repo,
            ))
            .child(chip(
                "hooks-local",
                "Local",
                layer == HookLayer::Local,
                repo.clone(),
                phase,
                HookLayer::Local,
            ))
            .child(
                div()
                    .w(px(1.0))
                    .h(px(14.0))
                    .bg(hx.border),
            )
            .child(chip(
                "hooks-setup",
                "Setup",
                phase == HookScriptPhase::Setup,
                repo.clone(),
                HookScriptPhase::Setup,
                layer,
            ))
            .child(chip(
                "hooks-teardown",
                "Teardown",
                phase == HookScriptPhase::Teardown,
                repo.clone(),
                HookScriptPhase::Teardown,
                layer,
            ))
            .when(layer == HookLayer::Local, |d| {
                let clear_repo = repo.clone();
                d.child(
                    div()
                        .id("hooks-clear-local")
                        .px_1p5()
                        .py_0p5()
                        .rounded_md()
                        .text_xs()
                        .text_color(hx.red)
                        .border_1()
                        .border_color(hx.border)
                        .cursor_pointer()
                        .tooltip(tip("Remove local override (back to repo scripts)"))
                        .hover(|h| h.bg(hx.surface_hover))
                        .child("Clear local")
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.clear_project_local_hooks(&clear_repo, window, cx);
                        })),
                )
            })
            .child(
                div()
                    .id("hooks-done")
                    .ml_1()
                    .px_1p5()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .text_color(hx.tx1)
                    .border_1()
                    .border_color(hx.border)
                    .cursor_pointer()
                    .tooltip(tip("Leave hooks editor and restore the project file tree"))
                    .hover(|h| h.bg(hx.surface_hover))
                    .child("Done")
                    .on_click(cx.listener(|ws, _, window, cx| {
                        ws.exit_hooks_edit(window, cx);
                    })),
            )
    }

    pub(crate) fn render_empty_center(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(hx.bg0)
            .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx1).child("No project open"))
            .child(div().text_sm().text_color(hx.tx2).child("Open a git repository to start orchestrating worktrees."))
            .child(
                div()
                    .id("empty-add")
                    .mt_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(tint(hx.accent, 0.16))
                    .text_sm()
                    .text_color(hx.accent)
                    .cursor_pointer()
                    .hover(|d| d.bg(tint(hx.accent, 0.24)))
                    .on_click(cx.listener(|ws, _, window, cx| ws.add_project(window, cx)))
                    .child("＋ Add a project"),
            )
    }

    /// One tab button, with a close affordance.
    pub(crate) fn render_tab(
        &self,
        hx: &HarnessTheme,
        tab: &Tab,
        active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = tab.id;
        let close_id = tab.id;
        let key = self.resolve().map(|r| r.key).unwrap_or_default();
        let remote_on = self.remote_is_marked(&key, tab.id);
        let remote_menu = Self::remote_tab_kind(tab).is_some();
        let drag_key = key.clone();
        let drop_key = key;
        let onto = tab.id;
        let ghost_label = tab.title.clone();
        let ghost_theme = hx.clone();
        let hover_bg = hx.surface_hover;
        let group = gpui::SharedString::from(format!("tab-{}", tab.id));
        div()
            .id(("tab", tab.id as usize))
            .group(group.clone())
            .flex()
            .items_center()
            .gap_1()
            .h(px(26.0))
            .pl_3()
            .pr_1p5()
            .rounded_md()
            // Every tab carries the 1px border, transparent unless active. Only
            // the active one had it, so switching tabs made the newly active
            // tab 2px wider and nudged every tab after it sideways — the strip
            // visibly jittered on each click.
            .border_1()
            .border_color(if active { hx.border } else { Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 } })
            .when(active, |d| d.bg(hx.bg0))
            // The UI face, like the rest of the chrome: the labels are names,
            // not code, and monospace here was the one place it stood out.
            .text_xs()
            .text_color(if active { hx.tx0 } else { hx.tx2 })
            .when(
                matches!(
                    &tab.content,
                    TabContent::File {
                        preview: true,
                        ..
                    }
                ),
                |d| d.italic(),
            )
            .when_some(tab.agent.clone(), |d, kind| {
                d.child(div().mr_1().size(px(14.0)).child(agent_icon(&kind, 13.0)))
            })
            .when(remote_menu, |d| {
                d.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        ws.tab_context_menu = Some((id, ev.position.x));
                        cx.notify();
                    }),
                )
            })
            .cursor_pointer()
            .hover(|d| d.text_color(hx.tx1))
            .on_click(cx.listener(move |ws, ev: &gpui::ClickEvent, window, cx| {
                ws.activate_tab(id, window, cx);
                // Match Zed: double-click pins a preview tab.
                if ev.click_count() > 1 {
                    ws.pin_file_tab(id, window, cx);
                }
            }))
            // The ghost that follows the cursor. gpui wants a real view here, so
            // it is a one-off entity rendering this tab's own label — a generic
            // rectangle would not say which tab is moving.
            .on_drag(DraggedTab { key: drag_key, id }, move |_, _, _, cx| {
                let label = ghost_label.clone();
                let hx = ghost_theme.clone();
                cx.new(|_| DragGhost { label, hx })
            })
            .drag_over::<DraggedTab>(move |d, _, _, _| d.bg(hover_bg))
            .on_drop(cx.listener(move |ws, dragged: &DraggedTab, _, cx| {
                // Same worktree only — see DraggedTab's note.
                if dragged.key != drop_key {
                    return;
                }
                ws.move_tab(&drop_key, dragged.id, onto);
                cx.notify();
            }))
            .when(remote_on, |d| {
                d.child(
                    div()
                        .id(("tab-remote", tab.id as usize))
                        .flex_none()
                        .size(px(6.0))
                        .mr_1()
                        .rounded_full()
                        .bg(hx.accent)
                        .tooltip(tip("Remote control on")),
                )
            })
            .child(tab.title.clone())
            .child(
                div()
                    .id(("tab-close", tab.id as usize))
                    .px_1()
                    .rounded_sm()
                    .text_color(hx.tx2)
                    .cursor_pointer()
                    .tooltip(tip("Close tab"))
                    // On the active tab, and on whichever one is hovered: an ×
                    // on every tab was as loud as the labels themselves. It is
                    // only faded, so the tab keeps its width.
                    .when(!active, |d| d.opacity(0.0).group_hover(group, |s| s.opacity(1.0)))
                    .hover(|d| d.bg(hx.surface).text_color(hx.red))
                    .child("×")
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.request_close_tab(close_id, window, cx);
                        // Otherwise the click also selects the tab being closed.
                        cx.stop_propagation();
                    })),
            )
    }

    /// The single `+` that opens the new-tab menu (§3.4).
    ///
    /// One affordance rather than one button per kind: the strip already
    /// carries the worktree's tabs, and four permanent `＋x` buttons competed
    /// with them for both space and attention.
    pub(crate) fn render_new_tab_button(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("new-tab")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(px(22.0))
            .w(px(24.0))
            .ml_1()
            .rounded_md()
            .text_xs()
            .text_color(hx.tx2)
            .cursor_pointer()
            .tooltip(tip("New tab"))
            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
            .child("＋")
            .on_click(cx.listener(|ws, ev: &gpui::ClickEvent, _, cx| {
                // Anchor under the button that was clicked, so the menu lands
                // wherever the strip has scrolled to.
                ws.tab_menu = Some(ev.position().x);
                cx.notify();
            }))
    }

    /// The inline-comment popover, anchored beside the line it belongs to.
    ///
    /// A popover rather than a panel at the bottom: a comment is about one
    /// line, and reading it fifty rows away makes you hunt for what it means.
    pub(crate) fn render_comment_popover(
        &self,
        hx: &HarnessTheme,
        draft: &CommentDraft,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        const W: f32 = 320.0;
        // Open away from the nearer edge. The dock is on the right, so a gutter
        // click there must open leftward — but in a tab the diff can sit
        // anywhere, and opening left of a left-hand click would run off-window.
        let window_width = f32::from(window.viewport_size().width);
        let anchor_x = f32::from(draft.at.x);
        let left = if anchor_x > window_width / 2.0 {
            (anchor_x - W - 14.0).max(8.0)
        } else {
            (anchor_x + 14.0).min((window_width - W - 8.0).max(8.0))
        };
        let top = (f32::from(draft.at.y) - 12.0).max(48.0);
        let existing = self
            .resolve()
            .and_then(|r| self.review.get(&r.key, &draft.file, draft.line))
            .is_some();
        let file_label = draft
            .file
            .rsplit('/')
            .next()
            .unwrap_or(&draft.file)
            .to_string();

        div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(W))
            .rounded_lg()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .shadow_lg()
            .flex()
            .flex_col()
            .overflow_hidden()
            // Typing in the popover must not count as "clicked outside".
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            // Header: what this comment is attached to.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .bg(hx.bg1)
                    .border_b_1()
                    .border_color(hx.border)
                    .child(div().text_xs().text_color(hx.amber).child("▸"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx0)
                            .overflow_hidden()
                            .child(file_label),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx2)
                            .child(format!("line {}", draft.line)),
                    ),
            )
            // Body: the text field. The editor carries its own placeholder,
            // caret, and selection; the wrapper routes confirm/cancel.
            .child(
                div()
                    .key_context("HarnessInput")
                    .on_action(cx.listener(Self::confirm_comment))
                    .on_action(cx.listener(Self::cancel_comment))
                    .min_h(px(56.0))
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(hx.tx0)
                    .child(draft.editor.clone()),
            )
            // Footer: the two things you can do, and how.
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .bg(hx.bg1)
                    .border_t_1()
                    .border_color(hx.border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(hx.tx2)
                            .child(if existing {
                                "clear the text to delete"
                            } else {
                                "goes to the agent with the review"
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .child(div().text_color(hx.tx2).child("esc"))
                            .child(div().text_color(hx.accent).child("⏎ save")),
                    ),
            )
    }

    /// The remote-controllable kind of a tab: terminals only.
    fn remote_tab_kind(tab: &Tab) -> Option<TabKind> {
        match (&tab.content, &tab.agent) {
            (TabContent::Terminal(_), Some(a)) => Some(TabKind::Agent(a.clone())),
            (TabContent::Terminal(_), None) => Some(TabKind::Shell),
            _ => None,
        }
    }

    /// The remote item for a tab in the current worktree; `None` when the tab
    /// is gone (closed, or another worktree is selected) or has no item.
    pub(crate) fn tab_context_item(
        &self,
        tab_id: u64,
    ) -> Option<(&'static str, Option<&'static str>)> {
        let key = self.resolve()?.key;
        let kind = self
            .panes
            .get(&key)?
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(Self::remote_tab_kind)?;
        remote_menu_item(&kind, self.remote_is_marked(&key, tab_id))
    }

    /// Right-click menu on a Shell/Agent tab: allow / stop remote control.
    pub(crate) fn render_tab_context_menu(
        &self,
        hx: &HarnessTheme,
        tab_id: u64,
        x: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let item = self.tab_context_item(tab_id);
        // Same container + mouse-down swallow as `render_tab_menu`: the
        // backdrop dismisses on mouse-down, which would eat the item's click.
        let mut menu = div()
            .absolute()
            .left(x)
            .top(px(72.0))
            .w(px(220.0))
            .p(px(4.0))
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .flex()
            .flex_col()
            .gap_1()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            );
        if let Some((label, warning)) = item {
            menu = menu.child(
                div()
                    .id("tabctx-remote")
                    .flex()
                    .flex_col()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .text_color(hx.tx1)
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                    .child(label)
                    .children(warning.map(|w| div().text_color(hx.tx2).child(w)))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        ws.tab_context_menu = None;
                        let key = ws.resolve().map(|r| r.key).unwrap_or_default();
                        ws.remote_toggle_mark(&key, tab_id, cx);
                        let now = ws.remote_is_marked(&key, tab_id);
                        let other =
                            ws.remote.serve == crate::workspace::remote::RemoteServe::OtherBuild;
                        if let Some(n) = mark_note(ws.remote.file.enabled, other, now) {
                            ws.set_note(n.to_string());
                        }
                        cx.notify();
                    })),
            );
        }
        menu
    }

    /// The popup listing what can be opened in this worktree.
    pub(crate) fn render_tab_menu(
        &self,
        hx: &HarnessTheme,
        x: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let agent = self.resolve().and_then(|r| r.agent_kind);
        let mut menu = div()
            .absolute()
            .left(x)
            // Just below the top bar + tab strip.
            .top(px(72.0))
            .w(px(190.0))
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
            .child(self.tab_menu_item(hx, "❯_  Terminal", "⌘T", TabKind::Shell, cx))
            .child(self.tab_menu_item(hx, "{ }  Editor", "⌘E", TabKind::Editor, cx));
        // Only in Tab mode: in Dock mode the dock IS the surface, and a tab
        // showing the same review is the duplicate the setting exists to avoid.
        if self.prefs.diff_location == DiffLocation::Tab {
            menu = menu.child(self.tab_menu_item(hx, "⑂  Diff", "", TabKind::Diff, cx));
        }
        // Every enabled agent, on any selection (D8 revised: main included) —
        // a worktree's recorded default merely decides which signal the
        // status pill trusts first.
        {
            for kind in agent_catalog(&self.prefs) {
                let disabled = self
                    .prefs
                    .agents
                    .get(kind.as_str())
                    .is_some_and(|p| p.disabled);
                if disabled {
                    continue;
                }
                menu = menu.child(self.tab_menu_item_agent(hx, &kind, cx));
            }
        }
        // The chat tab is pi's RPC surface specifically — approvals as UI,
        // session capture for training (§4.4) — so it appears only for
        // worktrees and only while pi is enabled.
        let pi_enabled = !self
            .prefs
            .agents
            .get(AgentKind::Pi.as_str())
            .is_some_and(|p| p.disabled);
        if agent.is_some() && pi_enabled {
            menu = menu.child(self.tab_menu_item(hx, "π  pi chat", "⌘⇧C", TabKind::Chat, cx));
        }
        menu
    }

    pub(crate) fn tab_menu_item(
        &self,
        hx: &HarnessTheme,
        label: &str,
        shortcut: &str,
        kind: TabKind,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(gpui::SharedString::from(format!("tabmenu-{label}")))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .text_xs()
            .text_color(hx.tx1)
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
            .child(div().font_family(hx.font_mono).child(label.to_string()))
            .child(div().text_color(hx.tx2).child(shortcut.to_string()))
            .on_click(cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.tab_menu = None;
                ws.open_tab(kind.clone(), window, cx);
            }))
    }

    /// A tab-menu item with the agent's brand mark.
    pub(crate) fn tab_menu_item_agent(
        &self,
        hx: &HarnessTheme,
        agent: &AgentKind,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = agent_display(agent);
        let id = format!("tabmenu-agent-{}", agent.as_str());
        let kind = TabKind::Agent(agent.clone());
        div()
            .id(gpui::SharedString::from(id))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .text_xs()
            .text_color(hx.tx1)
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(16.0)).child(agent_icon(agent, 15.0)))
                    .child(div().font_family(hx.font_mono).child(label)),
            )
            .on_click(cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.tab_menu = None;
                ws.open_tab(kind.clone(), window, cx);
            }))
    }
}
