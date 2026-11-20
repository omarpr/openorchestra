//! The always-on conductor's chrome: frame rendering kicked off in the
//! background, the tick loop that animates him, dragging him around, and the
//! speech bubble. The state machine itself lives in `crate::conductor`.

use super::super::*;

use gpui::{RenderImage, img};

use crate::browser_pane::Hole;
use crate::conductor::scene::FRAME_PX;

/// The figure's on-screen size; frames are rendered at 2× this.
const FIGURE: f32 = (FRAME_PX / 2) as f32;
/// Default distance from the window's bottom-right corner.
const DEFAULT_OFFSET: [f32; 2] = [16.0, 36.0];
/// Pointer travel that turns a press into a drag rather than a click.
const DRAG_SLOP: f32 = 3.0;

/// A press on the conductor, which becomes a drag once it moves.
pub(crate) struct ConductorDrag {
    mouse: gpui::Point<Pixels>,
    start: [f32; 2],
    moved: bool,
    /// Where a plain click (no drag) goes: the caption's agent.
    nav: Option<String>,
}

/// The theme accent as the renderer wants it.
fn accent_of(hx: &HarnessTheme) -> [f32; 3] {
    [hx.accent.r, hx.accent.g, hx.accent.b]
}

/// What the conductor's drawn children cut out of a browser page, from
/// their laid-out `bounds` and, in the same order, each one's corner radius
/// (`None`: it cuts nothing, like the full-window drag shield).
pub(crate) fn conductor_holes(
    bounds: &[gpui::Bounds<Pixels>],
    radii: &[Option<f32>],
) -> Vec<Hole> {
    bounds
        .iter()
        .zip(radii)
        .filter_map(|(b, r)| r.map(|r| Hole::from_bounds(*b, r)))
        .collect()
}

/// "(repo)" for a main checkout, "(repo · worktree)" otherwise.
pub(crate) fn conductor_label(label: &str, repo: &str) -> String {
    match (repo.is_empty(), label) {
        (true, l) => format!("({l})"),
        (false, "main") => format!("({repo})"),
        (false, l) => format!("({repo} · {l})"),
    }
}

impl HarnessWorkspace {
    /// Start what the conductor needs while he's switched on: his frames
    /// (re-rendered when the accent changes, since his bow tie wears it) and
    /// the tick loop. Idempotent; called every render.
    pub(crate) fn ensure_conductor(&mut self, hx: &HarnessTheme, cx: &mut Context<Self>) {
        if !self.prefs.conductor_always_on {
            return;
        }
        let accent = accent_of(hx);
        if self.conductor_frames_accent != Some(accent) {
            self.conductor_frames_accent = Some(accent);
            cx.spawn(async move |view, cx| {
                let raw = cx
                    .background_executor()
                    .spawn(async move { crate::conductor::scene::render_all(accent) })
                    .await;
                let frames: Vec<Arc<RenderImage>> = raw
                    .into_iter()
                    .filter_map(|bytes| image::RgbaImage::from_raw(FRAME_PX, FRAME_PX, bytes))
                    .map(|buf| Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
                    .collect();
                let _ = view.update(cx, |ws, cx| {
                    // A newer accent arrived while this one rendered.
                    if ws.conductor_frames_accent != Some(accent) {
                        return;
                    }
                    if let Some(old) = ws.conductor_frames.replace(frames) {
                        for image in old {
                            cx.drop_image(image, None);
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        if !self.conductor_ticking {
            self.conductor_ticking = true;
            cx.spawn(async move |view, cx| {
                loop {
                    let Ok(interval) = view.update(cx, |ws, _| ws.conductor.tick_interval()) else {
                        break;
                    };
                    cx.background_executor().timer(interval).await;
                    let keep = view
                        .update(cx, |ws, cx| ws.conductor_tick(cx))
                        .unwrap_or(false);
                    if !keep {
                        break;
                    }
                }
            })
            .detach();
        }
    }

    /// One animation step. Re-renders only when the picture or caption
    /// actually changed, so a resting conductor costs a wake-up, not a frame.
    fn conductor_tick(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.prefs.conductor_always_on {
            self.conductor_ticking = false;
            self.conductor_view = None;
            cx.notify();
            return false;
        }
        let selected = self
            .selected_agent()
            .map(|(target, kind)| self.agent_selection_target(target, kind));
        // `tick` needs the workspace (to name alert targets) while mutating
        // the conductor, so lend it out for the call.
        let mut conductor = std::mem::take(&mut self.conductor);
        let view = conductor.tick(
            std::time::Instant::now(),
            selected.as_ref(),
            |target, kind| {
                let (_, _, id) = self.alert_target(&target.root);
                super::super::state::alert_nav_id(&id, kind, target.tab_id)
            },
        );
        self.conductor = conductor;
        if self.conductor_view.as_ref() != Some(&view) {
            self.conductor_view = Some(view);
            cx.notify();
        }
        true
    }

    /// Follow the pointer while the conductor is being dragged. Returns
    /// whether a conductor drag consumed the move.
    pub(crate) fn conductor_drag_move(
        &mut self,
        ev: &MouseMoveEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(drag) = &mut self.conductor_drag else {
            return false;
        };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.conductor_drag = None;
            save_prefs_to(&self.prefs_path, &self.prefs);
            return true;
        }
        let dx = f32::from(ev.position.x - drag.mouse.x);
        let dy = f32::from(ev.position.y - drag.mouse.y);
        if !drag.moved && dx.abs() + dy.abs() < DRAG_SLOP {
            return true;
        }
        drag.moved = true;
        let vp = window.viewport_size();
        // Anchored to the bottom-right, so moving right shrinks `right`.
        let right = (drag.start[0] - dx).clamp(0.0, (f32::from(vp.width) - FIGURE).max(0.0));
        let bottom = (drag.start[1] - dy).clamp(0.0, (f32::from(vp.height) - FIGURE).max(0.0));
        self.prefs.conductor_offset = Some([right, bottom]);
        cx.notify();
        true
    }

    /// Finish a press on the conductor: save where a drag left him, or treat
    /// a still press on the caption as "take me to that agent".
    pub(crate) fn conductor_drag_end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.conductor_drag.take() else {
            return;
        };
        if drag.moved {
            save_prefs_to(&self.prefs_path, &self.prefs);
        } else if let Some(nav) = drag.nav.filter(|n| !n.is_empty()) {
            self.navigate_to_alert(&nav, window, cx);
            // Clicking his line is checking on that agent, even when you were
            // already there (so the next tick sees no arrival to dismiss on).
            self.dismiss_selected_agent();
        }
        cx.notify();
    }

    /// Pointer tracking for anything that is part of the conductor.
    ///
    /// The root element's move/up listeners can't do this alone: gpui only
    /// delivers a move to an element whose hitbox is hovered, and the
    /// conductor occludes what's under him so clicks on him don't fall
    /// through. Mid-drag he stays under the pointer, so the root heard
    /// nothing until the pointer outran his edge — and he jumped to catch
    /// up. Listening on him too gets every move. (Both firing is harmless:
    /// the position is start + delta, and ending twice is a no-op.)
    fn track_conductor_pointer(
        &self,
        el: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        el.on_mouse_move(cx.listener(|ws, ev: &MouseMoveEvent, window, cx| {
            ws.conductor_drag_move(ev, window, cx);
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|ws, _: &MouseUpEvent, window, cx| ws.conductor_drag_end(window, cx)),
        )
    }

    fn start_conductor_drag(
        &mut self,
        ev: &MouseDownEvent,
        nav: Option<String>,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.conductor_drag = Some(ConductorDrag {
            mouse: ev.position,
            start: self.prefs.conductor_offset.unwrap_or(DEFAULT_OFFSET),
            moved: false,
            nav,
        });
        cx.notify();
    }

    /// The conductor and his speech bubble, floating over everything but
    /// modal overlays. The bubble sits on whichever side has room.
    ///
    /// A browser page is native and sits above all of this, so he and his
    /// bubble or badge are cut out of it each frame, from their laid-out
    /// bounds (`browser_pane::set_punch_through`): he shows through the
    /// page and takes the pointer there. No conductor, no holes.
    pub(crate) fn render_conductor(
        &self,
        hx: &HarnessTheme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let conductor = self.conductor_element(hx, window, cx);
        if conductor.is_none() {
            crate::browser_pane::set_punch_through(window, Vec::new());
        }
        conductor
    }

    fn conductor_element(
        &self,
        hx: &HarnessTheme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.prefs.conductor_always_on {
            return None;
        }
        let view = self.conductor_view.as_ref()?;
        let frame = self.conductor_frames.as_ref()?.get(view.frame)?.clone();
        let vp = window.viewport_size();
        let (vw, vh) = (f32::from(vp.width), f32::from(vp.height));
        // Re-clamp on every render: the window may have shrunk since he was
        // put there, and he must never end up somewhere he can't be grabbed.
        let [right, bottom] = self.prefs.conductor_offset.unwrap_or(DEFAULT_OFFSET);
        let right = right.clamp(0.0, (vw - FIGURE).max(0.0));
        let bottom = bottom.clamp(0.0, (vh - FIGURE).max(0.0));
        // Closed hand from the press on: gpui keeps the element's cursor at
        // the moment its drag starts for the whole drag.
        let dragging = self.conductor_drag.is_some();

        let figure = div()
            .id("conductor")
            .occlude()
            .absolute()
            .right(px(right))
            .bottom(px(bottom))
            .w(px(FIGURE))
            .h(px(FIGURE))
            .cursor(if dragging {
                gpui::CursorStyle::ClosedHand
            } else {
                gpui::CursorStyle::OpenHand
            })
            .child(img(frame).size_full())
            .on_hover(cx.listener(|ws, hovered: &bool, _, cx| {
                ws.conductor_hovered = *hovered;
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, ev: &MouseDownEvent, _, cx| ws.start_conductor_drag(ev, None, cx)),
            );
        let figure = self.track_conductor_pointer(figure, cx);

        // Progress alone ("Running swift build… 21s") collapses to a small
        // badge so it stops covering whatever is under it; a wait or a
        // "finished" opens the full bubble, and so does hovering him.
        let collapsed = !view.needs_you() && !self.conductor_hovered && !dragging;
        let on_left = right + FIGURE / 2.0 < vw / 2.0;
        let badge = (collapsed && view.running > 0).then(|| {
            let n = view.running;
            let b = div()
                .id("conductor-badge")
                .occlude()
                .absolute()
                .bottom(px(bottom + FIGURE * 0.42))
                .flex()
                .items_center()
                .gap_1p5()
                .pl_2()
                .pr_1p5()
                .py_0p5()
                .rounded_full()
                .bg(hx.surface2)
                .border_1()
                .border_color(hx.border2)
                .text_xs()
                .text_color(hx.tx1)
                .whitespace_nowrap()
                .child(if n == 1 {
                    "1 agent working".to_string()
                } else {
                    format!("{n} agents working")
                })
                // Same arc as the sidebar's RUNNING pill, on the same clock,
                // so the badge and the rows it summarises spin together.
                .child(spin_arc(
                    hx.accent,
                    tint(hx.accent, 0.3),
                    10.0,
                    self.anim_secs(),
                ))
                .on_hover(cx.listener(|ws, hovered: &bool, _, cx| {
                    ws.conductor_hovered = *hovered;
                    cx.notify();
                }));
            let b = if on_left {
                b.right(px(right + FIGURE + 2.0))
            } else {
                b.left(px(vw - right + 2.0))
            };
            self.track_conductor_pointer(b, cx)
        });
        let bubble = (!collapsed && !view.lines.is_empty()).then(|| {
            // On the left of him when he's in the right half of the window,
            // on his right otherwise, so it never runs off-screen.
            let gap = 2.0;
            let mut b = div()
                .id("conductor-caption")
                .occlude()
                .absolute()
                // Anchored at the bottom so extra lines grow upward, away
                // from him.
                .bottom(px(bottom + FIGURE * 0.42))
                .w(px(230.0))
                .rounded_lg()
                .bg(hx.surface2)
                .border_1()
                .border_color(hx.border2)
                .shadow_md()
                .p_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .on_hover(cx.listener(|ws, hovered: &bool, _, cx| {
                    ws.conductor_hovered = *hovered;
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|ws, ev: &MouseDownEvent, _, cx| {
                        ws.start_conductor_drag(ev, None, cx)
                    }),
                );
            for (i, line) in view.lines.iter().enumerate() {
                let nav = line.nav.clone();
                b = b.child(
                    div()
                        .id(("conductor-line", i))
                        .px_1p5()
                        .py_0p5()
                        .rounded_md()
                        .flex()
                        .flex_col()
                        .when(i > 0, |d| d.border_t_1().border_color(hx.border))
                        .when(!line.nav.is_empty(), |d| {
                            d.cursor_pointer().hover(|d| d.bg(hx.surface_hover))
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(hx.tx2)
                                .truncate()
                                .child(line.label.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(if line.announcement { hx.accent } else { hx.tx0 })
                                .child(line.text.clone()),
                        )
                        // Each line is its own way in: a still click goes to
                        // that agent, a drag still moves him.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                                ws.start_conductor_drag(ev, Some(nav.clone()), cx)
                            }),
                        ),
                );
            }
            if view.more > 0 {
                b = b.child(
                    div()
                        .px_1p5()
                        .pt_0p5()
                        .border_t_1()
                        .border_color(hx.border)
                        .text_xs()
                        .text_color(hx.tx2)
                        .child(format!("+{} more", view.more)),
                );
            }
            b = if on_left {
                b.right(px(right + FIGURE + gap))
            } else {
                b.left(px(vw - right + gap))
            };
            self.track_conductor_pointer(b, cx)
        });

        // While he's being dragged, an invisible full-window shield sits just
        // under him and takes every pointer event. Without it, the moment the
        // pointer outran him it was over whatever lay beneath — and zed's
        // terminal forwards every hovered move to the program inside (button
        // state included, active gpui drag or not). Under tmux's mouse mode
        // that reads as a selection drag: copy mode, highlighted text.
        let shield = self.conductor_drag.is_some().then(|| {
            let s = div()
                .id("conductor-drag-shield")
                .occlude()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .cursor(gpui::CursorStyle::ClosedHand);
            self.track_conductor_pointer(s, cx)
        });

        // What each child below cuts out of a browser page, in child order:
        // the bubble with its `rounded_lg` corners (half a rem), the badge
        // as a pill (clamped to half its height), him as a square. Not the
        // shield: it covers the whole window, and only while dragging.
        let radii: Vec<Option<f32>> = shield
            .is_some()
            .then_some(None)
            .into_iter()
            .chain(bubble.is_some().then_some(Some(f32::from(window.rem_size()) * 0.5)))
            .chain(badge.is_some().then_some(Some(f32::MAX)))
            .chain([Some(0.0)])
            .collect();

        Some(
            div()
                .absolute()
                .size_full()
                .top_0()
                .left_0()
                .on_children_prepainted(move |bounds, window, _| {
                    crate::browser_pane::set_punch_through(window, conductor_holes(&bounds, &radii));
                })
                .children(shield)
                .children(bubble)
                .children(badge)
                .child(figure)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, point, px, size};

    use super::{conductor_holes, conductor_label};
    use crate::browser_pane::Hole;

    #[test]
    fn the_shield_cuts_nothing_and_the_rest_keep_their_corners() {
        let b = |x: f32, y: f32, w: f32, h: f32| Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        // While dragging with the bubble open: shield, bubble, figure.
        let bounds = [b(0.0, 0.0, 1200.0, 800.0), b(900.0, 680.0, 230.0, 60.0), b(1120.0, 700.0, 64.0, 64.0)];
        let radii = [None, Some(8.0), Some(0.0)];
        assert_eq!(
            conductor_holes(&bounds, &radii),
            [
                Hole { x: 900.0, y: 680.0, w: 230.0, h: 60.0, radius: 8.0 },
                Hole { x: 1120.0, y: 700.0, w: 64.0, h: 64.0, radius: 0.0 },
            ]
        );
        // Him alone.
        assert_eq!(conductor_holes(&bounds[2..], &[Some(0.0)]).len(), 1);
        assert!(conductor_holes(&[], &[]).is_empty());
    }

    #[test]
    fn labels_name_the_project_and_any_worktree() {
        assert_eq!(conductor_label("main", "app"), "(app)");
        assert_eq!(conductor_label("fix-login", "app"), "(app · fix-login)");
        assert_eq!(conductor_label("somewhere", ""), "(somewhere)");
    }
}
