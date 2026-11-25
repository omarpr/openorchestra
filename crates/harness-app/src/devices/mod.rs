//! The Device tab's pure model: simulator (and Android emulator) listing,
//! device choice and the single-frame hand-off between the capture thread
//! and the UI.

#[cfg(target_os = "macos")]
pub(crate) mod android;
pub(crate) mod chrome;
pub(crate) mod controls;
#[cfg(target_os = "macos")]
pub(crate) mod convert;
pub(crate) mod input;
#[cfg(target_os = "macos")]
pub(crate) mod sim_display;
pub(crate) mod simctl;
pub(crate) mod tab_state;
#[cfg(target_os = "macos")]
pub(crate) mod worker;

use std::sync::Mutex;

/// What kind of device a [`DeviceInfo`] is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Platform {
    #[default]
    Ios,
    Android,
}

/// How Android device ids start: `"android:<avd.id>"`, so an AVD's id
/// can't collide with an iOS UDID in the prefs, the picker or the tab.
const ANDROID_PREFIX: &str = "android:";

impl Platform {
    /// The platform a device id belongs to.
    pub(crate) fn of_id(id: &str) -> Platform {
        if id.starts_with(ANDROID_PREFIX) {
            Platform::Android
        } else {
            Platform::Ios
        }
    }
}

/// The device id of the AVD `avd_id`.
#[cfg(target_os = "macos")]
pub(crate) fn android_device_id(avd_id: &str) -> String {
    format!("{ANDROID_PREFIX}{avd_id}")
}

/// The AVD id inside an Android device id; None for an iOS one.
#[cfg(target_os = "macos")]
pub(crate) fn avd_id_of(device_id: &str) -> Option<&str> {
    device_id.strip_prefix(ANDROID_PREFIX)
}

/// The Android groups alone (`devices::android`).
#[cfg(target_os = "macos")]
pub(crate) use android::list_android;

/// Android devices are only found on macOS: none.
#[cfg(not(target_os = "macos"))]
pub(crate) fn list_android() -> AndroidInventory {
    AndroidInventory { running: Vec::new(), configured: Ok(Vec::new()) }
}

pub(crate) struct AndroidInventory {
    pub running: Vec<DeviceInfo>,
    pub configured: Result<Vec<DeviceInfo>, String>,
}

/// Whether the Android SDK has adb, which an emulator's appearance and
/// text size controls run. Checks the disk: call it off the main thread.
#[cfg(target_os = "macos")]
pub(crate) fn adb_available() -> bool {
    android::controls::adb().is_some()
}

/// No emulators elsewhere than macOS, so no adb either.
#[cfg(not(target_os = "macos"))]
pub(crate) fn adb_available() -> bool {
    false
}

/// A running Android emulator, as its discovery file
/// (`~/Library/Caches/TemporaryItems/avd/running/pid_<pid>.ini`) lists it.
// Only found (made) on macOS, in `devices::android`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AndroidRunning {
    /// The emulator's (qemu's) pid, from the file name.
    pub pid: u32,
    /// `avd.id`: "Pixel_9_Pro_API_35".
    pub avd_id: String,
    /// `avd.name`: "Pixel 9 Pro API 35".
    pub name: String,
    pub grpc_port: u16,
    /// The gRPC bearer token, when the emulator wrote one.
    pub token: Option<String>,
    /// The console port (5554), which also names its adb serial.
    pub serial_port: u16,
    pub avd_dir: std::path::PathBuf,
}

/// A simulator or an Android emulator (running, or a stopped AVD).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeviceInfo {
    /// A simulator's UDID, or `"android:<avd.id>"`.
    pub udid: String,
    pub name: String,
    /// "iOS 27.2", or "Android".
    pub os: String,
    /// "iPhone-18-Pro"; empty for Android.
    pub device_type: String,
    pub platform: Platform,
    /// A running emulator's discovery entry; None for a stopped AVD and
    /// for iOS.
    pub android: Option<AndroidRunning>,
}

/// Why a device stream could not start or kept going.
// Elsewhere than macOS no stream exists, so only `Unsupported` is ever made.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamError {
    NoXcode,
    Unsupported(&'static str),
    DeviceNotFound,
    Failed(String),
    /// An emulator listed moments ago isn't answering yet: it is still
    /// booting, which is no failure.
    Starting(String),
}

/// The device to watch: the user's pick if it is booted, else the one being
/// watched if it still is (a device that boots meanwhile doesn't take over),
/// else the first booted device.
pub(crate) fn choose_device<'a>(
    booted: &'a [DeviceInfo],
    preferred: Option<&str>,
    current: Option<&str>,
) -> Option<&'a DeviceInfo> {
    let find = |udid: &str| booted.iter().find(|d| d.udid == udid);
    preferred
        .and_then(find)
        .or_else(|| current.and_then(find))
        .or_else(|| booted.first())
}

/// How far clockwise to turn the (always portrait) simulator surface so its
/// picture is upright.
// Only the macOS stream turns frames.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Quarter {
    None,
    Cw90,
    Cw180,
    Cw270,
}

/// The turn for a `screenProperties.uiOrientation`, from the Device controls
/// spike: 1 portrait, 3 landscape with the UI's top at the surface's left
/// edge, 4 with it at the right edge, 2 upside down (never seen on an
/// iPhone; presumed). Anything else is drawn as it comes.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn quarter_for(ui_orientation: u32) -> Quarter {
    match ui_orientation {
        3 => Quarter::Cw90,
        2 => Quarter::Cw180,
        4 => Quarter::Cw270,
        _ => Quarter::None,
    }
}

/// Holds the newest frame. A capture thread `put`s; the UI `take`s. A `put`
/// over an unread frame replaces it and returns false, so the UI is woken
/// once per burst rather than once per frame.
// Only the macOS stream (`worker`) hands frames over.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct FrameSlot<T>(Mutex<Option<T>>);

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
    use super::simctl::{DeviceList, parse_devices};
    use super::*;

    const ONE: &str = include_str!("testdata/booted_one.json");
    const NONE: &str = include_str!("testdata/booted_none.json");
    const TWO: &str = include_str!("testdata/booted_two.json");
    const ALL: &str = include_str!("testdata/devices_all.json");

    fn parse_booted(json: &str) -> Result<Vec<DeviceInfo>, String> {
        parse_devices(json).map(|l| l.booted)
    }

    fn names(list: &[DeviceInfo]) -> Vec<(&str, &str)> {
        list.iter().map(|d| (d.name.as_str(), d.os.as_str())).collect()
    }

    fn dev(udid: &str) -> DeviceInfo {
        DeviceInfo {
            udid: udid.into(),
            name: udid.into(),
            os: "iOS 27.2".into(),
            device_type: "iPhone-18-Pro".into(),
            ..Default::default()
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
                ..Default::default()
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
    fn parses_booted_and_shut_down_groups() {
        let DeviceList { booted, shut_down, .. } = parse_devices(ALL).unwrap();
        // Each group sorted by name, then OS.
        assert_eq!(names(&booted), [("iPhone 18 Pro", "iOS 27.0"), ("iPhone 18 Pro", "iOS 27.2")]);
        assert_eq!(names(&shut_down), [("iPad Air 13-inch (M4)", "iOS 27.0"), ("iPhone 18", "iOS 27.2")]);
        assert_eq!(shut_down[1].udid, "11111111-0000-0000-0000-000000000001");
        assert_eq!(shut_down[1].device_type, "iPhone-18");
        assert_eq!(parse_devices(NONE), Ok(DeviceList::default()));
    }

    #[test]
    fn unavailable_and_transitional_devices_are_left_out() {
        let list = parse_devices(ALL).unwrap();
        let all: Vec<&str> = list.booted.iter().chain(&list.shut_down).map(|d| d.name.as_str()).collect();
        // Booting, Shutting Down and Creating are in neither group, nor is a
        // device whose runtime is gone (`isAvailable: false`), booted or not.
        for gone in ["iPhone Air", "iPhone 17e", "iPhone 17", "iPhone 16", "iPhone 16 Pro"] {
            assert!(!all.contains(&gone), "{gone} in {all:?}");
        }
        assert_eq!(all.len(), 4, "{all:?}");
        assert!(parse_devices("not json").is_err());
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
        assert_eq!(choose_device(&list, None, Some("b")).unwrap().udid, "b");
    }

    #[test]
    fn choose_prefers_the_pick_then_the_watched_device() {
        let list = [dev("a"), dev("b"), dev("c")];
        assert_eq!(choose_device(&list, Some("c"), Some("b")).unwrap().udid, "c");
        assert_eq!(choose_device(&list, Some("gone"), Some("b")).unwrap().udid, "b");
    }

    #[test]
    fn choose_falls_back_to_the_first_when_the_selection_is_gone() {
        let list = [dev("a"), dev("b")];
        assert_eq!(choose_device(&list, Some("gone"), Some("gone")).unwrap().udid, "a");
        assert_eq!(choose_device(&list, None, None).unwrap().udid, "a");
    }

    #[test]
    fn choose_with_nothing_booted_is_none() {
        assert!(choose_device(&[], Some("a"), Some("a")).is_none());
    }

    #[test]
    fn quarter_turns_follow_the_ui_orientation() {
        assert_eq!(quarter_for(1), Quarter::None);
        assert_eq!(quarter_for(3), Quarter::Cw90);
        assert_eq!(quarter_for(4), Quarter::Cw270);
        assert_eq!(quarter_for(2), Quarter::Cw180);
    }

    #[test]
    fn an_unknown_orientation_is_drawn_as_it_comes() {
        for o in [0, 5, 6, u32::MAX] {
            assert_eq!(quarter_for(o), Quarter::None, "{o}");
        }
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
