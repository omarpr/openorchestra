//! Rendering for the chrome view, grouped by screen region. Each submodule is
//! an `impl HarnessWorkspace` block (see `workspace.rs` for the split rationale
//! and the shared types/struct they render).

mod center;
mod chrome;
pub(crate) use chrome::app_menu_open;
pub(crate) mod conductor;
pub(crate) mod device_panel;
mod dialogs;
mod diff;
mod diff_edit;
mod new_task;
pub(crate) mod ports_panel;
mod preflight;
mod project_search;
mod review_overview;
mod settings;
/// `pub(crate)` for `qr_cells` / `parse_port`, which `remote.rs` and the
/// port field's commit use.
pub(crate) mod settings_remote;
/// `pub(crate)` for `SidebarSection` alone — its variant labels are the keys
/// `Prefs::collapsed_sections` persists, so the prefs tests pin them.
pub(crate) mod sidebar;
mod start_specialists;
mod tool_panel;

use super::*;
use gpui::{Div, ElementId, ScrollHandle, SharedString, Stateful};
use std::{cell::RefCell, rc::Rc};

/// Tab order for a modal. The two non-interactive stops let Tab and Shift-Tab
/// wrap inside the dialog even when the underlying workspace has tab stops.
pub(super) struct ModalFocus {
    start: FocusHandle,
    end: FocusHandle,
    controls: RefCell<HashMap<ElementId, FocusHandle>>,
    scroll: ScrollHandle,
    revealed: Rc<RefCell<Option<FocusHandle>>>,
}

impl ModalFocus {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            start: cx.focus_handle().tab_index(-1),
            end: cx.focus_handle().tab_index(isize::MAX),
            controls: Default::default(),
            scroll: ScrollHandle::new(),
            revealed: Default::default(),
        }
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.start, cx);
    }

    pub(super) fn restore(&self, window: &mut Window, cx: &mut App) {
        if !self.start.contains_focused(window, cx) {
            self.focus(window, cx);
        }
    }

    pub(super) fn reset_scroll(&self) {
        self.scroll.set_offset(gpui::Point::default());
        self.revealed.borrow_mut().take();
    }

    fn card(&self, card: Stateful<Div>, title: &'static str) -> Stateful<Div> {
        let forward_start = self.start.clone();
        let backward_start = self.start.clone();
        let end = self.end.clone();
        card.role(gpui::Role::Dialog)
            .aria_label(title)
            .key_context("HarnessModal")
            .track_focus(&self.start)
            .tab_group()
            .on_action(move |_: &crate::FocusNextModalControl, window, cx| {
                window.focus_next(cx);
                if !forward_start.contains_focused(window, cx) {
                    window.focus(&forward_start, cx);
                    window.focus_next(cx);
                }
            })
            .on_action(move |_: &crate::FocusPreviousModalControl, window, cx| {
                window.focus_prev(cx);
                if !backward_start.contains_focused(window, cx) {
                    window.focus(&end, cx);
                    window.focus_prev(cx);
                }
            })
    }

    fn end(&self) -> Div {
        div().absolute().size(px(0.0)).track_focus(&self.end)
    }

    fn scroll_control(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        role: gpui::Role,
        hx: &HarnessTheme,
        cx: &mut App,
    ) -> Stateful<Div> {
        let id = id.into();
        let focus = self.control_focus(id.clone(), cx);
        self.reveal_on_focus(
            modal_control(id, label, role, hx).track_focus(&focus),
            focus,
        )
    }

    pub(super) fn control_focus(&self, id: ElementId, cx: &mut App) -> FocusHandle {
        self.controls
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    fn scroll_button(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        hx: &HarnessTheme,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.scroll_control(id, label, gpui::Role::Button, hx, cx)
    }

    fn scroll_choice(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        selected: bool,
        hx: &HarnessTheme,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.scroll_button(id, label, hx, cx)
            .aria_toggled(toggled(selected))
    }

    fn reveal_on_focus(&self, control: Stateful<Div>, focus: FocusHandle) -> Stateful<Div> {
        let scroll = self.scroll.clone();
        let revealed = self.revealed.clone();
        control.child(
            gpui::canvas(
                move |bounds, window, _| {
                    let shared = revealed.clone();
                    let mut revealed = revealed.borrow_mut();
                    if !focus.is_focused(window) {
                        if revealed.as_ref() == Some(&focus) {
                            *revealed = None;
                        }
                        return;
                    }
                    if revealed.as_ref() == Some(&focus) {
                        return;
                    }
                    *revealed = Some(focus.clone());

                    let viewport = scroll.bounds();
                    let dy = if bounds.top() < viewport.top() {
                        viewport.top() - bounds.top()
                    } else {
                        (viewport.bottom() - bounds.bottom()).min(px(0.0))
                    };
                    if dy != px(0.0) {
                        let scroll = scroll.clone();
                        let focus = focus.clone();
                        window.on_next_frame(move |window, _| {
                            // A section change in between (`reset_scroll`)
                            // cleared the reveal and the offset; the new
                            // section starts at the top.
                            if !reveal_still_pending(shared.borrow().as_ref(), &focus) {
                                return;
                            }
                            let mut offset = scroll.offset();
                            offset.y += dy;
                            scroll.set_offset(offset);
                            window.refresh();
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
    }
}

/// Whether a scroll queued to reveal `focus` still applies: the control
/// still holds the reveal (`reset_scroll`, on a section change, clears it).
fn reveal_still_pending<T: PartialEq>(revealed: Option<&T>, focus: &T) -> bool {
    revealed == Some(focus)
}

fn modal_control(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    role: gpui::Role,
    hx: &HarnessTheme,
) -> Stateful<Div> {
    div()
        .id(id)
        .role(role)
        .aria_label(label)
        .tab_index(0)
        .rounded_md()
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(|style| style.border_color(hx.accent).bg(tint(hx.accent, 0.22)))
}

fn modal_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    hx: &HarnessTheme,
) -> Stateful<Div> {
    modal_control(id, label, gpui::Role::Button, hx)
}

fn toggled(on: bool) -> gpui::Toggled {
    if on {
        gpui::Toggled::True
    } else {
        gpui::Toggled::False
    }
}

#[cfg(test)]
mod tests {
    use super::reveal_still_pending;

    #[test]
    fn a_queued_reveal_applies_only_while_its_control_holds_it() {
        assert!(reveal_still_pending(Some(&1), &1));
        // `reset_scroll` (a section change) cleared it before the frame.
        assert!(!reveal_still_pending(None, &1));
        // Another control took the reveal meanwhile.
        assert!(!reveal_still_pending(Some(&2), &1));
    }
}
