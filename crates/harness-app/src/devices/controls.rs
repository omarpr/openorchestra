//! The Device tab's test controls that need only public `simctl`: appearance,
//! text size and a PNG screenshot, and booting or shutting a device down.
//! Rotate and Home (private HID) are offered only when the caller says that
//! path is available.

// Only boot and shutdown are wired in so far; the bar (Task 3) calls the
// rest.
#![allow(dead_code)]

use std::time::Duration;

use super::simctl::{failure_text, shot_path_ext, simctl, simctl_ran, simctl_within};
use super::tab_state::DeviceView;

/// Booting can take a while (a first boot of a new device most of all).
const POWER_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    Rotate,
    Home,
    ToggleAppearance,
    TextSmaller,
    TextBigger,
    Screenshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Appearance {
    Light,
    Dark,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ControlOutcome {
    Done,
    Appearance(Appearance),
    /// The text size is already the smallest or largest.
    AtLimit,
    Png(Vec<u8>),
}

/// `simctl ui <udid> appearance` prints "light", "dark", "unsupported" or
/// "unknown".
pub(crate) fn parse_appearance(stdout: &str) -> Option<Appearance> {
    match stdout.trim() {
        "light" => Some(Appearance::Light),
        "dark" => Some(Appearance::Dark),
        _ => None,
    }
}

pub(crate) fn appearance_args(udid: &str, set: Option<Appearance>) -> Vec<String> {
    let mut args = vec!["ui".to_string(), udid.to_string(), "appearance".to_string()];
    match set {
        Some(Appearance::Light) => args.push("light".to_string()),
        Some(Appearance::Dark) => args.push("dark".to_string()),
        None => {}
    }
    args
}

pub(crate) fn content_size_args(udid: &str, bigger: bool) -> Vec<String> {
    let step = if bigger { "increment" } else { "decrement" };
    ["ui", udid, "content_size", step].map(String::from).to_vec()
}

pub(crate) fn boot_args(udid: &str) -> [&str; 2] {
    ["boot", udid]
}

pub(crate) fn shutdown_args(udid: &str) -> [&str; 2] {
    ["shutdown", udid]
}

/// Boots `udid` headless (no Simulator.app window). Blocks for up to a
/// minute: call it off the main thread.
pub(crate) fn boot(udid: &str) -> Result<(), String> {
    simctl_within(&boot_args(udid), POWER_TIMEOUT).map(drop)
}

/// Shuts `udid` down. Blocks for up to a minute: call it off the main
/// thread.
pub(crate) fn shutdown(udid: &str) -> Result<(), String> {
    simctl_within(&shutdown_args(udid), POWER_TIMEOUT).map(drop)
}

/// The controls to offer for what the tab shows. Rotate and Home go through
/// the private HID path, so only when `hid` says it is there.
pub(crate) fn available_controls(view: &DeviceView, hid: bool) -> Vec<Control> {
    if !matches!(view, DeviceView::Live { .. } | DeviceView::Reduced { .. }) {
        return Vec::new();
    }
    let mut controls = Vec::new();
    if hid {
        controls.extend([Control::Rotate, Control::Home]);
    }
    controls.extend([
        Control::ToggleAppearance,
        Control::TextSmaller,
        Control::TextBigger,
        Control::Screenshot,
    ]);
    controls
}

/// What a `content_size increment|decrement` run means. At the largest size
/// simctl exits 0 and prints "Already at max content size" on stdout; at the
/// smallest it is silent, which cannot be told from a step that worked.
fn content_size_outcome(code: i32, stdout: &str, stderr: &str) -> Result<ControlOutcome, String> {
    let says_limit = |text: &str| text.contains("Already at") && text.contains("content size");
    if says_limit(stdout) || says_limit(stderr) {
        return Ok(ControlOutcome::AtLimit);
    }
    if code != 0 {
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("simctl failed with exit code {code}")
        } else {
            stderr.to_string()
        });
    }
    Ok(ControlOutcome::Done)
}

fn str_args(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

fn read_appearance(udid: &str) -> Result<Appearance, String> {
    let out = simctl(&str_args(&appearance_args(udid, None)))?;
    parse_appearance(&out).ok_or_else(|| "this device has no appearance setting".to_string())
}

/// Runs a control that needs only public `simctl`. `Rotate` and `Home` are
/// not among them.
pub(crate) fn run_public(udid: &str, control: Control) -> Result<ControlOutcome, String> {
    match control {
        Control::ToggleAppearance => {
            let next = match read_appearance(udid)? {
                Appearance::Light => Appearance::Dark,
                Appearance::Dark => Appearance::Light,
            };
            simctl(&str_args(&appearance_args(udid, Some(next))))?;
            Ok(ControlOutcome::Appearance(next))
        }
        Control::TextSmaller | Control::TextBigger => {
            let args = content_size_args(udid, control == Control::TextBigger);
            let ran = simctl_ran(&str_args(&args))?;
            match ran.code {
                Some(code) => content_size_outcome(code, &ran.stdout, &ran.stderr),
                None => Err(failure_text(&ran)),
            }
        }
        Control::Screenshot => screenshot_png(udid).map(ControlOutcome::Png),
        Control::Rotate | Control::Home => Err("this control needs the device input path".into()),
    }
}

/// A PNG screenshot: simctl writes only to a file, so it goes to a fresh
/// file of its own, is read back and removed.
fn screenshot_png(udid: &str) -> Result<Vec<u8>, String> {
    let dir = std::env::temp_dir().join("openorchestra-device");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = shot_path_ext(&dir, udid, "png");
    let path_str = path.to_string_lossy();
    let result = simctl(&["io", udid, "screenshot", "--type=png", &path_str])
        .and_then(|_| std::fs::read(&path).map_err(|e| e.to_string()));
    let _ = std::fs::remove_file(&path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live() -> DeviceView {
        DeviceView::Live { udid: "U".into() }
    }

    fn reduced() -> DeviceView {
        DeviceView::Reduced { udid: "U".into(), why: "x".into(), transient: true }
    }

    #[test]
    fn parses_light_and_dark() {
        assert_eq!(parse_appearance("light\n"), Some(Appearance::Light));
        assert_eq!(parse_appearance("dark"), Some(Appearance::Dark));
    }

    #[test]
    fn unsupported_appearance_is_none() {
        assert_eq!(parse_appearance("unsupported\n"), None);
        assert_eq!(parse_appearance("unknown"), None);
        assert_eq!(parse_appearance(""), None);
    }

    #[test]
    fn appearance_args_get_and_set() {
        assert_eq!(appearance_args("U", None), ["ui", "U", "appearance"]);
        assert_eq!(appearance_args("U", Some(Appearance::Dark)), ["ui", "U", "appearance", "dark"]);
        assert_eq!(appearance_args("U", Some(Appearance::Light)), ["ui", "U", "appearance", "light"]);
    }

    #[test]
    fn content_size_args_increment_and_decrement() {
        assert_eq!(content_size_args("U", true), ["ui", "U", "content_size", "increment"]);
        assert_eq!(content_size_args("U", false), ["ui", "U", "content_size", "decrement"]);
    }

    #[test]
    fn boot_and_shutdown_args() {
        assert_eq!(boot_args("U"), ["boot", "U"]);
        assert_eq!(shutdown_args("U"), ["shutdown", "U"]);
    }

    #[test]
    fn no_controls_when_nothing_is_running() {
        for view in [
            DeviceView::NeedsXcode,
            DeviceView::NoneRunning,
            DeviceView::ShutDown { name: "iPhone".into() },
        ] {
            assert!(available_controls(&view, true).is_empty(), "{view:?}");
        }
    }

    #[test]
    fn public_controls_without_hid() {
        assert_eq!(
            available_controls(&live(), false),
            [Control::ToggleAppearance, Control::TextSmaller, Control::TextBigger, Control::Screenshot]
        );
    }

    #[test]
    fn all_controls_with_hid_in_order() {
        assert_eq!(
            available_controls(&live(), true),
            [
                Control::Rotate,
                Control::Home,
                Control::ToggleAppearance,
                Control::TextSmaller,
                Control::TextBigger,
                Control::Screenshot
            ]
        );
    }

    #[test]
    fn reduced_mode_keeps_public_controls() {
        assert_eq!(available_controls(&reduced(), false), available_controls(&live(), false));
        assert_eq!(available_controls(&reduced(), true), available_controls(&live(), true));
    }

    #[test]
    fn text_size_at_the_limit_is_quiet() {
        // Captured from a real iOS 27.2 simulator: at the largest size,
        // `content_size increment` exits 0 and says so on stdout.
        assert_eq!(
            content_size_outcome(0, "Already at max content size\n", ""),
            Ok(ControlOutcome::AtLimit)
        );
        // At the smallest size `decrement` is silent, and exits 0.
        assert_eq!(content_size_outcome(0, "", ""), Ok(ControlOutcome::Done));
        // A real failure still says so.
        assert_eq!(
            content_size_outcome(164, "", "Invalid device: X\n"),
            Err("Invalid device: X".to_string())
        );
    }

    /// Manual: `OO_TEST_SIM_UDID=<booted test device you created> cargo test
    /// boot_and_shutdown_on -- --ignored`. Shuts that device down and boots
    /// it again; never point it at a device you didn't create.
    #[test]
    #[ignore]
    fn boot_and_shutdown_on_a_live_device() {
        use super::super::simctl::list_devices;
        let udid = std::env::var("OO_TEST_SIM_UDID").expect("OO_TEST_SIM_UDID");
        let group = |udid: &str| {
            let list = list_devices().unwrap();
            if list.booted.iter().any(|d| d.udid == udid) {
                "booted"
            } else if list.shut_down.iter().any(|d| d.udid == udid) {
                "shut down"
            } else {
                "neither"
            }
        };
        assert_eq!(group(&udid), "booted");

        shutdown(&udid).unwrap();
        assert_eq!(group(&udid), "shut down", "right after shutdown returns");
        // A second shutdown fails, and its note line says why.
        let again = shutdown(&udid).unwrap_err();
        eprintln!("shutdown when shut down: {again:?}");

        boot(&udid).unwrap();
        assert_eq!(group(&udid), "booted", "right after boot returns");
        let again = boot(&udid).unwrap_err();
        eprintln!("boot when booted: {again:?}");
    }

    /// Manual: `OO_TEST_SIM_UDID=<booted test device> cargo test
    /// public_controls_on -- --ignored`. Changes that device's settings.
    #[test]
    #[ignore]
    fn public_controls_on_a_live_device() {
        let udid = std::env::var("OO_TEST_SIM_UDID").expect("OO_TEST_SIM_UDID");
        let start = read_appearance(&udid).unwrap();
        let other = run_public(&udid, Control::ToggleAppearance).unwrap();
        assert_ne!(other, ControlOutcome::Appearance(start));
        let back = run_public(&udid, Control::ToggleAppearance).unwrap();
        assert_eq!(back, ControlOutcome::Appearance(start));
        assert_eq!(read_appearance(&udid).unwrap(), start);

        assert!(matches!(
            run_public(&udid, Control::TextBigger),
            Ok(ControlOutcome::Done | ControlOutcome::AtLimit)
        ));
        assert!(matches!(
            run_public(&udid, Control::TextSmaller),
            Ok(ControlOutcome::Done | ControlOutcome::AtLimit)
        ));

        let ControlOutcome::Png(bytes) = run_public(&udid, Control::Screenshot).unwrap() else {
            panic!("not a PNG outcome");
        };
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
}
