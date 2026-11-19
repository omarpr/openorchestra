//! Settings → Remote: turn the phone server on, pick its port, add a phone
//! (trust the CA, then pair), and manage paired devices.
//!
//! Rendering and pure helpers only. Every mutation is a `remote_*` method in
//! `workspace/remote.rs`, which owns the server lifecycle.

use std::time::{Duration, SystemTime};

use super::super::*;
use super::settings::{chip, section_title};
use crate::workspace::remote::{REVOKE_CONFIRM, RemoteServe, SETUP_PORT};
use oo_remote::ServerStatus;

/// Pairing codes live this long (`oo_remote::auth`'s TTL).
const CODE_TTL: Duration = Duration::from_secs(10 * 60);
/// One QR module, in px.
const QR_CELL: f32 = 4.0;
/// Quiet zone around the code, in modules (the spec minimum).
const QR_QUIET: usize = 4;

impl HarnessWorkspace {
    /// The Remote pane of Settings, top to bottom: on/off and status, port,
    /// "Add a device", paired devices, the stale-IP notice, keep-awake.
    pub(crate) fn render_remote_settings(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let r = &self.remote;
        let enabled = r.file.enabled;
        let serving = r.serve == RemoteServe::Serving;
        let mut col = div().flex().flex_col();

        // ---- Remote control: On/Off + status --------------------------------
        let mut onoff = div().flex().gap_1p5().pb_1();
        for (label, on) in [("On", true), ("Off", false)] {
            onoff = onoff.child(
                div()
                    .id(gpui::SharedString::from(format!("remote-enabled-{label}")))
                    .child(chip(label.to_string(), enabled == on, hx))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        if !on {
                            ws.remote_close_setup();
                        }
                        ws.remote_set_enabled(on, cx);
                    })),
            );
        }
        let line = status_line(&r.serve, r.status.as_ref(), enabled && r.busy());
        let line_color = match r.serve {
            RemoteServe::Error(_) => hx.red,
            RemoteServe::OtherBuild => hx.amber,
            _ => hx.tx1,
        };
        col = col
            .child(section_title("Remote control", hx))
            .child(onoff)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .pb_1()
                    .text_xs()
                    .child(div().flex_none().text_color(hx.tx2).child("Status"))
                    .child(div().flex_1().min_w_0().text_color(line_color).child(line)),
            );
        // One row per address, so long ones don't run off the card, each with
        // a Copy button (settings text isn't selectable). Only the main
        // connection's address unless "Show all" was clicked.
        let all_urls = serving_urls(&r.serve, r.status.as_ref());
        let primary = r.status.as_ref().and_then(|s| s.primary_ip);
        let (shown, hidden) = shown_urls(all_urls, primary, r.ui.show_all_urls);
        for (i, url) in shown.iter().enumerate() {
            let copy = url.clone();
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_1()
                    .pl(px(46.0))
                    .text_xs()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx0)
                            .child(url.clone()),
                    )
                    .child(
                        div()
                            .id(("remote-url-copy", i))
                            .flex_none()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(hx.border)
                            .text_color(hx.tx1)
                            .cursor_pointer()
                            .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                            .child("Copy")
                            .on_click(cx.listener(move |ws, _, _, cx| {
                                cx.stop_propagation();
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    copy.clone(),
                                ));
                                ws.set_note(format!("copied {copy}"));
                                cx.notify();
                            })),
                    ),
            );
        }
        if all_urls.len() > 1 {
            let label = if hidden > 0 {
                format!("Show all addresses ({})", all_urls.len())
            } else {
                "Show main address only".to_string()
            };
            col = col.child(
                div().pb_1().pl(px(46.0)).text_xs().child(
                    div()
                        .id("remote-urls-toggle")
                        .text_color(hx.tx2)
                        .cursor_pointer()
                        .hover(|d| d.text_color(hx.tx0))
                        .child(label)
                        .on_click(cx.listener(|ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.remote.ui.show_all_urls = !ws.remote.ui.show_all_urls;
                            cx.notify();
                        })),
                ),
            );
        }
        if serving && r.status.as_ref().is_some_and(|s| s.ca_outdated) {
            col = col
                .child(div().pb_1().text_xs().text_color(hx.amber).child(
                    "This Mac's name or address changed since its certificate was made, so \
                     devices using the new one can't connect. A new certificate fixes that; \
                     every device then has to be added again.",
                ))
                .child(
                    div().flex().pb_1().child(
                        button(hx, "Create a new certificate")
                            .id("remote-renew-ca")
                            .on_click(cx.listener(|ws, _, _, cx| {
                                cx.stop_propagation();
                                ws.remote_renew_ca(cx);
                            })),
                    ),
                );
        }
        if r.serve == RemoteServe::OtherBuild {
            col = col.child(div().pb_1().text_xs().text_color(hx.tx2).child(
                "The other openOrchestra build (Dev or release) is serving devices. Tabs you \
                 mark here reach them through it; this build takes over when it quits.",
            ));
        }

        // ---- Port ----------------------------------------------------------
        let port = r.file.port;
        let editing_port = matches!(self.edit_kind(), Some(EditKind::RemotePort));
        let mut port_row = div().flex().items_center().gap_2().pb_1();
        if let Some(editor) = editing_port.then(|| self.edit_editor()).flatten() {
            port_row = port_row.child(div().w(px(90.0)).child(self.render_input(hx, editor, cx)));
        } else {
            port_row = port_row.child(
                div()
                    .id("remote-port")
                    .w(px(90.0))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(hx.surface)
                    .border_1()
                    .border_color(if r.ui.port_error.is_some() {
                        hx.red
                    } else {
                        hx.border
                    })
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(hx.tx0)
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
                    .child(port.to_string())
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.remote.ui.port_error = None;
                        ws.begin_edit(EditKind::RemotePort, &port.to_string(), "7443", window, cx);
                    })),
            );
        }
        port_row = port_row.child(
            div()
                .text_xs()
                .text_color(hx.tx2)
                .child("Click to change. Enter applies it by restarting the server."),
        );
        col = col.child(section_title("Port", hx)).child(port_row);
        col = col.child(
            div()
                .pb_1()
                .text_xs()
                .text_color(hx.tx2)
                .child("Changing the port means re-adding the app on paired devices."),
        );
        if let Some(e) = &r.ui.port_error {
            col = col.child(div().pb_1().text_xs().text_color(hx.red).child(e.clone()));
        }

        // ---- Add a device --------------------------------------------------
        col = col.child(section_title("Add a device", hx));
        // The pairing steps open in their own dialog over Settings
        // (`render_add_phone_dialog`).
        if serving {
            col = col.child(
                div().flex().pb_1().child(
                    button(hx, "Add a device")
                        .id("remote-add-device")
                        .on_click(cx.listener(|ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.remote_open_setup(cx);
                        })),
                ),
            );
        } else {
            col = col.child(div().pb_1().text_xs().text_color(hx.tx2).child(if enabled {
                "Available once Remote control is listening."
            } else {
                "Turn on Remote control to add a device."
            }));
        }

        // ---- Agents on phone -------------------------------------------------
        col = col.child(section_title("Agents on devices", hx));
        let rows = crate::workspace::remote::remote_agent_rows(&self.remote_sources(), &r.file);
        if rows.is_empty() {
            col = col.child(
                div()
                    .pb_1()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("No open terminal or agent tabs"),
            );
        }
        for (i, row) in rows.into_iter().enumerate() {
            let icon = match &row.kind {
                Some(k) => crate::workspace::agents::agent_icon(&AgentKind::from(k.as_str()), 14.0),
                None => div()
                    .size(px(14.0))
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("\u{276F}_")
                    .into_any_element(),
            };
            let mut toggles = div().flex().flex_none().gap_1p5();
            for (label, on) in [("On", true), ("Off", false)] {
                let key = row.key.clone();
                let tab_id = row.tab_id;
                toggles = toggles.child(
                    div()
                        .id(gpui::SharedString::from(format!(
                            "remote-agent-{i}-{label}"
                        )))
                        .child(chip(label.to_string(), row.marked == on, hx))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            if ws.remote_is_marked(&key, tab_id) != on {
                                ws.remote_toggle_mark(&key, tab_id, cx);
                            }
                        })),
                );
            }
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_1()
                    .child(div().flex_none().w(px(14.0)).child(icon))
                    .child(
                        div().flex_1().min_w_0().flex().flex_col().text_xs().child(
                            // Both texts give way (with …) before the
                            // On/Off buttons do.
                            div()
                                .flex()
                                .min_w_0()
                                .overflow_hidden()
                                .gap_1p5()
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .max_w(px(160.0))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_color(hx.tx0)
                                        .child(display_title(&row.title, row.kind.is_none())),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_color(hx.tx2)
                                        .child(if row.worktree.is_empty() {
                                            row.project.clone()
                                        } else {
                                            format!("{} · {}", row.project, row.worktree)
                                        }),
                                ),
                        ),
                    )
                    .child(toggles),
            );
        }

        // ---- Networks ------------------------------------------------------
        if let Some(st) = r.status.as_ref().filter(|_| serving) {
            col = col.child(section_title("Networks", hx)).child(
                div().pb_1().text_xs().text_color(hx.tx2).child(
                    "Devices only connect from trusted networks. Adding a device trusts the \
                     network you're on.",
                ),
            );
            for (i, n) in st.networks.iter().enumerate() {
                let key = n.key.clone();
                let trust = !n.trusted;
                let where_ = match (n.here, n.primary) {
                    (true, true) => "this network",
                    (true, false) => "also connected",
                    (false, _) => "not connected now",
                };
                col = col.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .pb_1()
                        .text_xs()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(hx.font_mono)
                                .text_color(hx.tx0)
                                .child(n.key.clone()),
                        )
                        .child(div().flex_none().text_color(hx.tx2).child(format!(
                            "{where_} \u{00B7} {}",
                            if n.trusted { "trusted" } else { "not trusted" }
                        )))
                        .child(
                            button(hx, if trust { "Trust" } else { "Forget" })
                                .id(("remote-net", i))
                                .on_click(cx.listener(move |ws, _, _, cx| {
                                    cx.stop_propagation();
                                    ws.remote_set_network_trust(&key, trust, cx);
                                })),
                        ),
                );
            }
        }

        // ---- Paired devices ------------------------------------------------
        col = col.child(section_title("Paired devices", hx));
        let mut devices = div().flex().flex_col().gap_1().pb_1();
        if r.file.devices.is_empty() {
            devices = devices.child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("No paired devices yet."),
            );
        }
        let now = SystemTime::now();
        for d in &r.file.devices {
            let armed = matches!(
                &r.ui.revoke_armed,
                Some((id, at)) if *id == d.id && at.elapsed() < REVOKE_CONFIRM
            );
            let (ro_id, ro, rv_id) = (d.id.clone(), d.read_only, d.id.clone());
            devices = devices.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py_0p5()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(hx.tx0)
                                    .child(d.name.clone()),
                            )
                            .child(div().text_xs().text_color(hx.tx2).child(
                                if oo_remote::auth::is_idle_expired(d, now) {
                                    "expired \u{2014} unused for 30 days; pair it again".to_string()
                                } else {
                                    last_seen_text(d.last_seen.as_deref(), now)
                                },
                            )),
                    )
                    .child(
                        div()
                            .id(gpui::SharedString::from(format!("remote-ro-{}", d.id)))
                            .flex_none()
                            .child(chip("Watch only".into(), ro, hx))
                            .on_click(cx.listener(move |ws, _, _, cx| {
                                cx.stop_propagation();
                                ws.remote_set_read_only(&ro_id, !ro, cx);
                            })),
                    )
                    .child(
                        button(hx, if armed { "Confirm revoke" } else { "Revoke" })
                            .id(gpui::SharedString::from(format!("remote-revoke-{}", d.id)))
                            .flex_none()
                            .when(armed, |b| b.border_color(hx.red).text_color(hx.red))
                            .on_click(cx.listener(move |ws, _, _, cx| {
                                cx.stop_propagation();
                                ws.remote_revoke_click(&rv_id, cx);
                            })),
                    ),
            );
        }
        col = col.child(devices);

        // ---- Android IP notice ---------------------------------------------
        if let Some(st) = r.status.as_ref().filter(|s| !s.stale_ip_devices.is_empty()) {
            col = col.child(div().pb_1().text_xs().text_color(hx.amber).child(format!(
                "This Mac's IP changed — re-add the app on: {}",
                st.stale_ip_devices.join(", ")
            )));
        }
        col =
            col.child(div().pb_1().text_xs().text_color(hx.tx2).child(
                "Tip: reserve this Mac's IP on your router so Android devices keep working.",
            ));

        // ---- Keep awake ----------------------------------------------------
        let awake = r.file.keep_awake;
        let mut awake_row = div().flex().gap_1p5().pb_1();
        for (label, on) in [("On", true), ("Off", false)] {
            awake_row = awake_row.child(
                div()
                    .id(gpui::SharedString::from(format!("remote-awake-{label}")))
                    .child(chip(label.to_string(), awake == on, hx))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        ws.remote_set_keep_awake(on, cx);
                    })),
            );
        }
        col.child(section_title(
            "Keep this Mac awake while Remote control is on",
            hx,
        ))
        .child(awake_row)
        .child(div().pb_2().text_xs().text_color(hx.tx2).child(
            "A closed MacBook lid still sleeps unless it's on power with an external display.",
        ))
    }

    /// "Add a device" as a dialog over Settings: a backdrop click, ✕ or Done
    /// closes it (and the setup port) and leaves Settings open.
    pub(crate) fn render_add_phone_dialog(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let close = |ws: &mut Self, cx: &mut Context<Self>| {
            ws.remote_close_setup();
            cx.notify();
        };
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hx.tx0)
                    .child("Add a device"),
            )
            .child(
                div()
                    .id("remote-setup-close")
                    .px_2()
                    .rounded_md()
                    .text_sm()
                    .text_color(hx.tx2)
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                    .child("✕")
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        close(ws, cx);
                    })),
            );
        let card = div()
            .id("remote-setup-card")
            .flex()
            .flex_col()
            .gap_3()
            .w(px(440.0))
            .max_h(px(600.0))
            .overflow_y_scroll()
            .p_4()
            .rounded_lg()
            .bg(hx.bg1)
            .border_1()
            .border_color(hx.border)
            .shadow_lg()
            .child(header)
            .child(self.render_add_phone(hx, cx));
        div()
            .id("remote-setup-backdrop")
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.35))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation())
            // Stop here: the Settings backdrop underneath would close Settings.
            .on_click(cx.listener(move |ws, _, _, cx| {
                cx.stop_propagation();
                close(ws, cx);
            }))
            .child(card.on_click(|_, _, cx| cx.stop_propagation()))
            .into_any_element()
    }

    /// The "Add a device" steps: 1. trust this Mac (QR + fingerprint),
    /// 2. pair (code).
    fn render_add_phone(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> gpui::Div {
        let ui = &self.remote.ui;
        let step = |n: &str, title: &str| {
            div()
                .pt_1()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(hx.tx0)
                .child(format!("{n}. {title}"))
        };
        let mut trust = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(step("1", "Trust this Mac"));
        let setup_error = setup_error(self.remote.status.as_ref());
        match (setup_error, &ui.setup_url) {
            // The setup port didn't open: a QR would lead nowhere.
            (Some(err), _) => {
                trust = trust.child(div().text_xs().text_color(hx.amber).child(err.to_string()));
            }
            (None, Some(url)) => {
                trust = trust.child(qr_view(&ui.setup_qr)).child(
                    div()
                        .text_xs()
                        .font_family(hx.font_mono)
                        .text_color(hx.tx1)
                        .child(url.clone()),
                );
            }
            (None, None) => {
                trust = trust.child(div().text_xs().text_color(hx.amber).child(
                    "This Mac has no local network address. Connect it to Wi-Fi or Ethernet.",
                ));
            }
        }
        let fingerprint = self
            .remote
            .status
            .as_ref()
            .map(|s| fingerprint_lines(&s.ca_fingerprint))
            .unwrap_or_default();
        let mut fp = div().flex().flex_col();
        for line in fingerprint {
            fp = fp.child(line);
        }
        trust = trust
            .child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("Certificate fingerprint (SHA-256)"),
            )
            .child(
                div()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(hx.amber)
                    .text_sm()
                    .font_family(hx.font_mono)
                    .text_color(hx.tx0)
                    .child(fp),
            )
            .child(div().text_xs().text_color(hx.tx1).child(
                "Before trusting it, check that the SHA-256 fingerprint your device shows for the \
                 certificate matches this exactly. iPhone: on the profile, tap More Details \
                 before Install. Android: Encryption & credentials \u{2192} User credentials. \
                 Don't rely on the setup page: it can't vouch for itself.",
            ));

        let mut pair = div().flex().flex_col().gap_1().child(step("2", "Pair"));
        match (&ui.code, &ui.code_error) {
            (Some((code, issued)), _) => {
                let expired = issued.elapsed() >= CODE_TTL;
                pair = pair
                    .child(
                        div()
                            .text_size(px(28.0))
                            .font_family(hx.font_mono)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(if expired { hx.tx2 } else { hx.tx0 })
                            .child(code.clone()),
                    )
                    .child(div().text_xs().text_color(hx.tx2).child(if expired {
                        "Expired. Get a new code."
                    } else {
                        "Valid for 10 minutes"
                    }));
            }
            (None, Some(e)) => {
                pair = pair.child(div().text_xs().text_color(hx.red).child(e.clone()));
            }
            (None, None) => {}
        }
        pair = pair.child(
            div().flex().child(
                button(hx, "New code")
                    .id("remote-new-code")
                    .on_click(cx.listener(|ws, _, _, cx| {
                        cx.stop_propagation();
                        ws.remote_new_code(cx);
                    })),
            ),
        );

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(trust)
            .child(pair)
            .child(
                div()
                    .flex()
                    .child(
                        button(hx, "Done")
                            .id("remote-setup-done")
                            .on_click(cx.listener(|ws, _, _, cx| {
                                cx.stop_propagation();
                                ws.remote_close_setup();
                                cx.notify();
                            })),
                    ),
            )
    }
}

/// A tab's title for a list that already shows its icon: a shell's "❯_ "
/// prefix would repeat it.
pub(crate) fn display_title(title: &str, shell: bool) -> String {
    if shell {
        let t = title
            .trim_start_matches('\u{276F}')
            .trim_start_matches('_')
            .trim_start();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    title.to_string()
}

/// A plain bordered button (the Settings "Reset …" style).
fn button(hx: &HarnessTheme, label: &str) -> gpui::Div {
    div()
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(hx.border)
        .bg(hx.surface)
        .text_xs()
        .text_color(hx.tx1)
        .cursor_pointer()
        .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
        .child(label.to_string())
}

/// The QR code as rows of run-length divs, black on white whatever the theme
/// (phones read inverted codes poorly), inside a quiet zone.
fn qr_view(cells: &[Vec<bool>]) -> gpui::Div {
    let black = gpui::rgb(0x000000);
    let mut grid = div().flex().flex_col();
    for row in cells {
        let mut line = div().flex().h(px(QR_CELL));
        let mut i = 0;
        while i < row.len() {
            let dark = row[i];
            let run = row[i..].iter().take_while(|c| **c == dark).count();
            line = line.child(
                div()
                    .flex_none()
                    .w(px(QR_CELL * run as f32))
                    .h_full()
                    .when(dark, |d| d.bg(black)),
            );
            i += run;
        }
        grid = grid.child(line);
    }
    div().flex().child(
        div()
            .flex_none()
            .p(px(QR_CELL * QR_QUIET as f32))
            .bg(gpui::rgb(0xffffff))
            .child(grid),
    )
}

/// The QR modules for `text`, row-major, `true` = dark. No quiet zone: the
/// renderer pads it.
pub(crate) fn qr_cells(text: &str) -> Vec<Vec<bool>> {
    let Ok(code) = qrcode::QrCode::new(text.as_bytes()) else {
        return Vec::new();
    };
    let n = code.width();
    code.to_colors()
        .chunks(n)
        .map(|row| row.iter().map(|c| *c == qrcode::Color::Dark).collect())
        .collect()
}

/// Validate the Port field: 1024..=65535 and not the setup port.
pub(crate) fn parse_port(text: &str) -> Result<u16, String> {
    let port = text
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|p| *p >= 1024)
        .ok_or_else(|| "Use a port from 1024 to 65535".to_string())?;
    if port == SETUP_PORT {
        return Err(format!("Port {SETUP_PORT} is used to set up devices"));
    }
    Ok(port)
}

/// "last seen 5 min ago" from a device's RFC 3339 `last_seen`.
pub(crate) fn last_seen_text(last_seen: Option<&str>, now: SystemTime) -> String {
    use time::format_description::well_known::Rfc3339;
    let Some(s) = last_seen else {
        return "never seen".into();
    };
    let Ok(at) = time::OffsetDateTime::parse(s, &Rfc3339) else {
        return "last seen at an unknown time".into();
    };
    // Negative (clock skew) clamps to zero.
    let secs = (time::OffsetDateTime::from(now) - at)
        .whole_seconds()
        .max(0);
    let rel = match secs {
        s if s < 60 => return "last seen just now".into(),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86_400 => format!("{} h", s / 3600),
        s if s < 2 * 86_400 => "1 day".into(),
        s => format!("{} days", s / 86_400),
    };
    format!("last seen {rel} ago")
}

/// The CA fingerprint (`AB:CD:…`, 32 bytes) as lines of 16 bytes: one
/// colon-joined token doesn't wrap, and a clipped fingerprint can't be
/// compared.
pub(crate) fn fingerprint_lines(fp: &str) -> Vec<String> {
    let parts: Vec<&str> = fp.split(':').filter(|p| !p.is_empty()).collect();
    parts.chunks(16).map(|c| c.join(":")).collect()
}

/// The addresses listed (one per row, each copyable) under "Listening on".
/// The addresses to list: the main connection's (`primary`) alone unless
/// `show_all`, plus how many are hidden. Falls back to the first address
/// when none matches `primary`.
pub(crate) fn shown_urls(
    urls: &[String],
    primary: Option<std::net::IpAddr>,
    show_all: bool,
) -> (Vec<String>, usize) {
    if show_all || urls.len() <= 1 {
        return (urls.to_vec(), 0);
    }
    let main = primary
        .map(|ip| format!("//{ip}:"))
        .and_then(|needle| urls.iter().find(|u| u.contains(&needle)))
        .or(urls.first());
    (main.into_iter().cloned().collect(), urls.len() - 1)
}

/// Why "Add a device" can't offer its QR (the setup port failed to open).
pub(crate) fn setup_error(status: Option<&ServerStatus>) -> Option<&str> {
    status.and_then(|s| s.setup_error.as_deref())
}

pub(crate) fn serving_urls<'a>(
    serve: &RemoteServe,
    status: Option<&'a ServerStatus>,
) -> &'a [String] {
    match (serve, status) {
        (RemoteServe::Serving, Some(st)) => &st.urls,
        _ => &[],
    }
}

/// The Status line under the On/Off chips.
pub(crate) fn status_line(
    serve: &RemoteServe,
    status: Option<&ServerStatus>,
    starting: bool,
) -> String {
    match serve {
        RemoteServe::Serving => match status {
            Some(st) if !st.urls.is_empty() => "Listening on".into(),
            _ => "Listening".into(),
        },
        RemoteServe::OtherBuild => "Served by the other openOrchestra build".into(),
        RemoteServe::Error(e) => e.clone(),
        RemoteServe::Off if starting => "Starting…".into(),
        RemoteServe::Off => "Off".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn qr_for_a_short_url_is_a_square_of_21_plus_modules() {
        let cells = qr_cells("http://192.168.1.20:7080/setup");
        let n = cells.len();
        assert!(n >= 21, "{n}");
        assert!(cells.iter().all(|row| row.len() == n));
        // Top-left finder pattern: dark ring, light gap, dark centre.
        assert!(cells[0][0] && cells[0][6] && cells[6][0] && cells[6][6]);
        assert!(!cells[1][1] && !cells[5][5]);
        assert!(cells[3][3]);
        // Separator after the finder is light.
        assert!(!cells[7][0] && !cells[0][7]);
    }

    #[test]
    fn port_accepts_unprivileged_ports_except_setup() {
        assert_eq!(parse_port("7443"), Ok(7443));
        assert_eq!(parse_port(" 1024 "), Ok(1024));
        assert_eq!(parse_port("65535"), Ok(65535));
        for bad in ["", "abc", "0", "80", "1023", "65536", "-1", "74 43"] {
            assert!(parse_port(bad).is_err(), "{bad:?} accepted");
        }
        let e = parse_port("7080").unwrap_err();
        assert!(e.contains("7080"), "{e}");
        assert_eq!(
            parse_port("80").unwrap_err(),
            "Use a port from 1024 to 65535"
        );
    }

    #[test]
    fn last_seen_is_relative() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000); // 2023-11-14T22:13:20Z
        let at = |s: &str| last_seen_text(Some(s), now);
        assert_eq!(last_seen_text(None, now), "never seen");
        assert_eq!(at("2023-11-14T22:13:00Z"), "last seen just now");
        assert_eq!(at("2023-11-14T22:12:20Z"), "last seen 1 min ago");
        assert_eq!(at("2023-11-14T21:20:00Z"), "last seen 53 min ago");
        assert_eq!(at("2023-11-14T20:13:20Z"), "last seen 2 h ago");
        assert_eq!(at("2023-11-13T22:13:20Z"), "last seen 1 day ago");
        assert_eq!(at("2023-11-04T22:13:20Z"), "last seen 10 days ago");
        // Clock skew: a future stamp reads as now, not "-3 min".
        assert_eq!(at("2023-11-14T22:16:20Z"), "last seen just now");
        assert_eq!(at("garbage"), "last seen at an unknown time");
        // The shape `oo_remote::auth` writes (Rfc3339 with nanoseconds).
        assert_eq!(at("2023-11-14T22:08:19.123456789Z"), "last seen 5 min ago");
    }

    #[test]
    fn fingerprint_splits_into_lines_of_16_bytes() {
        let bytes: Vec<String> = (0..32).map(|b| format!("{b:02X}")).collect();
        let lines = fingerprint_lines(&bytes.join(":"));
        assert_eq!(lines, [bytes[..16].join(":"), bytes[16..].join(":")]);
        assert_eq!(fingerprint_lines(""), Vec::<String>::new());
    }

    #[test]
    fn setup_error_replaces_the_qr() {
        assert_eq!(setup_error(None), None);
        assert_eq!(setup_error(Some(&ServerStatus::default())), None);
        let st = ServerStatus {
            setup_error: Some("Couldn't open port 7080".into()),
            ..ServerStatus::default()
        };
        assert_eq!(setup_error(Some(&st)), Some("Couldn't open port 7080"));
    }

    #[test]
    fn shell_titles_drop_the_glyph_the_icon_already_shows() {
        assert_eq!(display_title("\u{276F}_ shell", true), "shell");
        assert_eq!(display_title("\u{276F}_ npm run dev", true), "npm run dev");
        assert_eq!(display_title("\u{276F}_ ", true), "\u{276F}_ ");
        assert_eq!(display_title("claude", false), "claude");
    }

    #[test]
    fn only_the_main_connections_address_by_default() {
        let urls: Vec<String> = [
            "https://mac.local:7443/",
            "https://192.168.86.26:7443/",
            "https://192.168.139.3:7443/",
        ]
        .map(String::from)
        .to_vec();
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        assert_eq!(
            shown_urls(&urls, Some(ip("192.168.86.26")), false),
            (vec!["https://192.168.86.26:7443/".to_string()], 2)
        );
        assert_eq!(
            shown_urls(&urls, Some(ip("192.168.86.26")), true),
            (urls.clone(), 0)
        );
        // No match (or no main connection): the first address.
        assert_eq!(
            shown_urls(&urls, Some(ip("10.0.0.9")), false).0,
            vec![urls[0].clone()]
        );
        assert_eq!(shown_urls(&urls, None, false).0, vec![urls[0].clone()]);
        // "192.168.86.2" must not match "192.168.86.26".
        assert_eq!(
            shown_urls(&urls, Some(ip("192.168.86.2")), false).0,
            vec![urls[0].clone()]
        );
        assert_eq!(shown_urls(&urls[..1], None, false), (urls[..1].to_vec(), 0));
    }

    #[test]
    fn status_line_per_state() {
        let st = ServerStatus {
            urls: vec![
                "https://mac.local:7443/".into(),
                "https://192.168.1.20:7443/".into(),
            ],
            ca_fingerprint: "AB:CD".into(),
            ..ServerStatus::default()
        };
        // The URLs themselves are listed one per row below, each copyable.
        assert_eq!(
            status_line(&RemoteServe::Serving, Some(&st), false),
            "Listening on"
        );
        assert_eq!(
            serving_urls(&RemoteServe::Serving, Some(&st)),
            ["https://mac.local:7443/", "https://192.168.1.20:7443/"]
        );
        assert!(serving_urls(&RemoteServe::OtherBuild, Some(&st)).is_empty());
        assert!(serving_urls(&RemoteServe::Serving, None).is_empty());
        assert_eq!(status_line(&RemoteServe::Serving, None, false), "Listening");
        assert_eq!(
            status_line(&RemoteServe::OtherBuild, None, false),
            "Served by the other openOrchestra build"
        );
        assert_eq!(
            status_line(&RemoteServe::Error("certs: denied".into()), None, false),
            "certs: denied"
        );
        assert_eq!(status_line(&RemoteServe::Off, None, false), "Off");
        assert_eq!(status_line(&RemoteServe::Off, None, true), "Starting…");
    }
}
