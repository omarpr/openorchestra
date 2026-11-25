//! The Device tab's bar on an Android emulator (see "Spike findings" 6, 8
//! and 9 in the Android Device tab design):
//!
//! - Home and Back: `sendKey` with the key names "GoHome" and "GoBack".
//! - Rotate: `setPhysicalModel(ROTATION, [0, 0, z])`, z a quarter turn
//!   clockwise from what `getPhysicalModel` reports, within [−180, 180).
//! - Copy screenshot: `getScreenshot` as a PNG.
//! - Shut down: `setVmState(SHUTDOWN)`.
//! - Dark/Light and Text size: `adb -s emulator-<serial port> shell` with
//!   `cmd uimode night` and `settings … system font_scale`, offered only
//!   when the SDK has adb.
//!
//! Each run connects afresh (localhost: a few ms) and blocks, every call
//! and adb run bounded: call it off the main thread.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::grpc::image_format::ImgFormat;
use super::grpc::physical_model_value::PhysicalType;
use super::grpc::vm_run_state::RunState;
use super::grpc::{ImageFormat, ParameterValue, PhysicalModelValue, VmRunState};
use super::input::{BACK_KEY, HOME_KEY, named_key};
use super::stream::{Client, authorized, connect, status_text, without_token};
use crate::devices::AndroidRunning;
use crate::devices::controls::{Appearance, Control, ControlOutcome};
use crate::ports::{RunFailed, run_bounded_why};

/// The text sizes Text size steps through, smallest first, as `settings`
/// is given them.
pub(crate) const FONT_SCALES: [&str; 4] = ["0.85", "1.0", "1.15", "1.3"];
/// A screenshot of a big screen is a large PNG; the rest answer at once.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// An adb shell command on a running emulator takes a few tens of ms.
const ADB_TIMEOUT: Duration = Duration::from_secs(5);
/// Android's own power-off, through adb.
pub(crate) const POWER_OFF: [&str; 2] = ["reboot", "-p"];
/// How long a graceful power-off gets, the adb run and the emulator's exit
/// together (3 to 9 s seen), before the VM is stopped instead.
pub(crate) const GRACEFUL_WAIT: Duration = Duration::from_secs(15);
/// How long the VM's stop gets to end the emulator's process.
const HARD_WAIT: Duration = Duration::from_secs(5);

/// adb in the SDK at `sdk`.
pub(crate) fn adb_path(sdk: &Path) -> PathBuf {
    sdk.join("platform-tools").join("adb")
}

/// adb, when the SDK has it. Looked for each time it is asked (off the
/// main thread, with each list): it may be installed later.
pub(crate) fn adb() -> Option<PathBuf> {
    super::sdk::sdk().map(|sdk| adb_path(&sdk)).filter(|adb| adb.is_file())
}

/// The arguments that run `command` in the shell of the emulator whose
/// console is on `serial_port`.
pub(crate) fn adb_shell_args(serial_port: u16, command: &[&str]) -> Vec<String> {
    let mut args = vec!["-s".to_string(), format!("emulator-{serial_port}"), "shell".to_string()];
    args.extend(command.iter().map(|arg| arg.to_string()));
    args
}

/// `cmd uimode night` prints "Night mode: yes" or "Night mode: no" (also
/// "auto" or "custom", which are neither).
pub(crate) fn parse_night(stdout: &str) -> Option<Appearance> {
    match stdout.trim().strip_prefix("Night mode:")?.trim() {
        "yes" => Some(Appearance::Dark),
        "no" => Some(Appearance::Light),
        _ => None,
    }
}

/// `settings get system font_scale`: "null" until it was ever set, which
/// is 1.0.
pub(crate) fn parse_font_scale(stdout: &str) -> Option<f32> {
    match stdout.trim() {
        "" | "null" => Some(1.0),
        value => value.parse::<f32>().ok().filter(|v| v.is_finite() && *v > 0.0),
    }
}

/// The text size one step smaller or bigger (`bigger`) than `current`, in
/// [`FONT_SCALES`]; None at that end. A size between steps goes to the
/// next one in that direction.
pub(crate) fn next_font_scale(current: f32, bigger: bool) -> Option<&'static str> {
    // Within this of a step is on it (settings keeps a float).
    const SAME: f32 = 0.01;
    let scale = |step: &&str| step.parse::<f32>().unwrap_or(1.0);
    if bigger {
        FONT_SCALES.iter().find(|step| scale(step) > current + SAME).copied()
    } else {
        FONT_SCALES.iter().rev().find(|step| scale(step) < current - SAME).copied()
    }
}

/// The z rotation a quarter turn clockwise from `z` (each in [−180, 180),
/// a quarter turn's multiple): 0 → −90 → −180 → 90 → 0, as `adb emu
/// rotate` steps. An angle between quarters goes from the nearest.
pub(crate) fn rotation_after(z: f32) -> f32 {
    let quarter = if z.is_finite() { (z / 90.0).round() } else { 0.0 };
    // Quarters from −2 (−180) to 1 (90).
    let next = (quarter - 1.0 + 2.0).rem_euclid(4.0) - 2.0;
    next * 90.0
}

/// Runs a control on `dev`. `_appearance` is the tab's cached value for
/// display; a toggle reads the guest rather than trusting that cache.
/// Blocks: call it off the main thread.
pub(crate) fn run(
    dev: &AndroidRunning,
    control: Control,
    _appearance: Option<Appearance>,
) -> Result<ControlOutcome, String> {
    let token = dev.token.clone();
    match control {
        Control::Home | Control::Back => {
            let name = if control == Control::Home { HOME_KEY } else { BACK_KEY };
            let key = authorized(named_key(name), token.as_deref())?;
            call(dev, async move |mut c: Client| c.send_key(key).await.map(drop))?;
            Ok(ControlOutcome::Done)
        }
        Control::Rotate => {
            let ask = PhysicalModelValue { target: PhysicalType::Rotation as i32, ..Default::default() };
            let ask = authorized(ask, token.as_deref())?;
            call(dev, async move |mut c: Client| {
                let now = c.get_physical_model(ask).await?.into_inner();
                let z = now.value.and_then(|v| v.data.get(2).copied()).unwrap_or(0.0);
                let turned = PhysicalModelValue {
                    target: PhysicalType::Rotation as i32,
                    value: Some(ParameterValue { data: vec![0.0, 0.0, rotation_after(z)] }),
                    ..Default::default()
                };
                let turned = authorized(turned, token.as_deref()).map_err(tonic::Status::internal)?;
                c.set_physical_model(turned).await.map(drop)
            })?;
            Ok(ControlOutcome::Done)
        }
        Control::Screenshot => {
            let png = ImageFormat { format: ImgFormat::Png as i32, ..Default::default() };
            let png = authorized(png, token.as_deref())?;
            let image = call(dev, async move |mut c: Client| {
                c.get_screenshot(png).await.map(|r| r.into_inner().image)
            })?;
            if image.is_empty() {
                return Err("The emulator sent an empty screenshot".into());
            }
            Ok(ControlOutcome::Png(image))
        }
        Control::ToggleAppearance => {
            let next = toggled(night_mode(dev)?);
            let night = if next == Appearance::Dark { "yes" } else { "no" };
            adb_shell(dev, &["cmd", "uimode", "night", night])?;
            Ok(ControlOutcome::Appearance(next))
        }
        Control::TextSmaller | Control::TextBigger => {
            let now = adb_shell(dev, &["settings", "get", "system", "font_scale"])?;
            let now = parse_font_scale(&now).unwrap_or(1.0);
            match next_font_scale(now, control == Control::TextBigger) {
                None => Ok(ControlOutcome::AtLimit),
                Some(scale) => {
                    adb_shell(dev, &["settings", "put", "system", "font_scale", scale])?;
                    Ok(ControlOutcome::Done)
                }
            }
        }
    }
}

/// What the toggle sets when the appearance had to be read: the other one,
/// or dark when night mode is neither on nor off ("auto", "custom"), so
/// the button still does something.
pub(crate) fn toggled(read: Option<Appearance>) -> Appearance {
    match read {
        Some(Appearance::Dark) => Appearance::Light,
        Some(Appearance::Light) | None => Appearance::Dark,
    }
}

/// `dev`'s night mode now, through adb: None when it is neither on nor
/// off.
fn night_mode(dev: &AndroidRunning) -> Result<Option<Appearance>, String> {
    adb_shell(dev, &["cmd", "uimode", "night"]).map(|out| parse_night(&out))
}

/// `dev`'s appearance now, through adb. Only the live test asks directly;
/// the bar reads it as part of each toggle, never unasked.
#[cfg(test)]
pub(crate) fn read_appearance(dev: &AndroidRunning) -> Result<Appearance, String> {
    night_mode(dev)?.ok_or_else(|| "night mode is neither on nor off".to_string())
}

/// Shuts `dev` down ([`power_off_steps`]): Android's own power-off when
/// the SDK has adb, so nothing written just before is lost; the VM's stop
/// if that fails or the emulator is still running [`GRACEFUL_WAIT`] later.
/// Blocks (up to about 30 s: [`GRACEFUL_WAIT`], then the VM's stop's call
/// and [`HARD_WAIT`]): call it off the main thread.
pub(crate) fn shutdown(dev: &AndroidRunning) -> Result<(), String> {
    let mut failed = String::new();
    for step in power_off_steps(adb().is_some()) {
        let tried = match step {
            PowerOff::Graceful => power_off_gracefully(dev),
            PowerOff::Hard => power_off_hard(dev),
        };
        match tried {
            Ok(()) => return Ok(()),
            Err(why) => {
                log::info!("device: {step:?} power-off of {} failed: {why}", dev.avd_id);
                failed = why;
            }
        }
    }
    Err(failed)
}

/// `adb shell reboot -p`, then waits for the emulator to exit, all within
/// [`GRACEFUL_WAIT`]. Nothing when it has already exited: another emulator
/// may have its console port.
fn power_off_gracefully(dev: &AndroidRunning) -> Result<(), String> {
    still_running(dev, super::discovery::still_emulator)?;
    let adb = adb().ok_or_else(|| "adb isn't in the Android SDK".to_string())?;
    let started = std::time::Instant::now();
    let args = adb_shell_args(dev.serial_port, &POWER_OFF);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // As the guest goes down, adb may lose it before answering: only the
    // emulator's exit says it worked.
    let ran = run_bounded_why(&adb.to_string_lossy(), &args, GRACEFUL_WAIT);
    let left = GRACEFUL_WAIT.saturating_sub(started.elapsed());
    if exited_within(dev, left) {
        return Ok(());
    }
    Err(match ran {
        Err(RunFailed::Spawn) => "adb could not start".to_string(),
        Err(RunFailed::TimedOut) => format!("adb reboot -p timed out after {} s", GRACEFUL_WAIT.as_secs()),
        Ok(_) => format!("the emulator was still running {} s after reboot -p", GRACEFUL_WAIT.as_secs()),
    })
}

/// `setVmState(SHUTDOWN)`, then waits for the emulator to exit (about
/// 2 s). It may close the connection as it goes: that is no failure once
/// its process has exited.
fn power_off_hard(dev: &AndroidRunning) -> Result<(), String> {
    let down = VmRunState { state: RunState::Shutdown as i32 };
    let down = authorized(down, dev.token.as_deref())?;
    let stopped = call(dev, async move |mut c: Client| c.set_vm_state(down).await.map(drop));
    if exited_within(dev, HARD_WAIT) {
        return Ok(());
    }
    Err(stopped.err().unwrap_or_else(|| {
        format!("the emulator was still running {} s after setVmState(SHUTDOWN)", HARD_WAIT.as_secs())
    }))
}

/// Whether `dev`'s process exits within `wait` (polled).
fn exited_within(dev: &AndroidRunning, wait: Duration) -> bool {
    let started = std::time::Instant::now();
    loop {
        if !super::discovery::still_emulator(dev.pid) {
            return true;
        }
        if started.elapsed() >= wait {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// One way to power an emulator off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PowerOff {
    /// `adb shell reboot -p`: Android shuts down, writing what it holds.
    Graceful,
    /// `setVmState(SHUTDOWN)`: the VM stops at once, and writes from its
    /// last second or two are lost.
    Hard,
}

/// The ways Shut down tries, in order, each only if the one before failed
/// or the emulator didn't exit in time: graceful first when the SDK has
/// adb (`adb`).
pub(crate) fn power_off_steps(adb: bool) -> Vec<PowerOff> {
    if adb { vec![PowerOff::Graceful, PowerOff::Hard] } else { vec![PowerOff::Hard] }
}

/// Connects to `dev` and makes the calls in `calls`, bounded by
/// [`CALL_TIMEOUT`]; an error is one line, without the token.
fn call<T>(
    dev: &AndroidRunning,
    calls: impl AsyncFnOnce(Client) -> Result<T, tonic::Status>,
) -> Result<T, String> {
    still_running(dev, super::discovery::still_emulator)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("starting the control's runtime: {e}"))?;
    let port = dev.grpc_port;
    let result = runtime.block_on(async move {
        let client = connect(port).await.map_err(|(_, why)| why)?;
        match tokio::time::timeout(CALL_TIMEOUT, calls(client)).await {
            Ok(result) => result.map_err(|status| status_text(&status)),
            Err(_) => Err(format!("The emulator didn't answer within {} s", CALL_TIMEOUT.as_secs())),
        }
    });
    result.map_err(|why| without_token(&why, dev.token.as_deref()))
}

/// An error unless `dev` still runs, by `alive` (the emulator's pid still
/// its qemu). Asked before anything is sent: adb finds an emulator by its
/// console port, which another may have taken once `dev` exited.
fn still_running(dev: &AndroidRunning, alive: impl Fn(u32) -> bool) -> Result<(), String> {
    if alive(dev.pid) { Ok(()) } else { Err("The emulator has shut down".into()) }
}

/// Runs `command` in `dev`'s shell through adb; its stdout.
fn adb_shell(dev: &AndroidRunning, command: &[&str]) -> Result<String, String> {
    still_running(dev, super::discovery::still_emulator)?;
    let adb = adb().ok_or_else(|| "adb isn't in the Android SDK".to_string())?;
    let args = adb_shell_args(dev.serial_port, command);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = run_bounded_why(&adb.to_string_lossy(), &args, ADB_TIMEOUT).map_err(|why| match why {
        RunFailed::Spawn => "adb could not start".to_string(),
        RunFailed::TimedOut => format!("adb timed out after {} s", ADB_TIMEOUT.as_secs()),
    })?;
    if ran.code == Some(0) {
        return Ok(ran.stdout);
    }
    let stderr = ran.stderr.trim();
    Err(match ran.code {
        _ if !stderr.is_empty() => format!("adb: {}", stderr.lines().next().unwrap_or_default()),
        Some(code) => format!("adb failed with exit code {code}"),
        None => "adb was stopped by a signal".to_string(),
    })
}

/// Restores a live test AVD's exact font-scale setting, including an unset
/// value, on every scope exit. Cleanup must not panic while already unwinding.
#[cfg(test)]
pub(crate) struct FontScaleRestore {
    dev: AndroidRunning,
    raw: String,
}

#[cfg(test)]
impl FontScaleRestore {
    pub(crate) fn new(dev: &AndroidRunning) -> Self {
        let raw = adb_shell(dev, &["settings", "get", "system", "font_scale"])
            .expect("read the original font scale");
        Self { dev: dev.clone(), raw }
    }

    pub(crate) fn original(&self) -> &str {
        self.raw.trim()
    }
}

#[cfg(test)]
impl Drop for FontScaleRestore {
    fn drop(&mut self) {
        let result = match self.original() {
            "" | "null" => adb_shell(&self.dev, &["settings", "delete", "system", "font_scale"]),
            value => adb_shell(&self.dev, &["settings", "put", "system", "font_scale", value]),
        };
        if let Err(why) = result {
            eprintln!("device: restoring the test font scale failed: {why}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_scale_steps_and_stops_at_the_ends() {
        // From the default, one step each way.
        assert_eq!(next_font_scale(1.0, true), Some("1.15"));
        assert_eq!(next_font_scale(1.0, false), Some("0.85"));
        assert_eq!(next_font_scale(1.15, true), Some("1.3"));
        assert_eq!(next_font_scale(0.85, true), Some("1.0"));
        // At the ends: no further.
        assert_eq!(next_font_scale(1.3, true), None);
        assert_eq!(next_font_scale(0.85, false), None);
        // Past the ends (set elsewhere): back inside, or no further.
        assert_eq!(next_font_scale(2.0, false), Some("1.3"));
        assert_eq!(next_font_scale(2.0, true), None);
        assert_eq!(next_font_scale(0.5, true), Some("0.85"));
        // Between steps: the next one that way.
        assert_eq!(next_font_scale(1.1, true), Some("1.15"));
        assert_eq!(next_font_scale(1.1, false), Some("1.0"));
        // A float that settings rounded is still its step.
        assert_eq!(next_font_scale(1.149_999_9, true), Some("1.3"));
        // What settings says.
        assert_eq!(parse_font_scale("1.15\n"), Some(1.15));
        assert_eq!(parse_font_scale("null\n"), Some(1.0));
        assert_eq!(parse_font_scale(""), Some(1.0));
        assert_eq!(parse_font_scale("huh"), None);
    }

    #[test]
    fn shutdown_is_graceful_with_adb_else_hard() {
        // With adb: Android's own power-off, then the VM's if that fails
        // or the emulator is still there after it.
        assert_eq!(power_off_steps(true), [PowerOff::Graceful, PowerOff::Hard]);
        // Without: the VM's alone.
        assert_eq!(power_off_steps(false), [PowerOff::Hard]);
        assert_eq!(
            adb_shell_args(5554, &POWER_OFF),
            ["-s", "emulator-5554", "shell", "reboot", "-p"]
        );
        assert!(GRACEFUL_WAIT <= Duration::from_secs(15));
    }

    /// adb finds an emulator by its console port, which another emulator
    /// may have taken since: nothing goes to it unless the emulator shown
    /// still runs. This test process is alive but no emulator, as a reused
    /// pid would be.
    #[test]
    fn adb_reaches_only_the_emulator_shown() {
        let dev = AndroidRunning {
            pid: std::process::id(),
            avd_id: "Pixel_9_Pro_API_35".into(),
            name: "Pixel 9 Pro API 35".into(),
            grpc_port: 1,
            token: None,
            serial_port: 1,
            avd_dir: PathBuf::new(),
        };
        let shut = Err("The emulator has shut down".to_string());
        assert_eq!(still_running(&dev, super::super::discovery::still_emulator), shut);
        assert_eq!(still_running(&dev, |_| false), shut);
        assert_eq!(still_running(&dev, |pid| pid == dev.pid), Ok(()));
    }

    #[test]
    fn night_mode_reads_as_the_appearance() {
        assert_eq!(parse_night("Night mode: yes\n"), Some(Appearance::Dark));
        assert_eq!(parse_night("Night mode: no\n"), Some(Appearance::Light));
        assert_eq!(parse_night("Night mode: auto\n"), None);
        assert_eq!(parse_night(""), None);
        // The toggle, once read: the other one; from auto or custom, dark.
        assert_eq!(toggled(Some(Appearance::Dark)), Appearance::Light);
        assert_eq!(toggled(Some(Appearance::Light)), Appearance::Dark);
        assert_eq!(toggled(None), Appearance::Dark);
    }

    #[test]
    fn adb_runs_in_the_emulators_shell() {
        assert_eq!(
            adb_shell_args(5554, &["cmd", "uimode", "night", "yes"]),
            ["-s", "emulator-5554", "shell", "cmd", "uimode", "night", "yes"]
        );
        assert_eq!(
            adb_path(Path::new("/sdk")),
            PathBuf::from("/sdk/platform-tools/adb")
        );
    }

    /// Manual timing, on an AVD booted for the purpose (never anyone
    /// else's): `OO_TEST_AVD=<avd> cargo test android_control_timings --
    /// --ignored --nocapture`. How long each control's run takes, start to
    /// done (what a click waits for), median of four, each run an even
    /// number of times (or in pairs) so the AVD ends as it began.
    #[test]
    #[ignore]
    fn android_control_timings() {
        use std::time::Instant;
        let avd = std::env::var("OO_TEST_AVD").expect("set OO_TEST_AVD");
        let dev = super::super::discovery::running_emulators()
            .into_iter()
            .find(|r| r.avd_id == avd)
            .expect("the AVD is running");
        let median = |mut ms: Vec<f64>| {
            ms.sort_by(f64::total_cmp);
            ms[ms.len() / 2]
        };
        let time = |control, appearance| {
            let at = Instant::now();
            let out = run(&dev, control, appearance);
            assert!(out.is_ok(), "{control:?}: {out:?}");
            (at.elapsed().as_secs_f64() * 1000.0, out.unwrap())
        };
        let mut rows = Vec::new();
        for (name, control) in [
            ("Home", Control::Home),
            ("Back", Control::Back),
            ("Rotate", Control::Rotate),
            ("Copy screenshot", Control::Screenshot),
        ] {
            let ms = (0..4).map(|_| time(control, None).0).collect();
            rows.push((name, median(ms)));
        }
        // Read first (the first click), then known.
        let (read_first, first) = time(Control::ToggleAppearance, None);
        let ControlOutcome::Appearance(mut now) = first else { panic!("{first:?}") };
        let mut known = Vec::new();
        for _ in 0..3 {
            let (ms, out) = time(Control::ToggleAppearance, Some(now));
            let ControlOutcome::Appearance(next) = out else { panic!("{out:?}") };
            now = next;
            known.push(ms);
        }
        rows.push(("Dark/Light (read first)", read_first));
        rows.push(("Dark/Light (known)", median(known)));
        let restore = FontScaleRestore::new(&dev);
        let original = restore.original().to_string();
        let (mut bigger, mut smaller) = (Vec::new(), Vec::new());
        for _ in 0..4 {
            bigger.push(time(Control::TextBigger, None).0);
            smaller.push(time(Control::TextSmaller, None).0);
        }
        drop(restore);
        let restored = adb_shell(&dev, &["settings", "get", "system", "font_scale"]).unwrap();
        assert_eq!(restored.trim(), original);
        rows.push(("Larger text", median(bigger)));
        rows.push(("Smaller text", median(smaller)));
        eprintln!("| Control | ms (median) |\n|---|---|");
        for (name, ms) in rows {
            eprintln!("| {name} | {ms:.0} |");
        }
    }

    #[test]
    fn rotate_steps_a_quarter_turn_clockwise() {
        assert_eq!(rotation_after(0.0), -90.0);
        assert_eq!(rotation_after(-90.0), -180.0);
        assert_eq!(rotation_after(-180.0), 90.0);
        // 180 is reported as −180.
        assert_eq!(rotation_after(180.0), 90.0);
        assert_eq!(rotation_after(90.0), 0.0);
        // Near a quarter (a tilt): from that quarter.
        assert_eq!(rotation_after(-4.75), -90.0);
        assert_eq!(rotation_after(87.0), 0.0);
        // Four turns come back round.
        let mut z = 0.0;
        for _ in 0..4 {
            z = rotation_after(z);
            assert!((-180.0..180.0).contains(&z), "{z}");
        }
        assert_eq!(z, 0.0);
    }
}
