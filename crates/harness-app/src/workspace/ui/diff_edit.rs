//! The "you tried to edit a diff line" question (§3.4).
//!
//! Split out of `diff.rs` rather than added to it: that file already renders the
//! surface, the header, the file list, the rows and the review bar, and a modal
//! raised *from* a diff row is a separate concern from drawing one.
//!
//! The question exists because the two useful answers are genuinely different.
//! Reusing an open editor keeps the tab count down and is what you want when
//! you already have the file's neighbourhood on screen; a new tab is what you
//! want when the editor you have open is showing something you are not finished
//! with. Guessing between them means sometimes scrolling a file the user was
//! reading out from under them, which is worse than asking.

use super::super::*;

impl HarnessWorkspace {
    pub(crate) fn render_edit_prompt(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(prompt) = self.edit_prompt.as_ref() else {
            return div().into_any_element();
        };
        // Name the line when there is one: it is the difference between "we
        // will open this file" and "we will put you where you were looking".
        let target = match prompt.line {
            Some(l) => format!("{}:{l}", prompt.file),
            None => prompt.file.clone(),
        };
        let (reuse_file, reuse_line) = (prompt.file.clone(), prompt.line);
        let (new_file, new_line) = (prompt.file.clone(), prompt.line);

        let button = |id: &'static str, label: String, primary: bool| {
            div()
                .id(id)
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .h(px(34.0))
                .rounded_md()
                .border_1()
                .border_color(if primary { hx.accent } else { hx.border2 })
                .bg(if primary { tint(hx.accent, 0.16) } else { hx.surface })
                .text_sm()
                .text_color(if primary { hx.accent } else { hx.tx0 })
                .cursor_pointer()
                .hover(|d| d.border_color(hx.accent).text_color(hx.accent))
                .child(label)
        };

        let card = div()
            .w(px(420.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .shadow_lg()
            // A click inside the card must not reach the dismiss backdrop.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hx.tx0)
                    .child("Open this file to edit it?"),
            )
            .child(
                div()
                    .text_sm()
                    .font_family(hx.font_mono)
                    .text_color(hx.tx1)
                    .child(target),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("The diff shows the change — edits happen in the editor."),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_1()
                    .child(
                        button("edit-existing", "Open in editor".into(), true).on_click(
                            cx.listener(move |ws, _, window, cx| {
                                ws.open_in_editor(reuse_file.clone(), reuse_line, false, window, cx)
                            }),
                        ),
                    )
                    .child(
                        button("edit-new-tab", "New editor tab".into(), false).on_click(
                            cx.listener(move |ws, _, window, cx| {
                                ws.open_in_editor(new_file.clone(), new_line, true, window, cx)
                            }),
                        ),
                    ),
            );

        div()
            .id("edit-backdrop")
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000099))
            // Clicking outside cancels. Reading a diff is the common case, and
            // an accidental double-click must not cost you your place in it.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseDownEvent, _, cx| {
                    ws.edit_prompt = None;
                    cx.notify();
                }),
            )
            .child(card)
            .into_any_element()
    }
}
