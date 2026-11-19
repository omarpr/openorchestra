//! The requirements screen: what this machine is missing, and what it costs.
//!
//! Deliberately not a section of Settings. Settings is where you go on purpose;
//! this has to find *you*, on a first launch or when something required is
//! absent — and when it is blocking there is nothing to configure, only
//! something to install.

use super::super::*;

use crate::preflight::{self, Severity};

impl HarnessWorkspace {
    pub(crate) fn render_preflight(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let blocked = preflight::blocked(&self.preflight);
        let missing = preflight::any_missing(&self.preflight);

        let row = |c: &preflight::Check| {
            let (mark, color) = match (c.missing(), c.severity) {
                (false, _) => ("✓", hx.green),
                (true, Severity::Required) => ("✗", hx.red),
                (true, Severity::Recommended) => ("!", hx.amber),
                (true, Severity::Optional) => ("·", hx.tx2),
            };
            let mut line = div().flex().flex_col().gap_0p5().py_1p5().child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_none().w(px(12.0)).text_color(color).child(mark))
                    .child(
                        div()
                            .flex_none()
                            .w(px(110.0))
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(if c.missing() { hx.tx0 } else { hx.tx1 })
                            .child(c.name),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx2)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(match &c.found {
                                Some(v) => v.clone(),
                                None if c.severity == Severity::Required => {
                                    "required — not found".to_string()
                                }
                                None => "not found".to_string(),
                            }),
                    ),
            );
            // Only a gap needs explaining. A present tool has nothing to say
            // beyond its version, and repeating the cost for every row would
            // bury the two lines that matter.
            if c.missing() {
                line = line
                    .child(
                        div()
                            .pl(px(24.0))
                            .text_xs()
                            .text_color(hx.tx1)
                            .child(c.cost),
                    )
                    .child(
                        div()
                            .pl(px(24.0))
                            .mt_0p5()
                            .px_1p5()
                            .py_0p5()
                            .rounded_md()
                            .bg(hx.bg0)
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.accent)
                            .child(c.fix),
                    );
            }
            line
        };

        let mut body = div()
            .id("preflight-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_4()
            .py_2();
        for c in &self.preflight {
            body = body.child(row(c));
        }

        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .h(px(28.0))
                .px_3()
                .rounded_md()
                .border_1()
                .border_color(if primary { hx.accent } else { hx.border2 })
                .bg(if primary {
                    tint(hx.accent, 0.16)
                } else {
                    hx.surface
                })
                .text_xs()
                .text_color(if primary { hx.accent } else { hx.tx1 })
                .cursor_pointer()
                .hover(|d| d.border_color(hx.accent).text_color(hx.accent))
                .child(label)
        };

        let card = div()
            .id("preflight-card")
            .flex()
            .flex_col()
            .w(px(560.0))
            .max_h(px(620.0))
            .rounded_lg()
            .bg(hx.bg1)
            .border_1()
            .border_color(if blocked { hx.red } else { hx.border })
            .shadow_lg()
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(hx.border)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(hx.tx0)
                            .child(if blocked {
                                "openOrchestra needs a couple of things first"
                            } else {
                                "What openOrchestra uses"
                            }),
                    )
                    .child(div().text_xs().text_color(hx.tx2).child(if blocked {
                        "Install what is marked ✗, then Re-check — no restart needed."
                    } else if missing {
                        "Everything required is here. The rest is optional; each line says what it costs."
                    } else {
                        "Everything is installed. This screen lives in the openOrchestra menu."
                    })),
            )
            .child(body)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(hx.border)
                    .child(button("preflight-recheck", "Re-check", blocked).on_click(
                        cx.listener(|ws, _, _, cx| ws.rerun_preflight(cx)),
                    ))
                    // No way out while a required tool is missing: a dismissed
                    // screen would leave an app whose every surface is empty,
                    // with the explanation gone.
                    .when(!blocked, |d| {
                        d.child(button("preflight-close", "Continue", true).on_click(
                            cx.listener(|ws, _, _, cx| ws.dismiss_preflight(cx)),
                        ))
                    }),
            );

        div()
            .id("preflight-backdrop")
            // Occlude, not just catch clicks: without it the elements under
            // an overlay still hover, and their tooltips show through it.
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.55))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    // Click-outside dismisses only when dismissing is allowed.
                    if !preflight::blocked(&ws.preflight) {
                        ws.dismiss_preflight(cx);
                    }
                }),
            )
            // Same idiom as the settings overlay: prove it reaches pixels. An
            // overlay that lays out to nothing is indistinguishable from one
            // that never opened.
            .when(crate::probe_paint(), |d| {
                d.child(
                    gpui::canvas(
                        |b, _, _| log::info!("paint: preflight overlay bounds = {b:?}"),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .child(card)
            .into_any_element()
    }
}
