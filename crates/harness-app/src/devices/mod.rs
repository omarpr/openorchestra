//! The Device tab's pure model: booted-simulator listing, device choice and
//! the single-frame hand-off between the capture thread and the UI.

// Used by the Device tab (tasks 3–6 of docs/superpowers/plans/2026-11-22-device-tab-ios.md).
#![allow(dead_code, unused_imports)]

#[cfg(target_os = "macos")]
pub(crate) mod convert;
#[cfg(target_os = "macos")]
pub(crate) mod sim_display;
pub(crate) mod simctl;
pub(crate) mod tab_state;

use std::sync::Mutex;

/// A booted simulator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceInfo {
    pub udid: String,
    pub name: String,
    /// "iOS 27.2"
    pub os: String,
    /// "iPhone-18-Pro"
    pub device_type: String,
}

/// Why a device stream could not start or kept going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamError {
    NoXcode,
    Unsupported(&'static str),
    DeviceNotFound,
    Failed(String),
}

pub(crate) use simctl::parse_booted;

/// The selected UDID if it is still booted, else the first booted device.
pub(crate) fn choose_device<'a>(
    booted: &'a [DeviceInfo],
    selected: Option<&str>,
) -> Option<&'a DeviceInfo> {
    selected
        .and_then(|udid| booted.iter().find(|d| d.udid == udid))
        .or_else(|| booted.first())
}

/// Holds the newest frame. A capture thread `put`s; the UI `take`s. A `put`
/// over an unread frame replaces it and returns false, so the UI is woken
/// once per burst rather than once per frame.
pub(crate) struct FrameSlot<T>(Mutex<Option<T>>);

impl<T> FrameSlot<T> {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// True when the slot was empty, meaning the UI needs waking.
    pub(crate) fn put(&self, frame: T) -> bool {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        slot.replace(frame).is_none()
    }

    pub(crate) fn take(&self) -> Option<T> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = include_str!("testdata/booted_one.json");
    const NONE: &str = include_str!("testdata/booted_none.json");
    const TWO: &str = include_str!("testdata/booted_two.json");

    fn dev(udid: &str) -> DeviceInfo {
        DeviceInfo {
            udid: udid.into(),
            name: udid.into(),
            os: "iOS 27.2".into(),
            device_type: "iPhone-18-Pro".into(),
        }
    }

    #[test]
    fn parses_one_booted_iphone() {
        assert_eq!(
            parse_booted(ONE).unwrap(),
            vec![DeviceInfo {
                udid: "0F291397-02EB-448E-BDFF-320883371EB1".into(),
                name: "iPhone 18 Pro".into(),
                os: "iOS 27.2".into(),
                device_type: "iPhone-18-Pro".into(),
            }]
        );
    }

    #[test]
    fn no_booted_devices_is_an_empty_list() {
        assert_eq!(parse_booted(NONE), Ok(vec![]));
    }

    #[test]
    fn a_shutdown_device_in_the_list_is_skipped() {
        let json = ONE.replace("\"Booted\"", "\"Shutdown\"");
        assert_eq!(parse_booted(&json), Ok(vec![]));
    }

    #[test]
    fn two_devices_sort_by_name() {
        let got = parse_booted(TWO).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "iPad Air 13-inch (M4)");
        assert_eq!(got[0].os, "iOS 27.0");
        assert_eq!(got[1].name, "iPhone 18 Pro");
    }

    #[test]
    fn garbage_json_is_an_error() {
        assert!(parse_booted("not json").is_err());
    }

    #[test]
    fn choose_keeps_a_booted_selection() {
        let list = [dev("a"), dev("b")];
        assert_eq!(choose_device(&list, Some("b")).unwrap().udid, "b");
    }

    #[test]
    fn choose_falls_back_to_the_first_when_the_selection_is_gone() {
        let list = [dev("a"), dev("b")];
        assert_eq!(choose_device(&list, Some("gone")).unwrap().udid, "a");
        assert_eq!(choose_device(&list, None).unwrap().udid, "a");
    }

    #[test]
    fn choose_with_nothing_booted_is_none() {
        assert!(choose_device(&[], Some("a")).is_none());
    }

    #[test]
    fn frame_slot_wakes_once_then_coalesces() {
        let slot = FrameSlot::new();
        assert!(slot.put(1));
        assert!(!slot.put(2));
        assert_eq!(slot.take(), Some(2));
        assert_eq!(slot.take(), None);
        assert!(slot.put(3));
    }
}
