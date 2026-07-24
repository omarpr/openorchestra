//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::*;

impl HarnessWorkspace {

    pub(crate) fn render_top_bar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let branch = self.resolve().map(|r| r.branch).unwrap_or_else(|| "no folder".to_string());
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(38.0))
            .px_3()
            .bg(hx.bg0)
            .border_b_1()
            .border_color(hx.border)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).child("openOrchestra"))
                    .child(div().text_xs().text_color(hx.tx2).child("·"))
                    .child(div().text_xs().font_family(hx.font_mono).text_color(hx.tx1).child(branch)),
            )
            .child(
                div()
                    .id("toggle-sidebar")
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .text_color(if self.prefs.sidebar_open { hx.tx1 } else { hx.tx2 })
                    .cursor_pointer()
                    .tooltip(tip("Toggle worktrees sidebar"))
                    .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                    .child("▤ worktrees")
                    .on_click(cx.listener(|ws, _, _, cx| {
                        ws.prefs.sidebar_open = !ws.prefs.sidebar_open;
                        ws.save_prefs(cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("toggle-diff")
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .text_color(if self.prefs.diff_open { hx.tx1 } else { hx.tx2 })
                    .cursor_pointer()
                    .tooltip(tip("Toggle diff panel"))
                    .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                    .child("diff ▤")
                    .on_click(cx.listener(|ws, _, _, cx| {
                        ws.prefs.diff_open = !ws.prefs.diff_open;
                        ws.save_prefs(cx);
                        cx.notify();
                    })),
            )
    }

    /// The conductor pet: a small maestro who appears with each notification
    /// and conducts until dismissed or done. Divs only — head, tapering robe,
    /// and two arms whose geometry flips between beats, plus alternating
    /// notes. Clippy walked so this little one could wave.
    pub(crate) fn render_pet(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (message, _) = self.pet.clone()?;
        let up = self.pet_pose;
        let arm = |left: bool| {
            let d = div().absolute().rounded_full().bg(hx.accent);
            match (left, up) {
                // Beat up: arms vertical, raised high.
                (true, true) => d.w(px(7.0)).h(px(26.0)).top(px(18.0)).left(px(14.0)),
                (false, true) => d.w(px(7.0)).h(px(26.0)).top(px(18.0)).left(px(67.0)),
                // Beat down: arms swing out sideways.
                (true, false) => d.w(px(24.0)).h(px(7.0)).top(px(40.0)).left(px(2.0)),
                (false, false) => d.w(px(24.0)).h(px(7.0)).top(px(40.0)).left(px(62.0)),
            }
        };
        Some(
            div()
                .absolute()
                .bottom(px(52.0))
                .right(px(16.0))
                .w(px(232.0))
                .rounded_lg()
                .bg(hx.surface2)
                .border_1()
                .border_color(hx.border2)
                .shadow_lg()
                .p_2()
                .flex()
                .gap_2()
                .items_center()
                .child(
                    // The stage: 88×72, everything positioned in it.
                    div()
                        .relative()
                        .flex_none()
                        .w(px(88.0))
                        .h(px(72.0))
                        .child(
                            div()
                                .absolute()
                                .top(px(4.0))
                                .left(px(33.0))
                                .w(px(22.0))
                                .h(px(22.0))
                                .rounded_full()
                                .bg(hx.tx0),
                        )
                        .child(
                            // The robe: wide-bottomed via border radius.
                            div()
                                .absolute()
                                .top(px(28.0))
                                .left(px(26.0))
                                .w(px(36.0))
                                .h(px(40.0))
                                .rounded_t_full()
                                .rounded_b_md()
                                .bg(hx.accent),
                        )
                        .child(arm(true))
                        .child(arm(false))
                        .child(
                            // The baton, only visible on the upbeat.
                            div()
                                .absolute()
                                .w(px(3.0))
                                .h(px(16.0))
                                .rounded_full()
                                .bg(hx.tx0)
                                .top(if up { px(2.0) } else { px(36.0) })
                                .left(if up { px(16.0) } else { px(-4.0) }),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(if up { px(0.0) } else { px(10.0) })
                                .right(px(2.0))
                                .text_sm()
                                .text_color(hx.amber)
                                .child(if up { "♪" } else { "♫" }),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(hx.tx0)
                        .child(message),
                )
                .child(
                    div()
                        .id("pet-close")
                        .flex_none()
                        .px_1()
                        .rounded_md()
                        .text_xs()
                        .text_color(hx.tx2)
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                        .child("✕")
                        .on_click(cx.listener(|ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.pet = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }
}
