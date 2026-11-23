//! The Device tab's bottom bar: buttons under the picture that act on the
//! simulator it shows (rotate, Home, appearance, text size, copy a
//! screenshot, shut down), and the runs behind them; and Show frame, which
//! only changes how the picture is drawn.
//!
//! A control acts only on the device on screen when it was clicked, only on
//! a click, and off the UI thread. One runs at a time
//! (`device_control_in_flight`); the buttons wait meanwhile.

use super::super::*;
use crate::devices::DeviceInfo;
use crate::devices::controls::{Appearance, Control, ControlOutcome};
use crate::devices::tab_state::{DeviceTab, DeviceView};

/// The device whose picture the tab shows (live or screenshots), which is
/// the one the bar's controls act on. None while nothing is shown.
pub(crate) fn watched_device(tab: &DeviceTab) -> Option<&DeviceInfo> {
    let (DeviceView::Live { udid } | DeviceView::Reduced { udid, .. }) = &tab.view else {
        return None;
    };
    tab.booted.iter().find(|d| &d.udid == udid)
}

/// The device the bar acts on: the one on screen, unless a boot offer
/// shows in place of its picture.
fn bar_target(tab: &DeviceTab, boot_offer: bool) -> Option<&DeviceInfo> {
    if boot_offer {
        return None;
    }
    watched_device(tab)
}

/// The UDID a control click acts on: the device on screen at the click.
/// None while another control runs (the click is ignored) or nothing shows.
pub(crate) fn control_target(tab: &DeviceTab, boot_offer: bool, in_flight: bool) -> Option<String> {
    if in_flight {
        return None;
    }
    bar_target(tab, boot_offer).map(|d| d.udid.clone())
}

/// A control's button: its element id, icon and tooltip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControlLook {
    pub id: &'static str,
    pub icon: &'static str,
    pub tooltip: &'static str,
}

/// How `control`'s button looks. The appearance button names (and shows)
/// the appearance a click switches to; until the device's is read, dark,
/// since a new simulator starts light.
pub(crate) fn control_look(control: Control, appearance: Option<Appearance>) -> ControlLook {
    let look = |id, icon, tooltip| ControlLook { id, icon, tooltip };
    match control {
        Control::Rotate => look("device-rotate", "icons/rotate_cw.svg", "Rotate"),
        Control::Home => look("device-home", "icons/oo/house.svg", "Home"),
        Control::ToggleAppearance => match appearance {
            Some(Appearance::Dark) => {
                look("device-appearance", "icons/oo/sun.svg", "Light appearance")
            }
            Some(Appearance::Light) | None => {
                look("device-appearance", "icons/oo/moon.svg", "Dark appearance")
            }
        },
        Control::TextSmaller => {
            look("device-text-smaller", "icons/oo/text_smaller.svg", "Smaller text")
        }
        Control::TextBigger => {
            look("device-text-bigger", "icons/oo/text_bigger.svg", "Larger text")
        }
        Control::Screenshot => look("device-screenshot", "icons/oo/camera.svg", "Copy screenshot"),
    }
}

/// The note a finished control leaves, and whether it is a failure. A
/// control whose effect shows on the picture says nothing.
pub(crate) fn outcome_note(result: &Result<ControlOutcome, String>) -> Option<(String, bool)> {
    match result {
        Ok(ControlOutcome::Done | ControlOutcome::Appearance(_)) => None,
        Ok(ControlOutcome::AtLimit) => Some(("Text size is at its limit".to_string(), false)),
        Ok(ControlOutcome::Png(_)) => Some(("Copied screenshot".to_string(), false)),
        Err(e) => Some((error_line(e), true)),
    }
}

/// Show frame's button. It stays the same either way and shows as pressed
/// while the frame is on.
pub(crate) const FRAME_TOGGLE: ControlLook = ControlLook {
    id: "device-frame",
    icon: "icons/oo/phone_frame.svg",
    tooltip: "Show frame",
};

/// The line of a failure worth showing: its first, trimmed.
pub(crate) fn error_line(e: &str) -> String {
    e.trim().lines().next().unwrap_or_default().trim().to_string()
}

impl HarnessWorkspace {
    /// The device the bar acts on: the one on screen, unless a boot offer
    /// shows in place of its picture.
    fn device_bar_target(&self) -> Option<&DeviceInfo> {
        bar_target(&self.device_tab, self.device_boot_offer.is_some())
    }

    /// The row under the picture: the test controls, then Shut down. None
    /// while no picture shows.
    pub(crate) fn render_device_controls(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let target = self.device_bar_target()?;
        let busy = self.device_control_in_flight;
        let appearance = self
            .device_appearance
            .as_ref()
            .filter(|(udid, _)| *udid == target.udid)
            .map(|&(_, a)| a);
        let controls = crate::devices::controls::available_controls(
            &self.device_tab.view,
            self.device_can_rotate,
        );
        let buttons: Vec<AnyElement> = controls
            .into_iter()
            .map(|control| {
                let look = control_look(control, appearance);
                self.device_bar_button(
                    hx,
                    look.id,
                    look.icon,
                    look.tooltip,
                    move |ws, _, cx| ws.run_device_control(control, cx),
                    cx,
                )
            })
            .collect();
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .px_3()
                .py_1()
                .border_t_1()
                .border_color(hx.border)
                .children(buttons)
                .child(div().flex_1())
                .when(busy, |d| {
                    d.child(div().flex_none().text_xs().text_color(hx.tx2).child("Working…"))
                })
                // Not a device control: it works while one runs.
                .child(bar_button(
                    hx,
                    FRAME_TOGGLE,
                    true,
                    self.prefs.device_frame,
                    |ws, _, cx| ws.toggle_device_frame(cx),
                    cx,
                ))
                .child(self.device_bar_button(
                    hx,
                    "device-shut-down",
                    "icons/power.svg",
                    "Shut down",
                    |ws, _, cx| ws.ask_shut_down(cx),
                    cx,
                ))
                .into_any_element(),
        )
    }

    /// A test control's click: run it on the device on screen now, off the
    /// UI thread, and say how it went.
    fn run_device_control(&mut self, control: Control, cx: &mut Context<Self>) {
        let Some(udid) = control_target(
            &self.device_tab,
            self.device_boot_offer.is_some(),
            self.device_control_in_flight,
        ) else {
            return;
        };
        self.device_control_in_flight = true;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = {
                let udid = udid.clone();
                cx.background_spawn(async move {
                    crate::devices::controls::run_public(&udid, control)
                })
                .await
            };
            let _ = view.update(cx, |ws, cx| {
                ws.device_control_in_flight = false;
                ws.device_control_done(udid, result, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// A control on `udid` finished: copy a screenshot, remember a new
    /// appearance, and leave the note it calls for.
    fn device_control_done(
        &mut self,
        udid: String,
        result: Result<ControlOutcome, String>,
        cx: &mut Context<Self>,
    ) {
        if let Err(e) = &result {
            log::info!("device: control failed: {e}");
        }
        if let Some((note, failed)) = outcome_note(&result) {
            self.set_note_as(note, failed);
        }
        match result {
            Ok(ControlOutcome::Png(bytes)) => {
                let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes);
                cx.write_to_clipboard(gpui::ClipboardItem::new_image(&image));
            }
            Ok(ControlOutcome::Appearance(appearance)) => {
                self.device_appearance = Some((udid, appearance));
            }
            _ => {}
        }
    }

    /// Ask, off the UI thread, whether this Xcode's `devicectl` can rotate;
    /// Rotate is left out until it says yes. Asked again each time the tab
    /// appears until it does (a slow `xcrun` isn't a no for good), one ask
    /// at a time.
    pub(crate) fn check_device_rotate(&mut self, cx: &mut Context<Self>) {
        if self.device_can_rotate || std::mem::replace(&mut self.device_rotate_asking, true) {
            return;
        }
        cx.spawn(async move |view, cx| {
            let available = cx
                .background_spawn(async move { crate::devices::controls::rotate_available() })
                .await;
            let _ = view.update(cx, |ws, cx| {
                ws.device_rotate_asking = false;
                if available {
                    ws.device_can_rotate = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Read `udid`'s appearance off the UI thread, for the appearance
    /// button. Kept only while `udid` is still the device on screen.
    pub(crate) fn read_device_appearance(&mut self, udid: String, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            let read = {
                let udid = udid.clone();
                cx.background_spawn(async move {
                    crate::devices::controls::read_appearance(&udid)
                })
                .await
            };
            let _ = view.update(cx, |ws, cx| match read {
                Ok(appearance) => {
                    let on_screen = watched_device(&ws.device_tab).map(|d| d.udid.as_str());
                    if on_screen == Some(udid.as_str()) {
                        ws.device_appearance = Some((udid, appearance));
                        cx.notify();
                    }
                }
                Err(e) => log::debug!("device: reading the appearance failed: {e}"),
            });
        })
        .detach();
    }

    /// One icon button in the bar, with its tooltip. While a control runs it
    /// is dimmed and ignores clicks.
    fn device_bar_button(
        &self,
        hx: &HarnessTheme,
        id: &'static str,
        icon: &'static str,
        tooltip: &'static str,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let look = ControlLook { id, icon, tooltip };
        bar_button(hx, look, !self.device_control_in_flight, false, on_click, cx)
    }

    /// Show frame: draw the iPhone's frame around the picture or not, and
    /// remember it.
    fn toggle_device_frame(&mut self, cx: &mut Context<Self>) {
        self.prefs.device_frame = !self.prefs.device_frame;
        let show = self.prefs.device_frame;
        self.device_screen
            .update(cx, |screen, cx| screen.set_show_frame(show, cx));
        self.save_prefs(cx);
        cx.notify();
    }

    /// Shut down: ask first, naming the device on screen now.
    fn ask_shut_down(&mut self, cx: &mut Context<Self>) {
        if self.device_control_in_flight {
            return;
        }
        self.device_shutdown_confirm = self.device_bar_target().cloned();
        cx.notify();
    }

    /// The confirm's Shut down. Acts only if `device` is still the one on
    /// screen: the poll may have moved the tab on while the dialog was open.
    pub(crate) fn shut_down_device(&mut self, device: &DeviceInfo, cx: &mut Context<Self>) {
        self.device_shutdown_confirm = None;
        cx.notify();
        if self.device_control_in_flight
            || self.device_bar_target().map(|d| d.udid.as_str()) != Some(device.udid.as_str())
        {
            return;
        }
        let udid = device.udid.clone();
        self.run_device_power(cx, move || crate::devices::controls::shutdown(&udid));
    }

    /// The boot offer's Boot: make the offered device the user's pick (so the
    /// list that shows it running goes live on it) and boot it.
    pub(crate) fn boot_offered_device(&mut self, cx: &mut Context<Self>) {
        if self.device_control_in_flight {
            return;
        }
        let Some(device) = self.device_boot_offer.clone() else {
            return;
        };
        super::device_panel::pick_for_boot(&mut self.device_tab, &mut self.prefs, &device.udid);
        self.save_prefs(cx);
        let udid = device.udid;
        self.run_device_power(cx, move || crate::devices::controls::boot(&udid));
    }

    /// Run a boot or shutdown off the UI thread; when it ends, say why if
    /// it failed and list the devices again at once.
    fn run_device_power(
        &mut self,
        cx: &mut Context<Self>,
        run: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        self.device_control_in_flight = true;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { run() }).await;
            let _ = view.update(cx, |ws, cx| {
                ws.device_control_in_flight = false;
                if let Err(e) = result {
                    log::info!("device: {e}");
                    ws.set_error_note(error_line(&e));
                }
                if ws.device_shown {
                    ws.refresh_devices(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// An icon button for the bar. `enabled`: clickable (else dimmed);
/// `pressed`: drawn as a toggle that is on.
fn bar_button(
    hx: &HarnessTheme,
    look: ControlLook,
    enabled: bool,
    pressed: bool,
    on_click: impl Fn(&mut HarnessWorkspace, &mut Window, &mut Context<HarnessWorkspace>) + 'static,
    cx: &mut Context<HarnessWorkspace>,
) -> AnyElement {
    div()
        .id(look.id)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(24.0))
        .rounded_md()
        .tooltip(tip(look.tooltip))
        .child(gpui::svg().size(px(14.0)).path(look.icon).text_color(if pressed {
            hx.tx0
        } else {
            hx.tx1
        }))
        .when(pressed, |d| d.bg(hx.surface2))
        .when(!enabled, |d| d.opacity(0.4))
        .when(enabled, |d| {
            d.cursor_pointer()
                .hover(|d| d.bg(hx.surface_hover))
                .on_click(cx.listener(move |ws, _, window, cx| on_click(ws, window, cx)))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{control_look, control_target, error_line, outcome_note, watched_device};
    use crate::devices::DeviceInfo;
    use crate::devices::controls::{Appearance, Control, ControlOutcome};
    use crate::devices::tab_state::{DeviceTab, DeviceView};

    fn dev(udid: &str) -> DeviceInfo {
        DeviceInfo {
            udid: udid.into(),
            name: format!("name-{udid}"),
            os: "iOS 27.2".into(),
            device_type: "iPhone-18-Pro".into(),
        }
    }

    #[test]
    fn shut_down_targets_the_device_on_screen() {
        let mut tab = DeviceTab::new();
        let ticket = tab.list_started();
        tab.on_list(ticket, Ok(vec![dev("a"), dev("b")]));
        assert_eq!(watched_device(&tab).map(|d| d.udid.as_str()), Some("a"));

        // The picture, not the pick: a pick that isn't booted leaves "a" on
        // screen, and "a" is what Shut down names.
        tab.preferred = Some("c".into());
        assert_eq!(watched_device(&tab).map(|d| d.name.as_str()), Some("name-a"));

        tab.view = DeviceView::Reduced { udid: "b".into(), why: "x".into(), transient: true };
        assert_eq!(watched_device(&tab).map(|d| d.udid.as_str()), Some("b"));

        for view in [
            DeviceView::NeedsXcode,
            DeviceView::NoneRunning,
            DeviceView::ShutDown { name: "name-a".into() },
        ] {
            tab.view = view;
            assert!(watched_device(&tab).is_none(), "{:?}", tab.view);
        }
    }

    #[test]
    fn a_failure_note_is_its_first_line() {
        assert_eq!(
            error_line("  Unable to boot device in current state: Booted\nUnderlying error...\n"),
            "Unable to boot device in current state: Booted"
        );
        assert_eq!(error_line(""), "");
    }

    /// A tab watching `a`, out of `a` and `b`.
    fn watching_a() -> DeviceTab {
        let mut tab = DeviceTab::new();
        let ticket = tab.list_started();
        tab.on_list(ticket, Ok(vec![dev("a"), dev("b")]));
        tab
    }

    #[test]
    fn a_click_targets_the_device_shown_at_click_time() {
        let mut tab = watching_a();
        assert_eq!(control_target(&tab, false, false).as_deref(), Some("a"));
        // A pick that isn't booted yet leaves "a" on screen: "a" it is.
        tab.preferred = Some("c".into());
        assert_eq!(control_target(&tab, false, false).as_deref(), Some("a"));
        // Screenshots of "b": "b".
        tab.view = DeviceView::Reduced { udid: "b".into(), why: "x".into(), transient: true };
        assert_eq!(control_target(&tab, false, false).as_deref(), Some("b"));
        // A boot offer covers the picture: nothing.
        assert_eq!(control_target(&tab, true, false), None);
        // Nothing on screen: nothing.
        for view in [
            DeviceView::NeedsXcode,
            DeviceView::NoneRunning,
            DeviceView::ShutDown { name: "a".into() },
        ] {
            tab.view = view;
            assert_eq!(control_target(&tab, false, false), None, "{:?}", tab.view);
        }
    }

    #[test]
    fn a_second_click_while_one_runs_is_ignored() {
        let tab = watching_a();
        assert_eq!(control_target(&tab, false, true), None);
        assert_eq!(control_target(&tab, false, false).as_deref(), Some("a"));
    }

    #[test]
    fn the_appearance_tooltip_names_the_switch_target() {
        let look = |a| control_look(Control::ToggleAppearance, a);
        assert_eq!(look(Some(Appearance::Light)).tooltip, "Dark appearance");
        assert_eq!(look(Some(Appearance::Dark)).tooltip, "Light appearance");
        // Not read yet: a new simulator starts light.
        assert_eq!(look(None).tooltip, "Dark appearance");
        assert_ne!(look(Some(Appearance::Light)).icon, look(Some(Appearance::Dark)).icon);
    }

    #[test]
    fn each_control_has_its_tooltip() {
        let tooltip = |c| control_look(c, None).tooltip;
        assert_eq!(tooltip(Control::Rotate), "Rotate");
        assert_eq!(tooltip(Control::Home), "Home");
        assert_eq!(tooltip(Control::TextSmaller), "Smaller text");
        assert_eq!(tooltip(Control::TextBigger), "Larger text");
        assert_eq!(tooltip(Control::Screenshot), "Copy screenshot");
        assert_eq!(super::FRAME_TOGGLE.tooltip, "Show frame");
    }

    #[test]
    fn what_a_finished_control_says() {
        assert_eq!(outcome_note(&Ok(ControlOutcome::Done)), None);
        assert_eq!(outcome_note(&Ok(ControlOutcome::Appearance(Appearance::Dark))), None);
        assert_eq!(
            outcome_note(&Ok(ControlOutcome::AtLimit)),
            Some(("Text size is at its limit".to_string(), false))
        );
        assert_eq!(
            outcome_note(&Ok(ControlOutcome::Png(vec![1]))),
            Some(("Copied screenshot".to_string(), false))
        );
        assert_eq!(
            outcome_note(&Err("devicectl timed out after 5 s\nmore".to_string())),
            Some(("devicectl timed out after 5 s".to_string(), true))
        );
    }

    #[test]
    fn every_bar_icon_is_bundled() {
        use gpui::AssetSource as _;
        let assets = crate::app_assets::AppAssets;
        let mut icons = vec!["icons/power.svg", super::FRAME_TOGGLE.icon];
        for control in [
            Control::Rotate,
            Control::Home,
            Control::TextSmaller,
            Control::TextBigger,
            Control::Screenshot,
        ] {
            icons.push(control_look(control, None).icon);
        }
        for appearance in [Appearance::Light, Appearance::Dark] {
            icons.push(control_look(Control::ToggleAppearance, Some(appearance)).icon);
        }
        for icon in icons {
            assert!(assets.load(icon).unwrap().is_some(), "{icon}");
        }
    }
}
