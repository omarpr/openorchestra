//! The "you tried to edit a diff line" prompt — opens that file in a harness
//! file tab at the requested line.

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
        let target = match prompt.line {
            Some(l) => format!("{}:{l}", prompt.file),
            None => prompt.file.clone(),
        };
        let (file, line) = (prompt.file.clone(), prompt.line);

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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseDownEvent, _, cx| {
                    ws.edit_prompt = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(360.0))
                    .p_3()
                    .rounded_md()
                    .bg(hx.bg1)
                    .border_1()
                    .border_color(hx.border)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(hx.tx0)
                            .child("Open in editor"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx1)
                            .child(target),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(hx.tx2)
                            .child("The diff shows the change — edits happen in the file."),
                    )
                    .child(
                        div()
                            .id("edit-open")
                            .mt_1()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(hx.surface)
                            .text_xs()
                            .text_color(hx.tx0)
                            .cursor_pointer()
                            .hover(|d| d.bg(hx.surface_hover))
                            .child("Open")
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.open_in_editor(file.clone(), line, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}
