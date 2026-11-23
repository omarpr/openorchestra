//! The Device tab's test controls, all through public tools: appearance,
//! text size, Home (bringing SpringBoard forward, where the input helper
//! can't press the button) and a PNG screenshot
//! through `simctl`, booting or shutting a device down, and Rotate through
//! `devicectl`.

use std::sync::OnceLock;
use std::time::Duration;

use super::simctl::{
    Xcode, failure_text, run_xcode, shot_path_ext, simctl, simctl_ran, simctl_within,
};
use super::tab_state::DeviceView;
use crate::ports::{Ran, RunFailed};

/// Booting can take a while (a first boot of a new device most of all).
const POWER_TIMEOUT: Duration = Duration::from_secs(60);
/// A `devicectl` rotate took 70-140 ms in the spike.
const DEVICECTL_TIMEOUT: Duration = Duration::from_secs(5);
/// The line `devicectl device orientation rotate --help` prints when the
/// subcommand exists. Its exit code says nothing: a missing subcommand
/// prints its parent's help and exits 0 too.
const ROTATE_USAGE: &str = "USAGE: devicectl device orientation rotate";
const SPRINGBOARD: &str = "com.apple.springboard";

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

/// Turns the device 90° clockwise (`right`), addressed by UDID: `--device`
/// also takes names, and those can be ambiguous.
pub(crate) fn rotate_args(udid: &str) -> [&str; 6] {
    ["device", "orientation", "rotate", "--device", udid, "right"]
}

/// Turns booted simulator `udid` 90° clockwise. Blocks for up to 5 s: call
/// it off the main thread.
pub(crate) fn rotate(udid: &str) -> Result<(), String> {
    let ran = run_xcode(Xcode::Devicectl, &rotate_args(udid), DEVICECTL_TIMEOUT).map_err(|why| {
        match why {
            RunFailed::Spawn => "devicectl could not start".to_string(),
            RunFailed::TimedOut => {
                format!("devicectl timed out after {} s", DEVICECTL_TIMEOUT.as_secs())
            }
        }
    })?;
    if ran.code == Some(0) {
        Ok(())
    } else {
        Err(devicectl_failure(&ran))
    }
}

/// Whether this Xcode's `devicectl` can rotate a simulator. Asked once per
/// process, and it blocks (up to 5 s): call it off the main thread. A run
/// that didn't finish isn't remembered, so a slow first start of `xcrun`
/// doesn't hide Rotate for good.
pub(crate) fn rotate_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    if let Some(&available) = AVAILABLE.get() {
        return available;
    }
    let help = ["device", "orientation", "rotate", "--help"];
    match run_xcode(Xcode::Devicectl, &help, DEVICECTL_TIMEOUT) {
        Ok(ran) => *AVAILABLE
            .get_or_init(|| lists_rotate_usage(&ran.stdout) || lists_rotate_usage(&ran.stderr)),
        Err(RunFailed::Spawn) => *AVAILABLE.get_or_init(|| false),
        Err(RunFailed::TimedOut) => false,
    }
}

/// Whether `--help` output shows the rotate subcommand's own usage line.
fn lists_rotate_usage(help: &str) -> bool {
    help.lines().any(|line| {
        line.trim_start()
            .strip_prefix(ROTATE_USAGE)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })
}

/// What a failed `devicectl` run says, trimmed; never empty. Its errors
/// open with "ERROR: ", which is dropped.
fn devicectl_failure(ran: &Ran) -> String {
    let stderr = ran.stderr.trim();
    let stderr = stderr.strip_prefix("ERROR: ").unwrap_or(stderr);
    match ran.code {
        _ if !stderr.is_empty() => stderr.to_string(),
        Some(code) => format!("devicectl failed with exit code {code}"),
        None => "devicectl was stopped by a signal".to_string(),
    }
}

/// Home without the Device input helper (which presses the real button,
/// `devices::input`): brings SpringBoard (the home screen) forward. Not a
/// button press (no lock, no Siri), but public, and it always works.
pub(crate) fn springboard_args(udid: &str) -> [&str; 3] {
    ["launch", udid, SPRINGBOARD]
}

/// `simctl launch`'s failure, with an unknown UDID ("Invalid device: …")
/// put the way `devicectl` puts it.
fn springboard_failure(why: String) -> String {
    if why.starts_with("Invalid device") {
        "The specified device was not found.".to_string()
    } else {
        why
    }
}

/// Shows booted simulator `udid`'s home screen. Blocks for up to 5 s: call
/// it off the main thread.
pub(crate) fn launch_springboard(udid: &str) -> Result<(), String> {
    simctl(&springboard_args(udid)).map(drop).map_err(springboard_failure)
}

/// The controls to offer for what the tab shows. Rotate only when `rotate`
/// says this Xcode's `devicectl` can (the caller passes
/// [`rotate_available`], asked off the main thread).
pub(crate) fn available_controls(view: &DeviceView, rotate: bool) -> Vec<Control> {
    if !matches!(view, DeviceView::Live { .. } | DeviceView::Reduced { .. }) {
        return Vec::new();
    }
    let mut controls = Vec::new();
    if rotate {
        controls.push(Control::Rotate);
    }
    controls.extend([
        Control::Home,
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

/// `udid`'s appearance now. Blocks for up to 5 s: call it off the main
/// thread.
pub(crate) fn read_appearance(udid: &str) -> Result<Appearance, String> {
    let out = simctl(&str_args(&appearance_args(udid, None)))?;
    parse_appearance(&out).ok_or_else(|| "this device has no appearance setting".to_string())
}

/// What the Dark/Light toggle does, given the appearance the tab already
/// knows the device has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TogglePlan {
    /// One call: set this (the other one).
    Set(Appearance),
    /// Not known yet: read it, then set the other.
    ReadFirst,
}

pub(crate) fn toggle_plan(known: Option<Appearance>) -> TogglePlan {
    known.map_or(TogglePlan::ReadFirst, |a| TogglePlan::Set(opposite(a)))
}

fn opposite(appearance: Appearance) -> Appearance {
    match appearance {
        Appearance::Light => Appearance::Dark,
        Appearance::Dark => Appearance::Light,
    }
}

/// Runs a control. `appearance`: what the tab knows the device's is, for
/// the toggle (None reads it first). Blocks (each tool run is bounded):
/// call it off the main thread.
pub(crate) fn run_public(
    udid: &str,
    control: Control,
    appearance: Option<Appearance>,
) -> Result<ControlOutcome, String> {
    match control {
        Control::ToggleAppearance => {
            let next = match toggle_plan(appearance) {
                TogglePlan::Set(next) => next,
                TogglePlan::ReadFirst => opposite(read_appearance(udid)?),
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
        Control::Rotate => rotate(udid).map(|()| ControlOutcome::Done),
        Control::Home => launch_springboard(udid).map(|()| ControlOutcome::Done),
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

    #[test]
    fn the_toggle_sets_the_opposite_of_what_the_tab_knows() {
        // Known: one call, setting the other.
        assert_eq!(toggle_plan(Some(Appearance::Light)), TogglePlan::Set(Appearance::Dark));
        assert_eq!(toggle_plan(Some(Appearance::Dark)), TogglePlan::Set(Appearance::Light));
        // Not read yet: read first.
        assert_eq!(toggle_plan(None), TogglePlan::ReadFirst);
    }

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

    fn ran(code: Option<i32>, stderr: &str) -> Ran {
        Ran { code, stdout: String::new(), stderr: stderr.into() }
    }

    #[test]
    fn rotate_turns_clockwise_by_udid() {
        assert_eq!(
            rotate_args("U"),
            ["device", "orientation", "rotate", "--device", "U", "right"]
        );
    }

    #[test]
    fn rotate_is_there_when_its_help_shows_its_usage() {
        // Captured from Xcode 27.2: `… orientation rotate --help`, and
        // `… orientation rotatex --help` standing in for an Xcode without the
        // subcommand (it prints the parent's help, exit 0).
        assert!(lists_rotate_usage(include_str!("testdata/devicectl_rotate_help.txt")));
        assert!(!lists_rotate_usage(include_str!("testdata/devicectl_missing_rotate_help.txt")));
        assert!(!lists_rotate_usage(""));
        assert!(!lists_rotate_usage("USAGE: devicectl device orientation rotatex --device <x>"));
    }

    #[test]
    fn devicectl_failure_says_why() {
        // Captured: an unknown UDID, exit 1.
        let not_found = "ERROR: The specified device was not found. (Name: X) \
            (com.apple.dt.CoreDeviceError error 1000 (0x3E8))\n       DeviceName = X\n";
        let text = devicectl_failure(&ran(Some(1), not_found));
        assert!(text.starts_with("The specified device was not found."), "{text}");
        assert_eq!(devicectl_failure(&ran(Some(64), "")), "devicectl failed with exit code 64");
        assert_eq!(devicectl_failure(&ran(None, " \n")), "devicectl was stopped by a signal");
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
    fn home_without_rotate() {
        assert_eq!(
            available_controls(&live(), false),
            [
                Control::Home,
                Control::ToggleAppearance,
                Control::TextSmaller,
                Control::TextBigger,
                Control::Screenshot
            ]
        );
    }

    #[test]
    fn all_controls_with_rotate_in_order() {
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
    fn home_brings_springboard_forward() {
        assert_eq!(springboard_args("U"), ["launch", "U", "com.apple.springboard"]);
    }

    #[test]
    fn home_on_a_missing_device_says_so() {
        // Captured: `simctl launch <unknown udid> com.apple.springboard`,
        // exit 148.
        assert_eq!(
            springboard_failure("Invalid device: 00000000-0000-0000-0000-000000000000".into()),
            "The specified device was not found."
        );
        assert_eq!(springboard_failure("simctl timed out after 5 s".into()), "simctl timed out after 5 s");
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

    const NO_DEVICE: &str = "00000000-0000-0000-0000-000000000000";

    fn png(udid: &str) -> Vec<u8> {
        match run_public(udid, Control::Screenshot, None) {
            Ok(ControlOutcome::Png(bytes)) => bytes,
            other => panic!("screenshot: {other:?}"),
        }
    }

    fn launch(udid: &str, bundle: &str) {
        simctl(&["launch", udid, bundle]).unwrap();
        // A first launch shows a placeholder for a moment.
        std::thread::sleep(Duration::from_secs(3));
    }

    /// The mean brightness (0-255) of the middle of a screenshot: Settings
    /// is near white (245), the home screen's wallpaper much darker (140).
    fn brightness(png: &[u8]) -> f64 {
        let image = image::load_from_memory(png).unwrap().to_luma8();
        let (w, h) = image.dimensions();
        let (mut sum, mut n) = (0.0, 0.0);
        for y in (h / 5..h * 4 / 5).step_by(7) {
            for x in (w / 5..w * 4 / 5).step_by(7) {
                sum += f64::from(image.get_pixel(x, y)[0]);
                n += 1.0;
            }
        }
        sum / n
    }

    /// Manual, on a simulator created and booted for the purpose (never
    /// anyone else's): `OO_TEST_SIM_UDID=<udid> cargo test rotate_on_a_live
    /// -- --ignored --nocapture`. Turns it four times in Safari and watches
    /// the screen stream's orientation go round.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn rotate_on_a_live_device() {
        use super::super::sim_display::{ScreenEvent, ScreenStream};
        use std::sync::mpsc;

        let udid = std::env::var("OO_TEST_SIM_UDID").expect("set OO_TEST_SIM_UDID");
        assert!(rotate_available(), "devicectl can't rotate");
        // A device fresh from boot reports "unknown"; start from portrait.
        // Safari turns with the device (the home screen and Settings don't).
        let set = ["devicectl", "device", "orientation", "set", "--device", &udid, "portrait"];
        let ran = crate::ports::run_bounded("xcrun", &set, DEVICECTL_TIMEOUT).unwrap();
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        launch(&udid, "com.apple.mobilesafari");
        let (tx, rx) = mpsc::channel();
        let stream = ScreenStream::start(&udid, move |event| {
            if let ScreenEvent::Orientation(o) = event {
                let _ = tx.send(o);
            }
        })
        .unwrap();
        let start = rx.recv_timeout(Duration::from_secs(2)).expect("the starting orientation");
        assert_eq!(start, 1, "portrait");
        let mut seen = Vec::new();
        for turn in 1..=4 {
            assert_eq!(run_public(&udid, Control::Rotate, None), Ok(ControlOutcome::Done));
            std::thread::sleep(Duration::from_millis(1500));
            let now: Vec<u32> = rx.try_iter().collect();
            eprintln!("turn {turn}: {now:?}");
            seen.extend(now);
        }
        drop(stream);
        // Safari refuses upside down, so one turn changes nothing: 3, -, 4, 1.
        assert!(seen.contains(&3) && seen.contains(&4), "{seen:?}");
        assert_eq!(seen.last(), Some(&start), "four turns come back round: {seen:?}");

        let why = rotate(NO_DEVICE).unwrap_err();
        assert!(why.starts_with("The specified device was not found."), "{why}");
    }

    /// Manual, on a simulator created and booted for the purpose:
    /// `OO_TEST_SIM_UDID=<udid> cargo test home_on_a_live -- --ignored
    /// --nocapture`. Opens Settings and goes Home, twice.
    #[test]
    #[ignore]
    fn home_on_a_live_device() {
        let udid = std::env::var("OO_TEST_SIM_UDID").expect("set OO_TEST_SIM_UDID");
        for _ in 0..2 {
            launch(&udid, "com.apple.Preferences");
            let settings = brightness(&png(&udid));
            assert_eq!(run_public(&udid, Control::Home, None), Ok(ControlOutcome::Done));
            std::thread::sleep(Duration::from_secs(1));
            let home = brightness(&png(&udid));
            eprintln!("brightness: Settings {settings:.0}, after Home {home:.0}");
            assert!(settings - home > 50.0, "Home left Settings in front");
        }
        assert_eq!(
            run_public(NO_DEVICE, Control::Home, None),
            Err("The specified device was not found.".to_string())
        );
    }

    /// Manual: `OO_TEST_SIM_UDID=<booted test device> cargo test
    /// public_controls_on -- --ignored`. Changes that device's settings.
    #[test]
    #[ignore]
    fn public_controls_on_a_live_device() {
        let udid = std::env::var("OO_TEST_SIM_UDID").expect("OO_TEST_SIM_UDID");
        let start = read_appearance(&udid).unwrap();
        // Not known: read, then set the other. Known: set the other at once.
        let other = run_public(&udid, Control::ToggleAppearance, None).unwrap();
        assert_ne!(other, ControlOutcome::Appearance(start));
        let ControlOutcome::Appearance(now) = other else { panic!("{other:?}") };
        let back = run_public(&udid, Control::ToggleAppearance, Some(now)).unwrap();
        assert_eq!(back, ControlOutcome::Appearance(start));
        assert_eq!(read_appearance(&udid).unwrap(), start);

        assert!(matches!(
            run_public(&udid, Control::TextBigger, None),
            Ok(ControlOutcome::Done | ControlOutcome::AtLimit)
        ));
        assert!(matches!(
            run_public(&udid, Control::TextSmaller, None),
            Ok(ControlOutcome::Done | ControlOutcome::AtLimit)
        ));

        let ControlOutcome::Png(bytes) = run_public(&udid, Control::Screenshot, None).unwrap() else {
            panic!("not a PNG outcome");
        };
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    /// Manual timing, on a simulator created and booted for the purpose
    /// (never anyone else's): `OO_TEST_SIM_UDID=<udid> cargo test
    /// control_timings -- --ignored --nocapture`. How long each control's
    /// run takes, start to done (what a click waits for, after the click
    /// itself), median of five; and Home through the input helper, warm and
    /// from cold.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn control_timings() {
        use std::time::Instant;
        let udid = std::env::var("OO_TEST_SIM_UDID").expect("set OO_TEST_SIM_UDID");
        let median = |mut ms: Vec<f64>| {
            ms.sort_by(f64::total_cmp);
            ms[ms.len() / 2]
        };
        let time = |f: &mut dyn FnMut()| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        };
        let run = |control| run_public(&udid, control, None).map(drop).unwrap();
        let mut rows: Vec<(&str, f64)> = Vec::new();
        let mut each = |name: &'static str, f: &mut dyn FnMut()| {
            let ms: Vec<f64> = (0..5).map(|_| time(f)).collect();
            rows.push((name, median(ms)));
        };
        each("list devices", &mut || drop(super::super::simctl::list_devices().unwrap()));
        each("Rotate", &mut || run(Control::Rotate));
        each("Home (SpringBoard)", &mut || run(Control::Home));
        // As the bar runs it: with the appearance known after the first.
        let mut known = None;
        each("Dark/Light toggle", &mut || {
            if let Ok(ControlOutcome::Appearance(now)) =
                run_public(&udid, Control::ToggleAppearance, known)
            {
                known = Some(now);
            }
        });
        each("Larger text", &mut || run(Control::TextBigger));
        each("Smaller text", &mut || run(Control::TextSmaller));
        each("Copy screenshot", &mut || run(Control::Screenshot));
        // Home through the input helper, if it was built.
        if crate::devices::input::helper_path().is_some() {
            use crate::devices::input::{HelperEvent, InputHelper};
            let wait_ready = |rx: &mut futures::channel::mpsc::UnboundedReceiver<HelperEvent>| {
                let t = Instant::now();
                while t.elapsed() < Duration::from_secs(10) {
                    if let Ok(HelperEvent::Ready(_)) = rx.try_recv() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                panic!("helper never ready");
            };
            let (events, mut rx) = futures::channel::mpsc::unbounded();
            let warm = InputHelper::spawn(&udid, 1, events).unwrap();
            wait_ready(&mut rx);
            each("Home (helper, running)", &mut || {
                warm.home().recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
            });
            drop(warm);
            let mut late = 0;
            each("Home (helper, started for it)", &mut || {
                let (events, _rx) = futures::channel::mpsc::unbounded();
                let cold = InputHelper::spawn(&udid, 2, events).unwrap();
                if !matches!(cold.home().recv_timeout(Duration::from_secs(5)), Ok(Ok(()))) {
                    late += 1;
                }
            });
            eprintln!("cold helper Home: {late} of 5 too late (not pressed)");
        }
        eprintln!("| Control | ms (median of 5) |\n|---|---|");
        for (name, ms) in rows {
            eprintln!("| {name} | {ms:.0} |");
        }
    }
}
