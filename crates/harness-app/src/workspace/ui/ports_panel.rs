//! The tools panel's Ports tab: listening TCP ports for the selected task, and
//! (collapsed) the rest of the Mac's. Data comes from `crate::ports`; this
//! file decides which sessions count as "this task" and draws the rows.

use super::super::*;
use crate::ports::{self, PortRow, PortsView, Scope};

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
        Some(ports::classify(sample, &self.ports_scopes()))
    }

    /// Count for the tab label: ports in this task.
    pub(crate) fn ports_task_count(&self) -> usize {
        self.ports_view().map(|v| v.task.len()).unwrap_or(0)
    }

    /// Take one sample now, off the UI thread.
    pub(crate) fn refresh_ports(&mut self, cx: &mut Context<Self>) {
        let has_tmux = self.has_tmux;
        cx.spawn(async move |view, cx| {
            let sample = cx.background_spawn(async move { ports::sample(has_tmux) }).await;
            let _ = view.update(cx, |ws, cx| ws.apply_ports(sample, cx));
        })
        .detach();
    }

    pub(crate) fn apply_ports(&mut self, sample: ports::PortsSample, cx: &mut Context<Self>) {
        self.ports_sample = Some(sample);
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
                .child("Looking for ports…")
                .into_any_element();
        };

        let mut body = div()
            .id("ports-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .py_1()
            .child(section_header(hx, "This task", view.task.len()));

        if view.task.is_empty() {
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
                body = body.child(self.render_port_row(hx, "task", row, cx));
            }
        }

        let open = self.ports_other_open;
        body = body.child(
            div()
                .id("ports-other-toggle")
                .mt_2()
                .cursor_pointer()
                .hover(|d| d.bg(hx.surface))
                .child(section_header(
                    hx,
                    if open { "▾ Other on this Mac" } else { "▸ Other on this Mac" },
                    view.other.len(),
                ))
                .on_click(cx.listener(|ws, _, _, cx| {
                    ws.ports_other_open = !ws.ports_other_open;
                    cx.notify();
                })),
        );
        if open {
            for row in &view.other {
                body = body.child(self.render_port_row(hx, "other", row, cx));
            }
        }
        body.into_any_element()
    }

    fn render_port_row(
        &self,
        hx: &HarnessTheme,
        section: &str,
        row: &PortRow,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let port = row.port;
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
            .child(small_button(hx, format!("port-open-{section}-{port}"), "Open").on_click(
                cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.open_url(&ports::url(port));
                }),
            ))
            .child(small_button(hx, format!("port-copy-{section}-{port}"), "Copy").on_click(
                cx.listener(move |ws, _, _, cx| {
                    cx.stop_propagation();
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(ports::url(port)));
                    ws.set_note(format!("copied {}", ports::url(port)));
                    cx.notify();
                }),
            ))
    }
}

fn section_header(hx: &HarnessTheme, title: &str, count: usize) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .px_3()
        .py_1()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(hx.tx1)
        .child(title.to_string())
        .child(div().text_color(hx.tx2).child(format!("· {count}")))
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
