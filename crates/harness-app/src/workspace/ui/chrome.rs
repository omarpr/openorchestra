//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::super::*;

/// The top bar's height. The traffic lights' y offset in `main.rs` (12px)
/// centres their 14px row on it.
const TOP_BAR_H: f32 = 38.0;

/// Room left of the top bar's content for the macOS traffic lights: the
/// buttons plus the window's 1px border, matching zed's `TRAFFIC_LIGHT_PADDING`
/// for current SDKs.
const TRAFFIC_LIGHT_PADDING: f32 = 80.0;

impl HarnessWorkspace {
    /// The top bar, which is also the window's title bar (the native one is
    /// transparent — see `main.rs`): it leaves room for the traffic lights,
    /// drags the window, and zooms it on double-click, as a native bar would.
    pub(crate) fn render_top_bar(
        &self,
        hx: &HarnessTheme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Fullscreen hides the traffic lights, so their padding goes too.
        let traffic_light_room = if window.is_fullscreen() {
            12.0
        } else {
            TRAFFIC_LIGHT_PADDING
        };
        let r = self.resolve();
        let branch = r.as_ref().map(|r| r.branch.clone()).unwrap_or_default();
        let (project, task) = self.location_names();
        // The coordinator has no repo or branch of its own: the bar names
        // the task and says who you are talking to.
        let coordinator_of = self
            .focused_coordinator_task()
            .and_then(|tid| self.task_store.get(&tid).map(|t| t.name.clone()));
        // The left toggle shows/hides the whole left panel (stats + worktrees),
        // so it sits on the left and is named for the panel, not its list.
        let sidebar_toggle = div()
            .id("toggle-sidebar")
            .flex_none()
            .px_2()
            .py_0p5()
            .rounded_md()
            .text_xs()
            .text_color(if self.prefs.sidebar_open {
                hx.tx1
            } else {
                hx.tx2
            })
            .cursor_pointer()
            .tooltip(tip("Toggle sidebar (⌘B)"))
            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
            .child("▤ sidebar")
            // Stop here so a double-click on the toggle doesn't also zoom the
            // window through the bar's title-bar handler.
            .on_click(cx.listener(|ws, _, _, cx| {
                cx.stop_propagation();
                ws.toggle_sidebar(cx);
            }));

        // Tools panel visibility. Always available so Files/Git stay reachable
        // even when Review lives in a center tab.
        let tools_toggle = div()
            .id("toggle-diff")
            .flex_none()
            .px_2()
            .py_0p5()
            .rounded_md()
            .text_xs()
            .text_color(if self.prefs.diff_open { hx.tx1 } else { hx.tx2 })
            .cursor_pointer()
            .tooltip(tip("Toggle tools panel: Files / Git / Review (⌘⇧B)"))
            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
            .child("tools ▤")
            .on_click(cx.listener(|ws, _, window, cx| {
                cx.stop_propagation();
                ws.toggle_diff(window, cx);
            }));

        div()
            .id("top-bar")
            .window_control_area(gpui::WindowControlArea::Drag)
            .flex()
            .flex_none()
            .items_center()
            .h(px(TOP_BAR_H))
            .pl(px(traffic_light_room))
            .pr_3()
            .bg(hx.bg0)
            .border_b_1()
            .border_color(hx.border)
            // Zed's title bar does the same: arm on press, move on the first
            // drag motion, disarm on release. Starting the move on press would
            // swallow clicks meant for the toggles.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseDownEvent, _, _| {
                    ws.titlebar_should_move = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseUpEvent, _, _| {
                    ws.titlebar_should_move = false;
                }),
            )
            .on_mouse_down_out(cx.listener(|ws, _: &MouseDownEvent, _, _| {
                ws.titlebar_should_move = false;
            }))
            .on_mouse_move(cx.listener(|ws, _: &MouseMoveEvent, window, _| {
                if ws.titlebar_should_move {
                    ws.titlebar_should_move = false;
                    window.start_window_move();
                }
            }))
            .on_click(|ev, window, _| {
                if ev.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            // Left: sidebar toggle. Center: title. Right: tools panel toggle.
            .child(sidebar_toggle)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .min_w_0()
                    // Where you are, most specific last: project › task, then
                    // the branch. The app's own name was here before, and
                    // the window title already says that.
                    .when_some(coordinator_of.clone(), |d, name| {
                        d.child(
                            div()
                                .min_w_0()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(hx.tx0)
                                .child(name),
                        )
                        .child(div().flex_none().text_xs().text_color(hx.tx2).child("·"))
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(hx.accent)
                                .child("coordinator"),
                        )
                    })
                    .when(coordinator_of.is_none(), |d| {
                        d.child(
                            div()
                                .flex_none()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(hx.tx0)
                                .child(project),
                        )
                    })
                    .when_some(task.filter(|_| coordinator_of.is_none()), |d, task| {
                        d.child(div().flex_none().text_sm().text_color(hx.tx2).child("›"))
                            .child(
                                div()
                                    .min_w_0()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_sm()
                                    .text_color(hx.tx0)
                                    .child(task),
                            )
                    })
                    .when(coordinator_of.is_none(), |d| {
                        d.child(div().flex_none().text_xs().text_color(hx.tx2).child("·"))
                            .child(
                                div()
                                    .min_w_0()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_xs()
                                    .font_family(hx.font_mono)
                                    .text_color(hx.tx1)
                                    .child(branch),
                            )
                    })
                    .children(self.render_a2a_delegation_chip(hx))
                    .children(self.render_remote_chip(hx, cx)),
            )
            .child(tools_toggle)
    }

    /// Remote control for the current tab: a chip that shows whether paired
    /// phones can reach it and toggles that. Terminal tabs only.
    fn render_remote_chip(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::workspace::remote::{
            ChipClick, RemoteChip, RemoteServe, chip_click, mark_note, remote_chip,
        };
        let key = self.resolve()?.key;
        let tab = self.panes.get(&key)?.active_tab()?;
        let tab_id = tab.id;
        let is_shell = matches!(Self::remote_tab_kind(tab), Some(TabKind::Shell));
        let marked = self.remote_is_marked(&key, tab_id);
        let state = remote_chip(
            Self::remote_tab_kind(tab).is_some(),
            marked,
            self.remote_live(),
        )?;
        let (fg, bg, border, tooltip) = match state {
            RemoteChip::Off if !self.remote.file.enabled => (
                hx.tx2,
                None,
                hx.border,
                "Remote control is off \u{2014} click to turn it on in Settings",
            ),
            RemoteChip::Off => (
                hx.tx2,
                None,
                hx.border,
                "Let paired devices see and type into this tab",
            ),
            RemoteChip::OnLive => (
                hx.accent,
                Some(tint(hx.accent, 0.16)),
                tint(hx.accent, 0.16),
                "Paired devices can see and type into this tab \u{2014} click to stop",
            ),
            RemoteChip::OnUnreachable => (
                hx.amber,
                Some(tint(hx.amber, 0.16)),
                tint(hx.amber, 0.16),
                "Remote control is off \u{2014} click to turn it on in Settings",
            ),
        };
        // A device typing into this tab right now: say who, on the chip.
        let typing = (state == RemoteChip::OnLive)
            .then(|| self.remote_typing(&crate::tmux::session_name(&key, tab_id)))
            .flatten();
        let label = match (state, &typing) {
            (RemoteChip::OnLive, Some(device)) => format!("{device} typing\u{2026}"),
            (RemoteChip::Off, _) => "Remote off".to_string(),
            (RemoteChip::OnLive, None) => "Remote on".to_string(),
            (RemoteChip::OnUnreachable, _) => "Remote paused".to_string(),
        };
        let dot_alpha = self.remote_dot_opacity();
        Some(
            div()
                .id("top-remote-chip")
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .h(px(20.0))
                .px_1p5()
                .rounded_md()
                .border_1()
                .border_color(border)
                .when_some(bg, |d, bg| d.bg(bg))
                .text_xs()
                .text_color(fg)
                .cursor_pointer()
                .tooltip(tip(tooltip))
                .hover(|d| d.bg(hx.surface_hover))
                .when(state == RemoteChip::OnLive, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .size(px(6.0))
                            .rounded_full()
                            .bg(hx.accent)
                            .opacity(dot_alpha),
                    )
                })
                .when(state == RemoteChip::Off, |d| {
                    d.child(
                        gpui::svg()
                            .size(px(12.0))
                            .flex_none()
                            .path("icons/oo/devices.svg")
                            .text_color(fg),
                    )
                })
                .child(label)
                // Stop here so the title bar's drag / double-click zoom
                // doesn't fire from the chip.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
                )
                .on_click(cx.listener(move |ws, _, window, cx| {
                    cx.stop_propagation();
                    let key = ws.resolve().map(|r| r.key).unwrap_or_default();
                    let marked = ws.remote_is_marked(&key, tab_id);
                    if let ChipClick::OpenSettings { mark } =
                        chip_click(ws.remote.file.enabled, marked)
                    {
                        if mark {
                            ws.remote_toggle_mark(&key, tab_id, cx);
                        }
                        ws.open_settings(SettingsSection::Remote, window, cx);
                        ws.set_note(
                            "Turn on Remote control to reach this tab from your devices"
                                .to_string(),
                        );
                        cx.notify();
                        return;
                    }
                    ws.remote_toggle_mark(&key, tab_id, cx);
                    let now = ws.remote_is_marked(&key, tab_id);
                    let other = ws.remote.serve == RemoteServe::OtherBuild;
                    if let Some(n) = mark_note(ws.remote.file.enabled, other, now) {
                        ws.set_note(n.to_string());
                    } else if now && is_shell {
                        ws.set_note(
                            "Remote on \u{2014} paired devices can use this shell".to_string(),
                        );
                    }
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// The task whose coordinator agent is focused, if any: the same test
    /// `ensure_pane` uses (the resolved pane is the coordinator's), so a stale
    /// member id that fell back to the coordinator counts too.
    pub(crate) fn focused_coordinator_task(&self) -> Option<String> {
        let r = self.resolve()?;
        let tid = r.task_id?;
        (r.key == a2a_broker::coordinator_pane_key(&tid)).then_some(tid)
    }

    /// Compact A2A delegation status for the active multi-member task.
    fn render_a2a_delegation_chip(&self, hx: &HarnessTheme) -> Option<AnyElement> {
        let rows = self.a2a_delegation_summary();
        if rows.is_empty() {
            let r = self.resolve()?;
            let tid = r.task_id.as_ref()?;
            if !self.task_brokers.contains_key(tid) {
                return None;
            }
            return Some(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1p5()
                    .py_0p5()
                    .rounded_md()
                    .bg(hx.surface)
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("A2A")
                    .into_any_element(),
            );
        }
        let working = rows
            .iter()
            .filter(|(_, _, s)| *s == "working" || *s == "submitted")
            .count();
        let done = rows.iter().filter(|(_, _, s)| *s == "completed").count();
        let label = format!("A2A {working} active · {done} done");
        Some(
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_1p5()
                .py_0p5()
                .rounded_md()
                .bg(hx.surface)
                .text_xs()
                .font_family(hx.font_mono)
                .text_color(if working > 0 { hx.amber } else { hx.tx2 })
                .child(label)
                .into_any_element(),
        )
    }

    /// The selected project's name and, for a task, the task's name (with the
    /// focused member's project when the task spans several).
    pub(crate) fn location_names(&self) -> (String, Option<String>) {
        match &self.selection {
            Selection::Main(repo) if !repo.is_empty() => (repo.clone(), None),
            Selection::Main(_) => ("No project".into(), None),
            Selection::Task { task_id, member_id } => {
                let Some(task) = self.task_store.get(task_id) else {
                    return ("No project".into(), None);
                };
                let member = member_id
                    .as_deref()
                    .and_then(|id| task.members.iter().find(|m| m.worktree_id == id))
                    .or_else(|| task.home_member());
                let project = member.map(|m| m.repo.clone()).unwrap_or_default();
                (project, Some(task.name.clone()))
            }
            Selection::Worktree(id) => match self.manager.registry.get(id) {
                Some(e) => (e.repo.clone(), Some(e.name.clone())),
                None => ("No project".into(), None),
            },
        }
    }
}
