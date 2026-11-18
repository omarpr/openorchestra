//! The tools panel's Ports tab: listening TCP ports for the selected task, and
//! (collapsed) the rest of the Mac's. Data comes from `crate::ports`; this
//! file decides which sessions count as "this task" and draws the rows.

use super::super::*;
use crate::ports::{self, PortRow, PortSort, PortsView, Scope, SortField, SortOrder};
use super::sidebar::SidebarSection;

/// The two sort dropdowns in the Ports tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SortMenu {
    Field,
    Order,
}

impl HarnessWorkspace {
    /// Whether the Ports tab is on screen, which is the only time it samples.
    pub(crate) fn ports_visible(&self) -> bool {
        self.prefs.diff_open && self.prefs.tool_tab == ToolTab::Ports
    }

    /// The tmux session prefixes whose processes count as "this task", using
    /// the same keys the footprint monitor attributes by (`apply_resource`),
    /// plus each one's folder so Compose containers started there count too.
    pub(crate) fn ports_scopes(&self) -> Vec<Scope> {
        let scope = |label: &str, key: &str, path: Option<&std::path::Path>, repo: Option<&str>| Scope {
            label: label.to_string(),
            session_prefix: crate::tmux::session_prefix(key),
            path: path.map(|p| p.to_path_buf()),
            repo: repo.map(str::to_string),
        };
        let task_scopes = |task_id: &str| -> Vec<Scope> {
            let Some(task) = self.task_store.get(task_id) else {
                return Vec::new();
            };
            let mut out: Vec<Scope> = task
                .members
                .iter()
                .map(|m| {
                    let key = a2a_broker::task_member_pane_key(&task.id, &m.worktree_id);
                    scope(&m.repo, &key, Some(&m.path), Some(&m.repo))
                })
                .collect();
            if super::super::coordinator::has_coordinator(task) {
                out.push(scope("coordinator", &a2a_broker::coordinator_pane_key(&task.id), None, None));
            }
            out
        };
        match &self.selection {
            Selection::Main(repo) => {
                let root = self.data.repos.iter().find(|r| &r.name == repo).map(|r| r.repo_root.as_path());
                vec![scope(repo, &format!("main:{repo}"), root, Some(repo))]
            }
            Selection::Task { task_id, .. } => task_scopes(task_id),
            Selection::Worktree(id) => match self
                .manager
                .registry
                .get(id)
                .and_then(|e| e.task_id.clone())
            {
                Some(tid) => task_scopes(&tid),
                None => {
                    let found = self.data.repos.iter().find_map(|r| {
                        r.worktrees.iter().find(|w| &w.id == id).map(|w| (r, w))
                    });
                    let path = found.map(|(_, w)| w.path.as_path());
                    let repo = found.map(|(r, _)| r.name.as_str());
                    vec![scope(id, id, path, repo)]
                }
            },
        }
    }

    /// The current sample classified against the current selection. `None`
    /// before the first sample lands.
    fn ports_view(&self) -> Option<PortsView> {
        let sample = self.ports_sample.as_ref()?;
        let mut view = ports::classify(sample, &self.ports_scopes());
        ports::sort_rows(&mut view.task, self.prefs.ports_sort);
        ports::sort_rows(&mut view.other, self.prefs.ports_sort);
        Some(view)
    }

    /// "Sort [Port ▾] [Ascending ▾]" above the lists.
    fn render_ports_sort_bar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let sort = self.prefs.ports_sort;
        let field_items: Vec<(&'static str, bool, PortSort)> = SortField::ALL
            .iter()
            .map(|&f| (f.label(), f == sort.field, PortSort { field: f, ..sort }))
            .collect();
        let order_items: Vec<(&'static str, bool, PortSort)> = SortOrder::ALL
            .iter()
            .map(|&o| (o.label(), o == sort.order, PortSort { order: o, ..sort }))
            .collect();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_3()
            .py_1()
            // No bottom rule: THIS TASK's header draws the line below.
            .child(div().text_xs().text_color(hx.tx2).mr_1().child("Sort"))
            .child(self.render_sort_dropdown(hx, SortMenu::Field, sort.field.label(), field_items, cx))
            .child(self.render_sort_dropdown(hx, SortMenu::Order, sort.order.label(), order_items, cx))
            .into_any_element()
    }

    /// One dropdown: a button showing the current choice, and when open a
    /// floating list of choices below it.
    fn render_sort_dropdown(
        &self,
        hx: &HarnessTheme,
        menu: SortMenu,
        current: &'static str,
        items: Vec<(&'static str, bool, PortSort)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.ports_sort_menu == Some(menu);
        let button = div()
            .id(gpui::SharedString::from(format!("ports-sort-{menu:?}")))
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py(px(2.0))
            .rounded_md()
            .border_1()
            .border_color(hx.border)
            .text_xs()
            .text_color(hx.tx0)
            .cursor_pointer()
            .when(open, |d| d.bg(hx.surface2))
            .hover(|d| d.bg(hx.surface))
            .child(current)
            .child(div().text_color(hx.tx2).child("▾"))
            // Capture phase: runs before the open menu's outside handler for
            // this same press, so that handler can leave it to the button.
            .capture_any_mouse_down(cx.listener(move |ws, ev: &MouseDownEvent, _, _| {
                ws.ports_sort_trigger_press = Some((menu, ev.position));
            }))
            .on_click(cx.listener(move |ws, _, _, cx| {
                ws.ports_sort_menu = (!open).then_some(menu);
                cx.notify();
            }));
        let mut col = div().flex().flex_col().child(button);
        if open {
            let mut list = div()
                .mt_1()
                .min_w(px(120.0))
                .p(px(4.0))
                .rounded_md()
                .bg(hx.surface2)
                .border_1()
                .border_color(hx.border2)
                .flex()
                .flex_col()
                .on_mouse_down_out(cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                    if dismisses(menu, ws.ports_sort_trigger_press.take(), ev.position) {
                        ws.ports_sort_menu = None;
                        cx.notify();
                    }
                }));
            for (label, selected, choice) in items {
                list = list.child(
                    div()
                        .id(gpui::SharedString::from(format!("ports-sort-{menu:?}-{label}")))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .text_color(if selected { hx.tx0 } else { hx.tx1 })
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover))
                        .child(div().w(px(10.0)).child(if selected { "✓" } else { "" }))
                        .child(label)
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            ws.prefs.ports_sort = choice;
                            ws.ports_sort_menu = None;
                            ws.save_prefs(cx);
                            cx.notify();
                        })),
                );
            }
            // Deferred + anchored: drawn above the lists instead of pushing
            // them down, and kept inside the window.
            col = col.child(gpui::deferred(gpui::anchored().snap_to_window().child(list)));
        }
        col.into_any_element()
    }

    /// Count for the tab label: ports in this task.
    pub(crate) fn ports_task_count(&self) -> usize {
        // The label shows a count only while Ports is visible (`tab_label`);
        // don't classify and sort on every Files/Git/Review render for a
        // number that would be thrown away.
        // An old sample's count would read as current: none until a scan lands.
        if !self.ports_visible() || !self.ports_current() {
            return 0;
        }
        self.ports_view().map(|v| v.task.len()).unwrap_or(0)
    }

    /// Whether the shown sample is recent enough to present as current.
    fn ports_current(&self) -> bool {
        self.ports_sample_at.is_some_and(|at| ports::is_current(at.elapsed()))
    }

    /// Take one sample now, off the UI thread.
    pub(crate) fn refresh_ports(&mut self, cx: &mut Context<Self>) {
        // A scan is already running: it's queued to run once that one ends.
        if !self.ports_gate.try_start(true) {
            return;
        }
        let has_tmux = self.has_tmux;
        let seq = self.next_ports_seq();
        let started = std::time::Instant::now();
        cx.spawn(async move |view, cx| {
            let sample = cx.background_spawn(async move { ports::sample(has_tmux) }).await;
            let _ = view.update(cx, |ws, cx| ws.finish_ports_scan(seq, started, sample, cx));
        })
        .detach();
    }

    /// A scan ended: show its sample (`None`: it timed out; keep what's
    /// shown, the poll retries), then run the refresh queued meanwhile, if
    /// Ports is still on screen.
    pub(crate) fn finish_ports_scan(
        &mut self,
        seq: u64,
        started: std::time::Instant,
        sample: Option<ports::PortsSample>,
        cx: &mut Context<Self>,
    ) {
        match sample {
            Some(sample) => self.apply_ports(seq, started, sample, cx),
            None => {
                self.ports_scan_failed = true;
                cx.notify();
            }
        }
        if self.ports_gate.finish() && self.ports_visible() {
            self.refresh_ports(cx);
        }
    }

    /// Number a scan as it starts, for [`ports::accept_scan`].
    pub(crate) fn next_ports_seq(&mut self) -> u64 {
        self.ports_seq_issued += 1;
        self.ports_seq_issued
    }

    fn apply_ports(&mut self, seq: u64, started: std::time::Instant, sample: ports::PortsSample, cx: &mut Context<Self>) {
        if !ports::accept_scan(&mut self.ports_seq_applied, seq) {
            return;
        }
        self.ports_sample = Some(sample);
        self.ports_sample_at = Some(started);
        self.ports_scan_failed = false;
        cx.notify();
    }

    pub(crate) fn render_ports_panel(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = self.ports_view() else {
            return div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hx.tx2)
                .child(if self.ports_scan_failed {
                    "Can't list ports right now. Retrying…"
                } else {
                    "Looking for ports…"
                })
                .into_any_element();
        };
        // Rows from an old sample: shown for context, marked as such, and
        // without Open/Copy until a scan succeeds.
        let current = self.ports_current();

        let sort_bar = self.render_ports_sort_bar(hx, cx);
        let stale_note = (!current).then(|| {
            div()
                .flex_none()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(hx.tx2)
                .child(if self.ports_scan_failed {
                    "Can't list ports right now. These rows may be out of date; retrying…"
                } else {
                    "Updating… these rows may be out of date."
                })
        });
        let mut body = div()
            .id("ports-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(self.section_header(
                hx,
                SidebarSection::PortsTask,
                Some(count_badge(hx, view.task.len())),
                cx,
            ));

        if self.section_collapsed(SidebarSection::PortsTask) {
            // Folded: header only.
        } else if view.task.is_empty() {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(hx.tx1)
                            .child("No servers running in this task"),
                    )
                    .child(div().text_xs().text_color(hx.tx2).child(
                        "Start a dev server in a terminal or ask the agent to, and it shows up here.",
                    )),
            );
        } else {
            for row in &view.task {
                body = body.child(self.render_port_row(hx, "task", row, current, cx));
            }
        }

        body = body.child(self.section_header(
            hx,
            SidebarSection::PortsOther,
            Some(count_badge(hx, view.other.len())),
            cx,
        ));
        if !self.section_collapsed(SidebarSection::PortsOther) {
            for row in &view.other {
                body = body.child(self.render_port_row(hx, "other", row, current, cx));
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(sort_bar)
            .children(stale_note)
            .child(body)
            .into_any_element()
    }

    fn render_port_row(
        &self,
        hx: &HarnessTheme,
        section: &str,
        row: &PortRow,
        actionable: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let port = row.port;
        let url = ports::url_for(row);
        let loopback_only = row
            .addrs
            .iter()
            .all(|a| a == "127.0.0.1" || a == "::1" || a == "localhost");
        let reach = if loopback_only { "localhost only".to_string() } else { row.addrs.join(", ") };
        // A container port reads as the container ("db · urbital · → 5432"),
        // not as the engine process that happens to hold the socket.
        let what = match &row.container {
            Some(c) if c.matched_by_name => {
                format!("{} · → {} · matched by name", c.label, c.container_port)
            }
            Some(c) => format!("{} · → {}", c.label, c.container_port),
            None => format!("{} · {reach}", row.name),
        };
        let detail = match &row.member {
            Some(m) => format!("{what} · {m}"),
            None => what,
        };
        let hover = match &row.container {
            Some(c) => {
                let why = if c.matched_by_name {
                    " · listed here because its name starts with the project's; nothing else links it"
                } else {
                    ""
                };
                format!("container {} · image {} · via {} (pid {}){why}", c.name, c.image, row.name, row.pid)
            }
            None => format!("{} (pid {})", row.name, row.pid),
        };
        let hover = match &row.cwd {
            Some(dir) if row.container.is_none() => format!("{hover} · in {}", dir.display()),
            _ => hover,
        };
        let is_container = row.container.is_some();
        let id = format!("port-{section}-{}-{}", row.pid, port);
        div()
            .id(gpui::SharedString::from(id))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .hover(|d| d.bg(hx.surface))
            .tooltip(tip(hover))
            // Fixed slot so ports line up whether or not a row has the icon.
            .child(
                div().flex_none().size(px(14.0)).when(is_container, |d| {
                    d.child(
                        gpui::svg()
                            .size(px(14.0))
                            .path("icons/file_icons/docker.svg")
                            .text_color(hx.tx1),
                    )
                }),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(56.0))
                    .font_family(hx.font_mono)
                    .text_sm()
                    .text_color(hx.tx0)
                    .child(format!(":{port}")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child(detail),
            )
            .when(!actionable, |d| d.opacity(0.6))
            .when(actionable, |d| {
                d.child(small_button(hx, port_action_id("open", section, row.pid, port), "Open").on_click({
                    let url = url.clone();
                    cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.open_url(&url);
                    })
                }))
                .child(small_button(hx, port_action_id("copy", section, row.pid, port), "Copy").on_click(
                    cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
                        ws.set_note(format!("copied {url}"));
                        cx.notify();
                    }),
                ))
            })
    }
}

/// The count at the right of a section header, styled like ARCHIVED's.
fn count_badge(hx: &HarnessTheme, n: usize) -> AnyElement {
    div()
        .flex_none()
        .text_xs()
        .font_family(hx.font_mono)
        .text_color(hx.tx2)
        .child(n.to_string())
        .into_any_element()
}

fn small_button(hx: &HarnessTheme, id: String, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(gpui::SharedString::from(id))
        .flex_none()
        .px_2()
        .py(px(2.0))
        .rounded_md()
        .bg(hx.surface)
        .text_xs()
        .text_color(hx.tx0)
        .cursor_pointer()
        .hover(|d| d.bg(hx.surface_hover))
        .child(label)
}

/// Stable, unique action ID for a row. Several processes may legally listen
/// on the same port on different addresses, so the port alone is insufficient.
fn port_action_id(action: &str, section: &str, pid: u32, port: u16) -> String {
    format!("port-{action}-{section}-{pid}-{port}")
}

/// Whether a mouse-down closes the open `menu` from outside. `trigger_press`
/// is the last press a dropdown button saw (menu, position), recorded in the
/// capture phase, which reaches the button before the menu's outside handler
/// for the same press.
///
/// A press on the menu's own button is left to the button: its click
/// toggles the menu closed, and a press that never becomes a click (released
/// elsewhere, a right-click) leaves it open. Nothing is remembered past this
/// press, so no later click can be mistaken for it.
fn dismisses(
    menu: SortMenu,
    trigger_press: Option<(SortMenu, gpui::Point<Pixels>)>,
    press_at: gpui::Point<Pixels>,
) -> bool {
    trigger_press != Some((menu, press_at))
}

#[cfg(test)]
mod tests {
    use super::{SortMenu, dismisses, port_action_id};
    use gpui::{point, px};

    #[test]
    fn a_press_on_the_menus_own_button_leaves_it_to_the_button() {
        // Left, right, released elsewhere: whatever the press turns into, the
        // outside handler must not close the menu; the button's own click
        // toggles it, and a press that isn't a click leaves it open.
        let at = point(px(40.), px(12.));
        assert!(!dismisses(SortMenu::Field, Some((SortMenu::Field, at)), at));
    }

    #[test]
    fn any_other_press_closes_it() {
        let button = point(px(40.), px(12.));
        let elsewhere = point(px(300.), px(500.));
        assert!(dismisses(SortMenu::Field, Some((SortMenu::Field, button)), elsewhere));
        assert!(dismisses(SortMenu::Field, None, elsewhere));
        // The other dropdown's button: this menu closes, that one opens.
        assert!(dismisses(SortMenu::Field, Some((SortMenu::Order, button)), button));
    }

    #[test]
    fn port_actions_are_unique_for_distinct_listeners_on_one_port() {
        assert_ne!(
            port_action_id("open", "other", 41, 8080),
            port_action_id("open", "other", 42, 8080),
        );
        assert_ne!(
            port_action_id("open", "other", 41, 8080),
            port_action_id("copy", "other", 41, 8080),
        );
    }
}
