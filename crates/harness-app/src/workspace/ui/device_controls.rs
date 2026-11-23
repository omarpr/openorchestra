//! The Device tab's bottom bar: buttons under the picture that act on the
//! simulator it shows, and the boot and shutdown runs behind them.
//!
//! A control acts only on the device on screen when it was clicked, only on
//! a click, and off the UI thread. One runs at a time
//! (`device_control_in_flight`); the buttons wait meanwhile.

use super::super::*;
use crate::devices::DeviceInfo;
use crate::devices::tab_state::{DeviceTab, DeviceView};

/// The device whose picture the tab shows (live or screenshots), which is
/// the one the bar's controls act on. None while nothing is shown.
pub(crate) fn watched_device(tab: &DeviceTab) -> Option<&DeviceInfo> {
    let (DeviceView::Live { udid } | DeviceView::Reduced { udid, .. }) = &tab.view else {
        return None;
    };
    tab.booted.iter().find(|d| &d.udid == udid)
}

/// The line of a failure worth showing: its first, trimmed.
pub(crate) fn error_line(e: &str) -> String {
    e.trim().lines().next().unwrap_or_default().trim().to_string()
}

impl HarnessWorkspace {
    /// The device the bar acts on: the one on screen, unless a boot offer
    /// shows in place of its picture.
    fn device_bar_target(&self) -> Option<&DeviceInfo> {
        if self.device_boot_offer.is_some() {
            return None;
        }
        watched_device(&self.device_tab)
    }

    /// The row under the picture. None while no picture shows.
    pub(crate) fn render_device_controls(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.device_bar_target()?;
        let busy = self.device_control_in_flight;
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
                .child(self.device_bar_button(
                    hx,
                    "device-shut-down",
                    "icons/power.svg",
                    "Shut down",
                    |ws, _, cx| ws.ask_shut_down(cx),
                    cx,
                ))
                .child(div().flex_1())
                .when(busy, |d| {
                    d.child(div().flex_none().text_xs().text_color(hx.tx2).child("Working…"))
                })
                .into_any_element(),
        )
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
        let enabled = !self.device_control_in_flight;
        div()
            .id(id)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(px(24.0))
            .rounded_md()
            .tooltip(tip(tooltip))
            .child(gpui::svg().size(px(14.0)).path(icon).text_color(hx.tx1))
            .when(!enabled, |d| d.opacity(0.4))
            .when(enabled, |d| {
                d.cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
                    .on_click(cx.listener(move |ws, _, window, cx| on_click(ws, window, cx)))
            })
            .into_any_element()
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

#[cfg(test)]
mod tests {
    use super::{error_line, watched_device};
    use crate::devices::DeviceInfo;
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
}
