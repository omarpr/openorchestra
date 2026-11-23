//! The tools panel's Device tab: a watch-only view of a booted iOS
//! simulator, picked from every booted one on the Mac.
//!
//! What runs is decided by `devices::tab_state::DeviceTab`; this file feeds
//! it (device lists, visibility, stream events, the picker) and carries out
//! its commands in [`HarnessWorkspace::run_device_commands`], the only code
//! that starts or stops a stream or the screenshot loop. Every start gets a
//! generation; whatever arrives for an older one is dropped.
//!
//! The selection belongs to the window, not the task: nothing on the task,
//! worktree or center-tab switch paths touches it.

use std::collections::VecDeque;
use std::sync::OnceLock;

use gpui::{AnyView, ImageFormat, ObjectFit, RenderImage, StyleRefinement};

use super::super::*;
use crate::devices::StreamError;
use crate::devices::tab_state::{Command, DeviceTab, DeviceView};

/// How often the device list is polled while the tab is on screen.
pub(crate) const DEVICE_POLL: Duration = Duration::from_secs(3);
/// The pause after a failed screenshot before the next try.
const SHOT_RETRY: Duration = Duration::from_millis(500);
/// How often a new screenshot loop looks again while a stopped loop's last
/// shot is still landing.
const SHOT_WAIT: Duration = Duration::from_millis(100);
const REDUCED_NOTE: &str =
    "Reduced frame rate — this Xcode's simulator display API isn't supported";

/// `OO_DEVICE_FORCE_SCREENSHOTS=1` makes every stream start fail, so the tab
/// takes its screenshot fallback (for checking that path). Read once.
fn force_screenshots() -> bool {
    static FORCE: OnceLock<bool> = OnceLock::new();
    *FORCE.get_or_init(|| std::env::var_os("OO_DEVICE_FORCE_SCREENSHOTS").is_some_and(|v| v == "1"))
}

/// Tell `tab` whether it is on screen, once per change. `shown` is what it
/// was last told.
pub(crate) fn sync_shown(tab: &mut DeviceTab, shown: &mut bool, now: bool) -> Vec<Command> {
    if *shown == now {
        return Vec::new();
    }
    *shown = now;
    if now { tab.on_shown() } else { tab.on_hidden() }
}

/// What one tick of the device-list poll does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PollTick {
    /// Re-check visibility. gpui reports visibility from occlusion alone, so
    /// ⌘H or minimise over an already covered window (Hidden → Hidden)
    /// fires no callback; the poll catches it within one tick.
    pub reconcile: bool,
    pub list: bool,
}

/// `tab_shown`: the panel is open on the Device tab; `visible`: that, and
/// the window isn't minimised or hidden.
pub(crate) fn poll_tick(tab_shown: bool, visible: bool) -> PollTick {
    PollTick {
        reconcile: tab_shown,
        list: tab_shown && visible,
    }
}

/// Whether a screenshot loop may take its next shot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shoot {
    Go,
    /// A stopped loop's shot is still landing; one in flight at a time.
    Wait,
    /// This loop's generation stopped.
    Stop,
}

pub(crate) fn may_shoot(live: Option<u64>, generation: u64, in_flight: bool) -> Shoot {
    if live != Some(generation) {
        Shoot::Stop
    } else if in_flight {
        Shoot::Wait
    } else {
        Shoot::Go
    }
}

/// The user picked `udid` in the menu: watch it and remember it. A device
/// that is no longer booted (a menu left open too long) is ignored.
pub(crate) fn pick_device(tab: &mut DeviceTab, prefs: &mut Prefs, udid: &str) -> Vec<Command> {
    if !tab.booted.iter().any(|d| d.udid == udid) {
        return Vec::new();
    }
    prefs.device_udid = Some(udid.to_string());
    tab.on_select(udid)
}

/// The simulator's picture. Its own entity, embedded cached, so a new frame
/// re-renders this view alone.
pub(crate) struct DeviceScreen {
    /// The live stream's newest frame (420f, ready for `surface()`).
    #[cfg(target_os = "macos")]
    frame: Option<objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>>,
    /// The newest screenshot, in Reduced mode.
    shot: Option<Arc<RenderImage>>,
}

impl DeviceScreen {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            frame: None,
            shot: None,
        }
    }

    /// Show nothing (a stream or the screenshots stopped, or another device
    /// is starting).
    fn clear(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        let had_frame = self.frame.take().is_some();
        #[cfg(not(target_os = "macos"))]
        let had_frame = false;
        let had_shot = self.drop_shot(cx);
        if had_frame || had_shot {
            cx.notify();
        }
    }

    #[cfg(target_os = "macos")]
    fn show_frame(
        &mut self,
        frame: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
        cx: &mut Context<Self>,
    ) {
        self.frame = Some(frame);
        cx.notify();
    }

    fn show_shot(&mut self, shot: Arc<RenderImage>, cx: &mut Context<Self>) {
        self.drop_shot(cx);
        self.shot = Some(shot);
        cx.notify();
    }

    /// Frees the shown screenshot, sprite-atlas texture included (gpui keeps
    /// a `RenderImage` there until told). Deferred: during a draw the
    /// window being drawn isn't reachable from `App`, so its atlas would
    /// keep the texture.
    fn drop_shot(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(old) = self.shot.take() else {
            return false;
        };
        cx.defer(move |cx| cx.drop_image(old, None));
        true
    }
}

impl Render for DeviceScreen {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let base = div().size_full().flex().items_center().justify_center();
        #[cfg(target_os = "macos")]
        if let Some(frame) = &self.frame {
            return base.child(
                gpui::surface(frame.clone())
                    .object_fit(ObjectFit::Contain)
                    .size_full(),
            );
        }
        match &self.shot {
            Some(shot) => base.child(
                gpui::img(gpui::ImageSource::Render(shot.clone()))
                    .object_fit(ObjectFit::Contain)
                    .size_full(),
            ),
            None => base,
        }
    }
}

/// A JPEG screenshot of `udid`, decoded for gpui. Runs off the UI thread.
fn take_shot(
    udid: &str,
    dir: &std::path::Path,
    svg: gpui::SvgRenderer,
) -> Result<Arc<RenderImage>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let bytes = crate::devices::simctl::screenshot_jpeg(udid, dir)?;
    gpui::Image::from_bytes(ImageFormat::Jpeg, bytes)
        .to_image_data(svg)
        .map_err(|e| format!("decoding the screenshot: {e}"))
}

impl HarnessWorkspace {
    /// Whether the tools panel is open on the Device tab.
    pub(crate) fn device_tab_shown(&self) -> bool {
        self.prefs.device_tab_shown()
    }

    /// Whether the Device tab is on screen: the only time it lists devices,
    /// streams or takes screenshots. A minimised window or hidden app isn't.
    pub(crate) fn device_visible(&self, window: &Window) -> bool {
        self.device_tab_shown() && !crate::browser_pane::window_is_minimized_or_hidden(window)
    }

    /// Catch-all for every way the Device tab's visibility changes (tool
    /// tab, panel open or closed, minimise, ⌘H): runs from `render`, the
    /// window-visibility observer, `set_tool_tab` and `toggle_diff`. A
    /// no-op unless it changed.
    pub(crate) fn sync_device_visibility(&mut self, window: &Window, cx: &mut Context<Self>) {
        let now = self.device_visible(window);
        let appeared = now && !self.device_shown;
        let cmds = sync_shown(&mut self.device_tab, &mut self.device_shown, now);
        self.run_device_commands(cmds, cx);
        if appeared {
            self.refresh_devices(cx);
        }
    }

    /// One tick of the 3 s poll: catch a visibility change gpui didn't
    /// report, then list if the tab is on screen.
    pub(crate) fn device_poll(&mut self, window: &Window, cx: &mut Context<Self>) {
        let tick = poll_tick(self.device_tab_shown(), self.device_visible(window));
        if tick.reconcile {
            self.sync_device_visibility(window, cx);
        }
        if tick.list {
            self.refresh_devices(cx);
        }
    }

    /// List the booted simulators now, off the UI thread. One at a time.
    pub(crate) fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.device_listing, true) {
            return;
        }
        cx.spawn(async move |view, cx| {
            let list = cx
                .background_spawn(async move { crate::devices::simctl::list_booted() })
                .await;
            let _ = view.update(cx, |ws, cx| ws.apply_device_list(list, cx));
        })
        .detach();
    }

    fn apply_device_list(
        &mut self,
        list: Result<Vec<crate::devices::DeviceInfo>, crate::devices::simctl::ListError>,
        cx: &mut Context<Self>,
    ) {
        self.device_listing = false;
        if !matches!(list, Err(crate::devices::simctl::ListError::Failed(_))) {
            self.device_listed = true;
        }
        if let Err(crate::devices::simctl::ListError::Failed(e)) = &list {
            log::debug!("device: listing simulators failed: {e}");
        }
        let cmds = self.device_tab.on_list(list);
        self.run_device_commands(cmds, cx);
        cx.notify();
    }

    /// The menu's choice.
    pub(crate) fn watch_device(&mut self, udid: &str, cx: &mut Context<Self>) {
        self.device_menu_open = false;
        let cmds = pick_device(&mut self.device_tab, &mut self.prefs, udid);
        self.save_prefs(cx);
        self.run_device_commands(cmds, cx);
        cx.notify();
    }

    /// Carry out the Device tab's commands. The only code that starts or
    /// stops a stream or the screenshot loop. A stream that can't start
    /// (or is forced off) is reported back to the tab at once, and the
    /// fallback it asks for runs here too.
    pub(crate) fn run_device_commands(&mut self, cmds: Vec<Command>, cx: &mut Context<Self>) {
        let mut queue = VecDeque::from(cmds);
        while let Some(cmd) = queue.pop_front() {
            match cmd {
                Command::StartStream(udid) => {
                    let generation = self.next_device_generation(cx);
                    let started = if force_screenshots() {
                        Err(StreamError::Unsupported(
                            "OO_DEVICE_FORCE_SCREENSHOTS is set",
                        ))
                    } else {
                        self.start_device_stream(generation, udid)
                    };
                    if let Err(e) = started {
                        log::info!("device: no live view ({e:?}); using screenshots");
                        queue.extend(self.device_tab.on_stream_failed(e));
                    }
                }
                Command::StartScreenshots(udid) => {
                    let generation = self.next_device_generation(cx);
                    self.start_device_screenshots(generation, udid, cx);
                }
                Command::StopStream => {
                    self.stop_device_work(cx);
                    #[cfg(target_os = "macos")]
                    if let Some(worker) = &self.device_worker {
                        worker.stop();
                    }
                }
                Command::StopScreenshots => self.stop_device_work(cx),
            }
        }
    }

    /// A new generation for a start; whatever older work still reports is
    /// ignored from here on. The old picture goes too.
    fn next_device_generation(&mut self, cx: &mut Context<Self>) -> u64 {
        self.device_generation += 1;
        self.device_live = Some(self.device_generation);
        self.device_screen.update(cx, |screen, cx| screen.clear(cx));
        self.device_generation
    }

    fn stop_device_work(&mut self, cx: &mut Context<Self>) {
        self.device_live = None;
        #[cfg(target_os = "macos")]
        {
            self.device_frames = None;
        }
        self.device_screen.update(cx, |screen, cx| screen.clear(cx));
    }

    #[cfg(target_os = "macos")]
    fn start_device_stream(&mut self, generation: u64, udid: String) -> Result<(), StreamError> {
        use crate::devices::worker::StreamWorker;
        if self.device_worker.is_none() {
            let worker = StreamWorker::spawn(self.device_to_ui.clone())
                .map_err(|e| StreamError::Failed(format!("starting the stream thread: {e}")))?;
            self.device_worker = Some(worker);
        }
        let frames = Arc::new(crate::devices::FrameSlot::new());
        self.device_frames = Some(frames.clone());
        if let Some(worker) = &self.device_worker {
            worker.start(generation, udid, frames);
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn start_device_stream(&mut self, _generation: u64, _udid: String) -> Result<(), StreamError> {
        Err(StreamError::Unsupported("iOS simulators need macOS"))
    }

    /// What the stream thread and the stream report.
    #[cfg(target_os = "macos")]
    pub(crate) fn on_device_msg(
        &mut self,
        msg: crate::devices::worker::StreamMsg,
        cx: &mut Context<Self>,
    ) {
        use crate::devices::worker::StreamMsg;
        let live = self.device_live;
        let current = |generation: u64| live == Some(generation);
        match msg {
            StreamMsg::Frame(generation) if current(generation) => {
                let Some(frame) = self.device_frames.as_ref().and_then(|f| f.take()) else {
                    return;
                };
                // The picture's entity only: the workspace isn't notified.
                self.device_screen
                    .update(cx, |screen, cx| screen.show_frame(frame.0, cx));
            }
            StreamMsg::Started(generation) if current(generation) => {
                log::debug!("device: live view started");
            }
            StreamMsg::Failed(generation, e) if current(generation) => {
                log::info!("device: no live view ({e:?}); using screenshots");
                let cmds = self.device_tab.on_stream_failed(e);
                self.run_device_commands(cmds, cx);
                cx.notify();
            }
            StreamMsg::Gone(generation) if current(generation) => {
                let cmds = self.device_tab.on_gone();
                self.run_device_commands(cmds, cx);
                cx.notify();
            }
            _ => {}
        }
    }

    /// Reduced mode: one `simctl` screenshot in flight at a time, the next
    /// as soon as the last lands (about 2 a second), until this generation
    /// stops. A loop started while a stopped one's shot is still landing
    /// waits for it (`device_shot_in_flight`).
    fn start_device_screenshots(&mut self, generation: u64, udid: String, cx: &mut Context<Self>) {
        let svg = cx.svg_renderer();
        let dir = std::env::temp_dir().join("openorchestra-device");
        cx.spawn(async move |view, cx| {
            let mut logged = false;
            loop {
                let step = view.update(cx, |ws, _| {
                    let step = may_shoot(ws.device_live, generation, ws.device_shot_in_flight);
                    if step == Shoot::Go {
                        ws.device_shot_in_flight = true;
                    }
                    step
                });
                match step {
                    Ok(Shoot::Go) => {}
                    Ok(Shoot::Wait) => {
                        cx.background_executor().timer(SHOT_WAIT).await;
                        continue;
                    }
                    Ok(Shoot::Stop) | Err(_) => break,
                }
                let shot = {
                    let (udid, dir, svg) = (udid.clone(), dir.clone(), svg.clone());
                    cx.background_spawn(async move { take_shot(&udid, &dir, svg) })
                        .await
                };
                let failed = match &shot {
                    Err(e) if !logged => {
                        logged = true;
                        log::warn!("device: screenshot failed: {e}");
                        true
                    }
                    Err(_) => true,
                    Ok(_) => false,
                };
                let Ok(true) = view.update(cx, |ws, cx| ws.show_device_shot(generation, shot, cx))
                else {
                    break;
                };
                if failed {
                    cx.background_executor().timer(SHOT_RETRY).await;
                }
            }
        })
        .detach();
    }

    /// A screenshot landed. False once its generation has stopped, which
    /// ends the loop.
    fn show_device_shot(
        &mut self,
        generation: u64,
        shot: Result<Arc<RenderImage>, String>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.device_shot_in_flight = false;
        if self.device_live != Some(generation) {
            return false;
        }
        if let Ok(shot) = shot {
            self.device_screen
                .update(cx, |screen, cx| screen.show_shot(shot, cx));
        }
        true
    }

    pub(crate) fn render_device_panel(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = match &self.device_tab.view {
            // Nothing yet rather than "No simulator running" for a moment.
            _ if !self.device_listed => div().flex_1().into_any_element(),
            DeviceView::NeedsXcode => device_empty(
                hx,
                "Needs Xcode".into(),
                Some("Install Xcode to watch iOS simulators here."),
            ),
            DeviceView::NoneRunning => device_empty(
                hx,
                "No simulator running".into(),
                Some("Simulators your agents boot show up here."),
            ),
            DeviceView::ShutDown { name } => device_empty(hx, format!("{name} shut down"), None),
            DeviceView::Live { .. } => self.render_device_screen(),
            DeviceView::Reduced { why, .. } => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("device-reduced-note")
                        .flex_none()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(hx.tx2)
                        .tooltip(tip(why.clone()))
                        .child(REDUCED_NOTE),
                )
                .child(self.render_device_screen())
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .children(self.render_device_picker(hx, cx))
            .child(body)
            .into_any_element()
    }

    /// The picture, cached so a new frame re-renders only it. A cached view
    /// is laid out from its style alone, never measured, so it sits in an
    /// absolutely placed box whose size is always definite.
    fn render_device_screen(&self) -> AnyElement {
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                div().absolute().inset_0().p_2().child(
                    AnyView::from(self.device_screen.clone())
                        .cached(StyleRefinement::default().size_full()),
                ),
            )
            .into_any_element()
    }

    /// "<name> · <os> ▾" over a list of every booted simulator. None while
    /// nothing is booted.
    fn render_device_picker(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let booted = &self.device_tab.booted;
        if booted.is_empty() {
            return None;
        }
        let label = |d: &crate::devices::DeviceInfo| format!("{} · {}", d.name, d.os);
        let selected = self.device_tab.selected.as_deref();
        let current = booted
            .iter()
            .find(|d| Some(d.udid.as_str()) == selected)
            .map(label)
            .unwrap_or_else(|| "Choose a simulator".to_string());
        let open = self.device_menu_open;
        let button = div()
            .id("device-picker")
            .flex()
            .items_center()
            .gap_1()
            .min_w_0()
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
            .child(div().min_w_0().truncate().child(current))
            .child(div().flex_none().text_color(hx.tx2).child("▾"))
            // Capture phase: reaches the button before the open menu's
            // outside handler for the same press (see `ports_panel::dismisses`).
            .capture_any_mouse_down(cx.listener(|ws, ev: &MouseDownEvent, _, _| {
                ws.device_menu_press = Some(ev.position);
            }))
            .on_click(cx.listener(move |ws, _, _, cx| {
                ws.device_menu_open = !open;
                cx.notify();
            }));
        let mut col = div().flex().flex_col().min_w_0().child(button);
        if open {
            let mut list = div()
                .mt_1()
                .min_w(px(180.0))
                .p(px(4.0))
                .rounded_md()
                .bg(hx.surface2)
                .border_1()
                .border_color(hx.border2)
                .flex()
                .flex_col()
                .on_mouse_down_out(cx.listener(|ws, ev: &MouseDownEvent, _, cx| {
                    if ws.device_menu_press.take() != Some(ev.position) {
                        ws.device_menu_open = false;
                        cx.notify();
                    }
                }));
            for d in booted {
                let is_selected = Some(d.udid.as_str()) == selected;
                let udid = d.udid.clone();
                list = list.child(
                    div()
                        .id(gpui::SharedString::from(format!("device-pick-{}", d.udid)))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .text_color(if is_selected { hx.tx0 } else { hx.tx1 })
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover))
                        .child(div().w(px(10.0)).child(if is_selected { "✓" } else { "" }))
                        .child(label(d))
                        .on_click(cx.listener(move |ws, _, _, cx| ws.watch_device(&udid, cx))),
                );
            }
            col = col.child(gpui::deferred(
                gpui::anchored().snap_to_window().child(list),
            ));
        }
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .px_3()
                .py_1()
                .child(col)
                .into_any_element(),
        )
    }
}

/// A centred title, and a line under it when there is one.
fn device_empty(hx: &HarnessTheme, title: String, detail: Option<&'static str>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .px_3()
        .child(div().text_sm().text_color(hx.tx1).child(title))
        .children(detail.map(|d| div().text_xs().text_color(hx.tx2).child(d)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{PollTick, Shoot, may_shoot, pick_device, poll_tick, sync_shown};
    use crate::devices::DeviceInfo;
    use crate::devices::tab_state::{Command, DeviceTab};
    use crate::workspace::{Prefs, ToolTab, load_prefs, save_prefs_to};

    fn dev(udid: &str) -> DeviceInfo {
        DeviceInfo {
            udid: udid.into(),
            name: format!("name-{udid}"),
            os: "iOS 27.2".into(),
            device_type: "iPhone-18-Pro".into(),
        }
    }

    /// A shown tab watching `a`, out of `a` and `b`.
    fn watching_a() -> (DeviceTab, bool) {
        let mut tab = DeviceTab::new();
        let mut shown = true;
        assert!(sync_shown(&mut tab, &mut shown, true).is_empty());
        tab.on_list(Ok(vec![dev("a"), dev("b")]));
        assert_eq!(tab.selected.as_deref(), Some("a"));
        (tab, shown)
    }

    /// The body of `fn <name>` in `src`, up to the next method at the same
    /// indent.
    fn method_body<'a>(src: &'a str, name: &str) -> &'a str {
        let start = src.find(name).unwrap_or_else(|| panic!("{name} not found"));
        let rest = &src[start + name.len()..];
        let end = rest.find("\n    pub(crate) fn ").unwrap_or(rest.len());
        &rest[..end]
    }

    #[test]
    fn switching_tasks_keeps_the_watched_device() {
        let (mut tab, mut shown) = watching_a();
        // A task, worktree or center-tab switch changes nothing the Device
        // tab's visibility depends on; the re-render it causes runs the
        // catch-all visibility sync, which must leave the stream alone.
        let issued = sync_shown(&mut tab, &mut shown, true);
        assert!(!issued.contains(&Command::StopStream), "{issued:?}");
        assert!(issued.is_empty(), "{issued:?}");
        assert_eq!(tab.selected.as_deref(), Some("a"));

        // And the switch paths themselves never reach the Device tab's state.
        let guarded = [
            (
                "select",
                method_body(include_str!("../actions.rs"), "pub(crate) fn select("),
            ),
            ("navigation.rs", include_str!("../navigation.rs")),
            ("actions/tasks.rs", include_str!("../actions/tasks.rs")),
        ];
        for (what, src) in guarded {
            for token in ["device_tab", "device_shown", "run_device_commands"] {
                assert!(!src.contains(token), "{what} mentions {token}");
            }
        }
    }

    #[test]
    fn the_device_tab_polls_only_while_visible() {
        let mut prefs = Prefs {
            diff_open: true,
            tool_tab: ToolTab::Ports,
            ..Prefs::default()
        };
        assert!(!prefs.device_tab_shown());
        prefs.tool_tab = ToolTab::Device;
        prefs.diff_open = false;
        assert!(!prefs.device_tab_shown());
        prefs.diff_open = true;
        assert!(prefs.device_tab_shown());
    }

    #[test]
    fn hiding_the_device_tab_stops_the_stream_and_showing_restarts_it() {
        let (mut tab, mut shown) = watching_a();
        assert_eq!(
            sync_shown(&mut tab, &mut shown, false),
            vec![Command::StopStream]
        );
        assert!(sync_shown(&mut tab, &mut shown, false).is_empty());
        assert_eq!(
            sync_shown(&mut tab, &mut shown, true),
            vec![Command::StartStream("a".into())]
        );
    }

    #[test]
    fn choosing_a_device_saves_it() {
        let (mut tab, _) = watching_a();
        let mut prefs = Prefs::default();
        assert_eq!(
            pick_device(&mut tab, &mut prefs, "b"),
            vec![Command::StopStream, Command::StartStream("b".into())]
        );
        assert_eq!(prefs.device_udid.as_deref(), Some("b"));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        save_prefs_to(&path, &prefs);
        assert_eq!(load_prefs(&path).device_udid.as_deref(), Some("b"));

        // A device that isn't booted (a stale menu) is not remembered.
        assert!(pick_device(&mut tab, &mut prefs, "gone").is_empty());
        assert_eq!(prefs.device_udid.as_deref(), Some("b"));
    }

    #[test]
    fn device_poll_reconciles_a_hide_the_window_never_reported() {
        // ⌘H or minimise over an already covered window is Hidden → Hidden in
        // gpui: no visibility callback, so the poll must notice it.
        assert_eq!(
            poll_tick(true, false),
            PollTick {
                reconcile: true,
                list: false
            }
        );
        assert_eq!(
            poll_tick(true, true),
            PollTick {
                reconcile: true,
                list: true
            }
        );
        // Another tool tab or the panel closed: nothing to do (render and
        // set_tool_tab already reconciled that).
        assert_eq!(
            poll_tick(false, false),
            PollTick {
                reconcile: false,
                list: false
            }
        );
    }

    #[test]
    fn device_screenshots_wait_for_the_shot_already_in_flight() {
        // The current loop, nothing in flight: shoot.
        assert_eq!(may_shoot(Some(2), 2, false), Shoot::Go);
        // A stopped loop's shot still landing: the new loop waits for it.
        assert_eq!(may_shoot(Some(2), 2, true), Shoot::Wait);
        // An old generation ends, whether or not a shot is in flight.
        assert_eq!(may_shoot(Some(2), 1, false), Shoot::Stop);
        assert_eq!(may_shoot(None, 1, true), Shoot::Stop);
    }
}
