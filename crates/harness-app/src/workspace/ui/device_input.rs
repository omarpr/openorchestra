//! The Device tab's picture as the simulator's touch screen and keyboard:
//! a click is a tap, a drag a swipe, a trackpad or wheel scroll a short
//! synthesised swipe (the device has no scroll input that works), and keys
//! typed while the picture has focus go to the device. Esc, or a click
//! elsewhere, gives the keyboard back. ⌘ chords stay the app's, except ⌘V,
//! which types the Mac's clipboard.
//!
//! Everything goes through one `oo-device-input` helper
//! (`devices::input`) for the device on screen, started on the first input
//! and ended when that device stops being shown. Without the helper, or once
//! it has failed, the picture is watch-only and says so in a tooltip.
//!
//! Clicks map from the rectangle the picture was last painted at (on the
//! device-pixel grid, inside the frame) and the turn it was drawn upright
//! with, back to the portrait screen the helper speaks.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender;
use gpui::{
    Bounds, DispatchPhase, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent, Subscription,
    TouchPhase as GestureStep, WeakEntity,
};

use super::super::*;
use super::device_panel::DeviceScreen;
use crate::devices::Quarter;
use crate::devices::input::{
    HelperEvent, InputHelper, KeyAction, Retry, TouchPhase, UNAVAILABLE_TIP, helper_path,
    key_action,
};

/// How long after its last scroll a wheel's synthesised swipe lifts its
/// finger (a wheel says nothing when it stops).
pub(crate) const SWIPE_IDLE: Duration = Duration::from_millis(120);
/// The same for a trackpad gesture, which ends itself: only in case its end
/// never comes (the pointer left the picture, say).
pub(crate) const GESTURE_IDLE: Duration = Duration::from_millis(600);
/// A wheel's line, in view pixels.
const SCROLL_LINE: Pixels = px(20.0);
/// The focus ring's width, and its gap from the device.
const RING: f32 = 2.0;
const RING_GAP: f32 = 3.0;

/// (u, v) in the upright picture, as the portrait surface's (x, y): the
/// inverse of the turn it was drawn with (see "Spike findings" in the
/// Device controls design: uiOrientation 3 is (v, 1 − u), 4 is (1 − v, u)).
fn portrait(u: f64, v: f64, turn: Quarter) -> (f64, f64) {
    match turn {
        Quarter::None => (u, v),
        Quarter::Cw90 => (v, 1.0 - u),
        Quarter::Cw180 => (1.0 - u, 1.0 - v),
        Quarter::Cw270 => (1.0 - v, u),
    }
}

/// Where `point` falls in `screen`, as 0…1 ratios across and down. None
/// for an empty screen.
fn ratios(point: Point<Pixels>, screen: Bounds<Pixels>) -> Option<(f64, f64)> {
    let (w, h) = (f64::from(f32::from(screen.size.width)), f64::from(f32::from(screen.size.height)));
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let u = f64::from(f32::from(point.x - screen.origin.x)) / w;
    let v = f64::from(f32::from(point.y - screen.origin.y)) / h;
    Some((u, v))
}

/// A point on the drawn screen `screen` (the picture as painted, turned
/// upright by `turn`) as the 0…1 ratios of the portrait screen the helper
/// takes. None outside the screen.
pub(crate) fn view_to_device(
    point: Point<Pixels>,
    screen: Bounds<Pixels>,
    turn: Quarter,
) -> Option<(f64, f64)> {
    let (u, v) = ratios(point, screen)?;
    let on = |r: f64| (0.0..=1.0).contains(&r);
    (on(u) && on(v)).then(|| portrait(u, v, turn))
}

/// [`view_to_device`] for a finger already down: off the screen, it stays
/// at the screen's edge.
pub(crate) fn clamped_to_device(point: Point<Pixels>, screen: Bounds<Pixels>, turn: Quarter) -> (f64, f64) {
    let (u, v) = ratios(point, screen).unwrap_or((0.5, 0.5));
    portrait(u.clamp(0.0, 1.0), v.clamp(0.0, 1.0), turn)
}

fn inside(point: Point<Pixels>, screen: Bounds<Pixels>) -> bool {
    ratios(point, screen).is_some_and(|(u, v)| (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v))
}

fn kept_on(point: Point<Pixels>, screen: Bounds<Pixels>) -> Point<Pixels> {
    let max = screen.origin + gpui::point(screen.size.width, screen.size.height);
    gpui::point(
        point.x.clamp(screen.origin.x, max.x),
        point.y.clamp(screen.origin.y, max.y),
    )
}

/// Where a trackpad gesture is, as far as its swipe goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Gesture {
    /// None, or a wheel (which has no phases).
    #[default]
    Free,
    /// Fingers on the trackpad.
    Touching,
    /// They lifted: what follows until the next gesture is the Mac's
    /// momentum.
    Tail,
}

/// The finger a scroll drags, in view pixels: down where the pointer is on
/// the first delta that moves, then moved by each delta. It lifts when the
/// gesture ends, or at the screen's edge (the next delta starts another),
/// or once the scroll goes quiet ([`Swipe::lift_after`]). A delta of
/// nothing never touches the device (a resting or lifting hand isn't a
/// tap), and the momentum after a trackpad gesture is left to iOS, which
/// flings the lifted finger itself.
#[derive(Debug, Default)]
pub(crate) struct Swipe {
    at: Option<Point<Pixels>>,
    gesture: Gesture,
}

impl Swipe {
    /// One scroll event: `step` its gesture phase, `precise` whether its
    /// delta came in pixels (a trackpad or Magic Mouse, whose momentum does
    /// too) rather than a wheel's lines.
    pub(crate) fn scroll(
        &mut self,
        pointer: Point<Pixels>,
        delta: Point<Pixels>,
        step: GestureStep,
        precise: bool,
        screen: Bounds<Pixels>,
    ) -> Vec<(TouchPhase, Point<Pixels>)> {
        match step {
            GestureStep::Started => self.gesture = Gesture::Touching,
            GestureStep::Ended | GestureStep::Cancelled => {
                self.gesture = Gesture::Tail;
                return self.end().into_iter().collect();
            }
            GestureStep::Moved if self.gesture == Gesture::Tail && precise => return Vec::new(),
            GestureStep::Moved if self.gesture == Gesture::Tail => self.gesture = Gesture::Free,
            GestureStep::Moved => {}
        }
        if delta == Point::default() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let from = match self.at {
            Some(at) => at,
            None if inside(pointer, screen) => {
                out.push((TouchPhase::Down, pointer));
                pointer
            }
            None => return out,
        };
        let to = from + delta;
        let kept = kept_on(to, screen);
        out.push((TouchPhase::Move, kept));
        if kept == to {
            self.at = Some(kept);
        } else {
            out.push((TouchPhase::Up, kept));
            self.at = None;
        }
        out
    }

    /// The scroll is over: lift the finger, if one is down.
    pub(crate) fn end(&mut self) -> Option<(TouchPhase, Point<Pixels>)> {
        self.at.take().map(|at| (TouchPhase::Up, at))
    }

    /// With a finger down, how long without scrolling before it lifts.
    pub(crate) fn lift_after(&self) -> Option<Duration> {
        self.at?;
        Some(if self.gesture == Gesture::Touching { GESTURE_IDLE } else { SWIPE_IDLE })
    }
}

/// What a mouse press on the picture's element does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Press {
    /// Touch the device there, and take the keyboard.
    Touch,
    /// Give the keyboard back: the press was beside the screen (the frame,
    /// the panel), or the picture takes no input.
    Unfocus,
    /// Not the left button: nothing.
    Ignore,
}

/// `left`: the left button; `on_screen`: inside the screen as painted;
/// `takes_input`: the picture can take input now.
pub(crate) fn press_action(left: bool, on_screen: bool, takes_input: bool) -> Press {
    match (left, on_screen && takes_input) {
        (false, _) => Press::Ignore,
        (true, true) => Press::Touch,
        (true, false) => Press::Unfocus,
    }
}

/// Where the picture was last painted, and the turn it was drawn with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Hit {
    pub screen: Bounds<Pixels>,
    pub turn: Quarter,
}

/// The input half of a [`DeviceScreen`].
pub(crate) struct ScreenInput {
    /// The device shown, which input goes to.
    target: Option<String>,
    helper: Option<InputHelper>,
    /// Numbers each helper start; events from older ones are dropped.
    serial: u64,
    /// The current helper failed (and none has come up since).
    failed: bool,
    /// Failed starts, for the device they were for.
    retry: Option<(String, Retry)>,
    events: UnboundedSender<HelperEvent>,
    /// Where the picture was last painted; None while it takes no input.
    /// The paint sets it.
    hit: Rc<Cell<Option<Hit>>>,
    /// A mouse finger is down (the paint's window listeners follow it),
    /// and where it is.
    touching: Rc<Cell<bool>>,
    touch_at: Point<Pixels>,
    /// The scroll's finger, and a count of scroll events for its idle lift.
    swipe: Swipe,
    swipe_ticket: u64,
    pub(crate) focus: FocusHandle,
    /// What had the keyboard before the picture took it.
    previous_focus: Option<FocusHandle>,
    focus_watch: Vec<Subscription>,
}

impl ScreenInput {
    pub(crate) fn new(cx: &mut Context<DeviceScreen>) -> Self {
        let (events, mut rx) = futures::channel::mpsc::unbounded();
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                if this.update(cx, |screen, cx| screen.on_helper_event(event, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            target: None,
            helper: None,
            serial: 0,
            failed: false,
            retry: None,
            events,
            hit: Rc::default(),
            touching: Rc::default(),
            touch_at: Point::default(),
            swipe: Swipe::default(),
            swipe_ticket: 0,
            focus: cx.focus_handle(),
            previous_focus: None,
            focus_watch: Vec::new(),
        }
    }
}

/// What the picture's paint needs for input, taken at render.
pub(crate) struct InputPaint {
    hit: Rc<Cell<Option<Hit>>>,
    touching: Rc<Cell<bool>>,
    this: WeakEntity<DeviceScreen>,
    interactive: bool,
    /// The focus ring's colour, while the picture has the keyboard.
    ring: Option<gpui::Hsla>,
}

impl InputPaint {
    /// The picture was painted at `screen` (with the device's frame, if
    /// any, at `device`), drawn upright by `turn`.
    pub(crate) fn paint(
        self,
        screen: Bounds<Pixels>,
        device: Bounds<Pixels>,
        turn: Quarter,
        window: &mut Window,
    ) {
        if !self.interactive {
            self.hit.set(None);
            return;
        }
        self.hit.set(Some(Hit { screen, turn }));
        // A finger down follows the mouse anywhere in the window, and
        // always lifts.
        let (touching, this) = (self.touching.clone(), self.this.clone());
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Capture && touching.get() {
                let _ = this.update(cx, |screen, _| {
                    // A release missed (outside the window, say): lift.
                    if e.pressed_button == Some(MouseButton::Left) {
                        screen.drag_to(e.position)
                    } else {
                        screen.lift(e.position)
                    }
                });
            }
        });
        let (touching, this) = (self.touching, self.this);
        window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Capture && e.button == MouseButton::Left && touching.get() {
                let _ = this.update(cx, |screen, _| screen.lift(e.position));
            }
        });
        if let Some(colour) = self.ring {
            let ring = device.dilate(px(RING_GAP + RING));
            let radius = f32::from(ring.size.width.min(ring.size.height)) * 0.12;
            window.paint_quad(gpui::quad(
                ring,
                px(radius),
                gpui::transparent_black(),
                px(RING),
                colour,
                gpui::BorderStyle::Solid,
            ));
        }
    }
}

impl DeviceScreen {
    /// The device input goes to: the one on screen, or None while nothing
    /// is. A new device (or none) ends the old one's helper.
    pub(crate) fn set_input_target(&mut self, udid: Option<String>, cx: &mut Context<Self>) {
        if self.input.target == udid {
            return;
        }
        self.stop_input();
        self.input.failed = false;
        self.input.target = udid;
        cx.notify();
    }

    /// Lifts any finger and ends the helper.
    fn stop_input(&mut self) {
        self.lift_all();
        self.input.helper = None;
    }

    /// Lifts the mouse's finger and the scroll's, if down. Never starts a
    /// helper to do it: fingers are only down on a running one.
    fn lift_all(&mut self) {
        let swiping = self.input.swipe.end();
        if self.input.helper.is_none() {
            self.input.touching.set(false);
            return;
        }
        let at = self.input.touch_at;
        self.lift(at);
        if let (Some((phase, at)), Some(hit)) = (swiping, self.input.hit.get()) {
            self.send_touch(phase, at, hit);
        }
    }

    /// Whether the picture is watch-only: no helper in this build, or the
    /// device's last one failed (the next input may try again) or failed
    /// too often.
    pub(crate) fn watch_only(&self) -> bool {
        self.input.target.is_some() && (!self.takes_input() || self.input.failed)
    }

    /// Whether input may go to the device on screen: there is a helper, and
    /// it hasn't failed for this device too often.
    fn takes_input(&self) -> bool {
        let Some(udid) = &self.input.target else {
            return false;
        };
        let gave_up = matches!(&self.input.retry, Some((u, r)) if u == udid && r.gave_up());
        helper_path().is_some() && !gave_up
    }

    /// The helper for the device on screen, started if need be (and
    /// allowed: a failing one is retried a few times, a while apart).
    fn helper(&mut self) -> Option<&InputHelper> {
        if self.input.helper.is_none() {
            let udid = self.input.target.clone()?;
            helper_path()?;
            if self.input.retry.as_ref().is_none_or(|(u, _)| *u != udid) {
                self.input.retry = Some((udid.clone(), Retry::default()));
            }
            let may = self.input.retry.as_ref().is_some_and(|(_, r)| r.may_start(Instant::now()));
            if !may {
                return None;
            }
            self.input.serial += 1;
            match InputHelper::spawn(&udid, self.input.serial, self.input.events.clone()) {
                Ok(helper) => self.input.helper = Some(helper),
                Err(e) => {
                    log::info!("device: no input: {e}");
                    return None;
                }
            }
        }
        self.input.helper.as_ref()
    }

    fn on_helper_event(&mut self, event: HelperEvent, cx: &mut Context<Self>) {
        let current = |serial| serial == self.input.serial && self.input.helper.is_some();
        match event {
            HelperEvent::Ready(serial) if current(serial) => {
                log::debug!("device: input ready");
                self.input.failed = false;
                cx.notify();
            }
            HelperEvent::Worked(serial) if current(serial) => {
                if let Some((_, retry)) = &mut self.input.retry {
                    retry.worked();
                }
            }
            HelperEvent::Failed(serial, why) if current(serial) => {
                log::info!("device: simulator input stopped: {why}");
                self.input.helper = None;
                self.input.touching.set(false);
                self.input.swipe = Swipe::default();
                self.input.failed = true;
                if let Some((_, retry)) = &mut self.input.retry {
                    retry.failed(Instant::now());
                }
                cx.notify();
            }
            _ => {}
        }
    }

    /// Home through the helper, for the bar: the receiver gets its answer.
    /// None when input can't go to `udid` (the bar then brings SpringBoard
    /// forward instead).
    pub(crate) fn press_home(&mut self, udid: &str) -> Option<Receiver<Result<(), String>>> {
        if self.input.target.as_deref() != Some(udid) {
            return None;
        }
        self.helper().map(InputHelper::home)
    }

    fn send_touch(&mut self, phase: TouchPhase, at: Point<Pixels>, hit: Hit) {
        let (x, y) = clamped_to_device(at, hit.screen, hit.turn);
        if let Some(helper) = self.helper() {
            helper.touch(phase, x, y);
        }
    }

    /// Any press on the picture's element, in the capture phase (before
    /// gpui's own focus-on-click, which this stands in for).
    fn mouse_pressed(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.prevent_default();
        let hit = self.input.hit.get();
        let on_screen =
            hit.is_some_and(|hit| view_to_device(event.position, hit.screen, hit.turn).is_some());
        let action = press_action(event.button == MouseButton::Left, on_screen, hit.is_some());
        match (action, hit) {
            (Press::Touch, Some(hit)) => {
                if !self.input.focus.is_focused(window) {
                    self.input.previous_focus = window.focused(cx);
                    window.focus(&self.input.focus, cx);
                }
                self.lift_all();
                self.send_touch(TouchPhase::Down, event.position, hit);
                self.input.touching.set(self.input.helper.is_some());
                self.input.touch_at = event.position;
            }
            (Press::Unfocus, _) => self.unfocus(window, cx),
            _ => {}
        }
    }

    fn drag_to(&mut self, position: Point<Pixels>) {
        if self.input.touching.get()
            && let Some(hit) = self.input.hit.get()
        {
            self.input.touch_at = position;
            self.send_touch(TouchPhase::Move, position, hit);
        }
    }

    fn lift(&mut self, position: Point<Pixels>) {
        if self.input.touching.replace(false)
            && let Some(hit) = self.input.hit.get()
        {
            self.send_touch(TouchPhase::Up, position, hit);
        }
    }

    fn scrolled(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let Some(hit) = self.input.hit.get() else {
            return;
        };
        if self.input.touching.get() {
            return;
        }
        let precise = matches!(event.delta, gpui::ScrollDelta::Pixels(_));
        let delta = event.delta.pixel_delta(SCROLL_LINE);
        let swiped = self.input.swipe.scroll(event.position, delta, event.touch_phase, precise, hit.screen);
        for (phase, at) in swiped {
            self.send_touch(phase, at, hit);
        }
        // Lift the finger once the scrolling stops.
        let Some(quiet) = self.input.swipe.lift_after() else {
            return;
        };
        self.input.swipe_ticket += 1;
        let ticket = self.input.swipe_ticket;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(quiet).await;
            let _ = this.update(cx, |screen, _| {
                if screen.input.swipe_ticket == ticket
                    && let (Some((phase, at)), Some(hit)) =
                        (screen.input.swipe.end(), screen.input.hit.get())
                {
                    screen.send_touch(phase, at, hit);
                }
            });
        })
        .detach();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match key_action(&event.keystroke) {
            KeyAction::Pass => {}
            KeyAction::Unfocus => {
                self.unfocus(window, cx);
                cx.stop_propagation();
            }
            KeyAction::Paste => {
                let text = cx.read_from_clipboard().and_then(|item| item.text());
                if let (Some(text), Some(helper)) = (text, self.helper()) {
                    helper.text(&text);
                }
                cx.stop_propagation();
            }
            KeyAction::Press { usage, shift } => {
                if let Some(helper) = self.helper() {
                    helper.press(usage, shift);
                    cx.stop_propagation();
                }
            }
        }
    }

    /// Give the keyboard back to what had it before the picture.
    fn unfocus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input.focus.is_focused(window) {
            return;
        }
        match self.input.previous_focus.take() {
            Some(previous) => window.focus(&previous, cx),
            None => window.blur(cx),
        }
    }

    /// For the render: what the paint needs. `interactive`: the picture as
    /// drawn can be mapped back to the device.
    pub(crate) fn input_paint(
        &mut self,
        interactive: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> InputPaint {
        if self.input.focus_watch.is_empty() {
            // The ring follows the keyboard, wherever it goes.
            let focus = self.input.focus.clone();
            self.input.focus_watch = vec![
                cx.on_focus_in(&focus, window, |_, _, cx| cx.notify()),
                cx.on_focus_out(&focus, window, |_, _, _, cx| cx.notify()),
            ];
        }
        let interactive = interactive && self.takes_input();
        let focused = self.input.focus.is_focused(window);
        InputPaint {
            hit: self.input.hit.clone(),
            touching: self.input.touching.clone(),
            this: cx.entity().downgrade(),
            interactive,
            ring: (interactive && focused).then(|| crate::theme::theme(cx).accent.into()),
        }
    }

    /// No picture: nothing to touch.
    pub(crate) fn no_input_picture(&mut self) {
        self.input.hit.set(None);
    }

    /// The picture's element, taking input: `picture` is its canvas, and
    /// `mappable` false when it can't take input as drawn.
    pub(crate) fn input_element(
        &self,
        picture: AnyElement,
        mappable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let watch_only = self.watch_only() || self.input.target.is_some() && !mappable;
        div()
            .id("device-screen")
            .size_full()
            .track_focus(&self.input.focus)
            .capture_any_mouse_down(cx.listener(Self::mouse_pressed))
            .on_mouse_down_out(cx.listener(|screen, _: &MouseDownEvent, window, cx| {
                screen.unfocus(window, cx)
            }))
            .on_scroll(cx.listener(|screen, e: &ScrollEvent, _, cx| screen.scrolled(e, cx)))
            .on_key_down(cx.listener(Self::key_down))
            .when(watch_only, |d| d.tooltip(tip(UNAVAILABLE_TIP)))
            .child(picture)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::Quarter;
    use crate::devices::input::TouchPhase;
    use gpui::{Bounds, TouchPhase as Step, point, px, size};

    fn screen() -> Bounds<gpui::Pixels> {
        Bounds::new(point(px(10.0), px(20.0)), size(px(100.0), px(200.0)))
    }

    fn at(x: f32, y: f32) -> gpui::Point<gpui::Pixels> {
        point(px(x), px(y))
    }

    fn close(got: Option<(f64, f64)>, want: (f64, f64)) {
        let (x, y) = got.unwrap_or_else(|| panic!("no point, wanted {want:?}"));
        assert!((x - want.0).abs() < 1e-6 && (y - want.1).abs() < 1e-6, "{got:?} != {want:?}");
    }

    #[test]
    fn view_to_device_maps_corners_and_centre() {
        let s = screen();
        close(view_to_device(at(10.0, 20.0), s, Quarter::None), (0.0, 0.0));
        close(view_to_device(at(110.0, 20.0), s, Quarter::None), (1.0, 0.0));
        close(view_to_device(at(10.0, 220.0), s, Quarter::None), (0.0, 1.0));
        close(view_to_device(at(110.0, 220.0), s, Quarter::None), (1.0, 1.0));
        close(view_to_device(at(60.0, 120.0), s, Quarter::None), (0.5, 0.5));
        close(view_to_device(at(35.0, 70.0), s, Quarter::None), (0.25, 0.25));
    }

    #[test]
    fn points_outside_the_screen_are_ignored() {
        let s = screen();
        for p in [at(9.0, 100.0), at(111.0, 100.0), at(50.0, 19.0), at(50.0, 221.0), at(-5.0, -5.0)] {
            assert_eq!(view_to_device(p, s, Quarter::None), None, "{p:?}");
            assert_eq!(view_to_device(p, s, Quarter::Cw90), None, "{p:?}");
        }
        let empty = Bounds::new(point(px(10.0), px(20.0)), size(px(0.0), px(200.0)));
        assert_eq!(view_to_device(at(10.0, 50.0), empty, Quarter::None), None);
        // A drag that leaves the screen keeps to its edge.
        close(Some(clamped_to_device(at(-50.0, 120.0), s, Quarter::None)), (0.0, 0.5));
        close(Some(clamped_to_device(at(60.0, 999.0), s, Quarter::None)), (0.5, 1.0));
    }

    #[test]
    fn turned_points_map_back_to_portrait() {
        // A landscape picture, drawn upright: (u, v) = (0.25, 0.1) in it.
        let s = Bounds::new(point(px(0.0), px(0.0)), size(px(200.0), px(100.0)));
        let p = at(50.0, 10.0);
        close(view_to_device(p, s, Quarter::None), (0.25, 0.1));
        // uiOrientation 3: the UI's top at the surface's left edge.
        close(view_to_device(p, s, Quarter::Cw90), (0.1, 0.75));
        // uiOrientation 4: at its right edge.
        close(view_to_device(p, s, Quarter::Cw270), (0.9, 0.25));
        close(view_to_device(p, s, Quarter::Cw180), (0.75, 0.9));
        // The drawn top-left corner is the surface's bottom-left once turned
        // clockwise, and its top-right turned the other way.
        close(view_to_device(at(0.0, 0.0), s, Quarter::Cw90), (0.0, 1.0));
        close(view_to_device(at(0.0, 0.0), s, Quarter::Cw270), (1.0, 0.0));
        // Each turn and its mapping undo each other: the centre stays put.
        for turn in [Quarter::None, Quarter::Cw90, Quarter::Cw180, Quarter::Cw270] {
            close(view_to_device(at(100.0, 50.0), s, turn), (0.5, 0.5));
        }
    }

    fn d(y: f32) -> gpui::Point<gpui::Pixels> {
        gpui::point(px(0.0), px(y))
    }

    #[test]
    fn a_scroll_drags_one_finger_and_lifts_it_when_done() {
        let s = screen();
        let mut swipe = Swipe::default();
        let wheel = |swipe: &mut Swipe, y| swipe.scroll(at(60.0, 120.0), d(y), Step::Moved, false, s);
        // The first delta puts the finger down where the pointer is.
        let first = wheel(&mut swipe, -20.0);
        assert_eq!(first.len(), 2, "{first:?}");
        assert!(matches!(first[0], (TouchPhase::Down, _)));
        assert!(matches!(first[1], (TouchPhase::Move, _)));
        close(view_to_device(first[1].1, s, Quarter::None), (0.5, 0.4));
        // Then it follows the deltas, and lifts at the screen's edge.
        let next = wheel(&mut swipe, -500.0);
        let [(TouchPhase::Move, p), (TouchPhase::Up, q)] = next[..] else {
            panic!("{next:?}");
        };
        close(view_to_device(p, s, Quarter::None), (0.5, 0.0));
        assert_eq!(p, q);
        assert!(swipe.end().is_none());
        // The next delta puts another finger down, which the end lifts.
        let again = wheel(&mut swipe, 30.0);
        assert!(matches!(again[..], [(TouchPhase::Down, _), (TouchPhase::Move, _)]), "{again:?}");
        let up = swipe.end().unwrap();
        assert_eq!(up.0, TouchPhase::Up);
        close(view_to_device(up.1, s, Quarter::None), (0.5, 0.65));
        // A scroll that starts off the screen (on the frame) touches nothing.
        assert!(swipe.scroll(at(0.0, 0.0), d(30.0), Step::Moved, false, s).is_empty());
    }

    #[test]
    fn a_resting_trackpad_puts_no_finger_down() {
        let s = screen();
        let mut swipe = Swipe::default();
        // Two fingers resting (MayBegin/Began) come as Started with no delta:
        // nothing touches the device, or the rest would be a tap.
        assert!(swipe.scroll(at(60.0, 120.0), d(0.0), Step::Started, true, s).is_empty());
        assert!(swipe.scroll(at(60.0, 120.0), d(0.0), Step::Moved, true, s).is_empty());
        assert!(swipe.end().is_none());
        assert_eq!(swipe.lift_after(), None);
    }

    #[test]
    fn ending_a_scroll_only_lifts() {
        let s = screen();
        let mut swipe = Swipe::default();
        // Lifting the fingers without moving: no tap either.
        assert!(swipe.scroll(at(60.0, 120.0), d(0.0), Step::Started, true, s).is_empty());
        assert!(swipe.scroll(at(60.0, 120.0), d(0.0), Step::Ended, true, s).is_empty());
        // After a drag, the end lifts the finger where it is, and only that.
        swipe.scroll(at(60.0, 120.0), d(0.0), Step::Started, true, s);
        let dragged = swipe.scroll(at(60.0, 120.0), d(-20.0), Step::Moved, true, s);
        let up = swipe.scroll(at(60.0, 120.0), d(0.0), Step::Ended, true, s);
        assert_eq!(up, [(TouchPhase::Up, dragged[1].1)]);
    }

    #[test]
    fn an_end_after_an_idle_lift_does_nothing() {
        let s = screen();
        let mut swipe = Swipe::default();
        swipe.scroll(at(60.0, 120.0), d(-20.0), Step::Moved, false, s);
        assert!(swipe.end().is_some());
        assert!(swipe.scroll(at(60.0, 120.0), d(0.0), Step::Ended, true, s).is_empty());
    }

    #[test]
    fn momentum_after_the_end_is_ignored() {
        let s = screen();
        let mut swipe = Swipe::default();
        swipe.scroll(at(60.0, 120.0), d(0.0), Step::Started, true, s);
        assert_eq!(swipe.scroll(at(60.0, 120.0), d(-10.0), Step::Moved, true, s).len(), 2);
        // While the fingers are on the trackpad, no early lift.
        assert_eq!(swipe.lift_after(), Some(GESTURE_IDLE));
        assert_eq!(swipe.scroll(at(60.0, 120.0), d(0.0), Step::Ended, true, s).len(), 1);
        // The momentum that follows is the Mac's, not a second swipe: iOS
        // flings the lifted finger on its own.
        for _ in 0..5 {
            assert!(swipe.scroll(at(60.0, 120.0), d(-30.0), Step::Moved, true, s).is_empty());
        }
        // A wheel is no momentum, and a new gesture ends the tail.
        let wheel = swipe.scroll(at(60.0, 120.0), d(-10.0), Step::Moved, false, s);
        assert_eq!(wheel.len(), 2, "{wheel:?}");
        assert_eq!(swipe.lift_after(), Some(SWIPE_IDLE));
        swipe.end();
        swipe.scroll(at(60.0, 120.0), d(0.0), Step::Ended, true, s);
        swipe.scroll(at(60.0, 120.0), d(0.0), Step::Started, true, s);
        assert_eq!(swipe.scroll(at(60.0, 120.0), d(-10.0), Step::Moved, true, s).len(), 2);
    }

    #[test]
    fn only_a_left_click_on_the_screen_takes_the_keyboard() {
        // A left click on the screen, with input: a touch (and focus).
        assert_eq!(press_action(true, true, true), Press::Touch);
        // On the frame or the panel beside it, or a watch-only picture:
        // the keyboard goes back.
        assert_eq!(press_action(true, false, true), Press::Unfocus);
        assert_eq!(press_action(true, true, false), Press::Unfocus);
        // Any other button does nothing at all (gpui would focus it).
        for (on, input) in [(true, true), (false, true), (true, false), (false, false)] {
            assert_eq!(press_action(false, on, input), Press::Ignore);
        }
    }
}
