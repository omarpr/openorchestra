//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::super::*;

impl HarnessWorkspace {

    pub(crate) fn render_center(&self, hx: &HarnessTheme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if std::env::var_os("HARNESS_PROBE_PAINT").is_some() {
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
                TabContent::Editor(ws) => {
                    if std::env::var_os("HARNESS_PROBE_PAINT").is_some() {
                        log::info!("paint: center renders Editor tab");
                    }
                    // `Workspace::render` draws panes and docks but attaches
                    // NONE of its action handlers — in zed, the *window root*
                    // (MultiWorkspace) calls `workspace.actions(...)` on its
                    // own wrapper and sets the "Workspace" key context there.
                    // We are that window root here, so we carry the same
                    // duties: without this wrapper every workspace action
                    // (⌘P, ⌘⇧F, dock toggles, ...) dispatches into a tree
                    // with no matching handler and no matching key context,
                    // and dies silently.
                    ws.update(cx, |workspace, cx| workspace.actions(div(), window, cx))
                        .key_context("Workspace")
                        // Make the wrapper itself focusable, so focus (and thus
                        // the "Workspace" context + these action handlers) can
                        // rest here with no file open. Clicking the empty editor
                        // area lands focus here too.
                        .track_focus(&self.editor_host_focus)
                        // `.flex()`, because gpui divs default to `display:
                        // Block`, and taffy's block layout does not resolve
                        // child percentage *heights*. zed's own window root
                        // wraps the workspace in `h_flex()` for this reason.
                        // Without it the whole hosted workspace is silently
                        // content-sized: panes look fine (they have intrinsic
                        // content), but the dock caches its panel as a
                        // childless leaf whose content height is zero — an
                        // invisible file tree with healthy state.
                        .flex()
                        .size_full()
                        .child(ws.clone())
                        .when(std::env::var_os("HARNESS_PROBE_PAINT").is_some(), |d| {
                            d.child(
                                gpui::canvas(
                                    |bounds, _, _| {
                                        log::info!("paint: editor slot bounds = {bounds:?}");
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                        })
                        .into_any_element()
                }
                TabContent::Chat(c) => c.clone().into_any_element(),
            },
            None => div().into_any_element(),
        };

        // The tab strip scrolls: a worktree can carry many tabs now, and
        // without this the `+` buttons would be pushed out of reach.
        let mut strip = div()
            .id("tab-strip")
            .flex()
            .flex_none()
            .items_center()
            .h(px(34.0))
            .px_2()
            .gap_1()
            .bg(hx.bg1)
            .border_b_1()
            .border_color(hx.border)
            .overflow_x_scroll();
        if let Some(p) = pane {
            for tab in &p.tabs {
                strip = strip.child(self.render_tab(hx, tab, tab.id == p.active, cx));
            }
        }
        strip = strip.child(self.render_new_tab_button(hx, cx));

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .bg(hx.bg0)
            .child(strip)
            .child(div().flex_1().h_full().min_h_0().child(content))
            .child(self.render_status_bar(hx, Some(&r), cx))
            .into_any_element()
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
            .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx1).child("No folder open"))
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
                    .on_click(cx.listener(|ws, _, window, cx| ws.add_folder(window, cx)))
                    .child("＋ Add a folder"),
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
        div()
            .id(("tab", tab.id as usize))
            .flex()
            .items_center()
            .gap_1()
            .h(px(26.0))
            .pl_3()
            .pr_1p5()
            .rounded_md()
            .when(active, |d| d.bg(hx.bg0).border_1().border_color(hx.border))
            .text_xs()
            .font_family(hx.font_mono)
            .text_color(if active { hx.tx0 } else { hx.tx2 })
            .when_some(tab.agent.clone(), |d, kind| {
                let (glyph, color) = agent_mark(&kind);
                d.child(
                    div()
                        .mr_1()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(color)
                        .child(glyph),
                )
            })
            .cursor_pointer()
            .hover(|d| d.text_color(hx.tx1))
            .on_click(cx.listener(move |ws, _, window, cx| {
                let Some(r) = ws.resolve() else { return };
                if let Some(pane) = ws.panes.get_mut(&r.key) {
                    pane.active = id;
                }
                ws.focus_active_pane(window, cx);
                cx.notify();
            }))
            .child(tab.title.clone())
            .child(
                div()
                    .id(("tab-close", tab.id as usize))
                    .px_1()
                    .rounded_sm()
                    .text_color(hx.tx2)
                    .cursor_pointer()
                    .tooltip(tip("Close tab"))
                    .hover(|d| d.bg(hx.surface).text_color(hx.red))
                    .child("×")
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.close_tab(close_id, window, cx);
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
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        const W: f32 = 320.0;
        // Open to the LEFT: the diff lives in the right-hand panel, so an
        // anchor rightwards would push the popover off-window.
        let left = (f32::from(draft.at.x) - W - 14.0).max(8.0);
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
                let (glyph, color) = agent_mark(&kind);
                let label = format!("{glyph}  {kind}");
                menu = menu.child(self.tab_menu_item_colored(
                    hx,
                    &label,
                    "",
                    TabKind::Agent(kind.clone()),
                    color,
                    cx,
                ));
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

    /// A tab-menu item whose leading glyph carries an agent's brand color.
    /// Monogram marks, not logos: hand-drawn approximations of trademarks
    /// would be worse than none, and real logo files can slot in later.
    pub(crate) fn tab_menu_item_colored(
        &self,
        hx: &HarnessTheme,
        label: &str,
        shortcut: &str,
        kind: TabKind,
        color: Rgba,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (glyph, rest) = label.split_once("  ").unwrap_or(("", label));
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(16.0))
                            .text_center()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(color)
                            .child(glyph.to_string()),
                    )
                    .child(div().font_family(hx.font_mono).child(rest.to_string())),
            )
            .child(div().text_color(hx.tx2).child(shortcut.to_string()))
            .on_click(cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.tab_menu = None;
                ws.open_tab(kind.clone(), window, cx);
            }))
    }
}
