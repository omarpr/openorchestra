//! The Android SDK: where it is, the AVDs it knows (`emulator -list-avds`)
//! and booting one headless.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::devices::AndroidRunning;
use crate::ports::{RunFailed, run_bounded_why};

/// `emulator -list-avds` takes about 25 ms (spike).
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// A headless boot on the GPU took 13-14 s in the spike. Log very slow
/// boots while continuing to observe their success or failure.
const SLOW_BOOT: Duration = Duration::from_secs(60);
const BOOT_POLL: Duration = Duration::from_millis(250);

/// The SDK to use: `ANDROID_HOME`, then `ANDROID_SDK_ROOT`, then Android
/// Studio's default (`~/Library/Android/sdk`), the first that has an
/// emulator (`has_emulator`). An empty variable counts as unset.
pub(crate) fn sdk_root(
    android_home: Option<OsString>,
    android_sdk_root: Option<OsString>,
    home: Option<PathBuf>,
    has_emulator: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let from_env = [android_home, android_sdk_root]
        .into_iter()
        .flatten()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let default = home.map(|h| h.join("Library/Android/sdk"));
    from_env.chain(default).find(|sdk| has_emulator(&emulator_path(sdk)))
}

/// The SDK on this Mac, found once. Not finding one isn't kept, so an SDK
/// installed later is found the next time the tab lists.
pub(crate) fn sdk() -> Option<PathBuf> {
    static FOUND: OnceLock<PathBuf> = OnceLock::new();
    if let Some(sdk) = FOUND.get() {
        return Some(sdk.clone());
    }
    let sdk = sdk_root(
        std::env::var_os("ANDROID_HOME"),
        std::env::var_os("ANDROID_SDK_ROOT"),
        std::env::var_os("HOME").map(PathBuf::from),
        |p| p.is_file(),
    )?;
    Some(FOUND.get_or_init(|| sdk).clone())
}

/// The emulator launcher in `sdk`.
pub(crate) fn emulator_path(sdk: &Path) -> PathBuf {
    sdk.join("emulator").join("emulator")
}

/// The AVD ids `emulator -list-avds` printed. Anything that isn't one (a
/// log line, which has spaces) is left out.
pub(crate) fn parse_avd_list(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.contains(char::is_whitespace))
        .map(str::to_string)
        .collect()
}

/// The AVDs the SDK knows, running or not (none without an SDK). Runs
/// `emulator -list-avds` (about 25 ms, so it isn't cached): call it off
/// the main thread.
pub(crate) fn list_avds() -> Result<Vec<String>, String> {
    let Some(sdk) = sdk() else {
        return Ok(Vec::new());
    };
    let emulator = emulator_path(&sdk);
    let ran = run_bounded_why(&emulator.to_string_lossy(), &["-list-avds"], LIST_TIMEOUT)
        .map_err(|why| match why {
            RunFailed::Spawn => "the Android emulator could not start".to_string(),
            RunFailed::TimedOut => {
                format!("emulator -list-avds timed out after {} s", LIST_TIMEOUT.as_secs())
            }
        })?;
    if ran.code != Some(0) {
        return Err(boot_failure(&ran.stderr, ran.code));
    }
    Ok(parse_avd_list(&ran.stdout))
}

/// How the tab boots an AVD: headless, on the Mac's GPU (SwiftShader costs
/// about four cores; spike, 2026-11-24).
pub(crate) fn boot_args(avd_id: &str) -> [&str; 5] {
    ["-avd", avd_id, "-no-window", "-gpu", "host"]
}

/// Emulator and launcher logs use both "ERROR |" and "emulator: ERROR:".
fn log_line(line: &str) -> (Option<&str>, &str) {
    let line = line.trim();
    let line = line.strip_prefix("emulator:").map_or(line, str::trim_start);
    if let Some((tag, message)) = line.split_once(['|', ':']) {
        let tag = tag.trim();
        if !tag.is_empty() && tag.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return (Some(tag), message.trim());
        }
    }
    match line.split_whitespace().next() {
        Some(tag @ ("ERROR" | "FATAL" | "PANIC")) => (Some(tag), line[tag.len()..].trim_start()),
        _ => (None, line),
    }
}

/// Why a boot failed: its first error message, else its last log
/// message, else the process exit status.
pub(crate) fn boot_failure(log: &str, code: Option<i32>) -> String {
    let mut lines = log.lines().map(log_line).filter(|(_, message)| !message.is_empty());
    let error = lines.clone()
        .find(|(tag, _)| matches!(tag, Some("ERROR" | "FATAL" | "PANIC")))
        .or_else(|| lines.next_back());
    match (error, code) {
        (Some((_, message)), _) => message.to_string(),
        (None, Some(code)) => format!("The emulator exited with code {code}"),
        (None, None) => "The emulator was stopped by a signal".to_string(),
    }
}

/// Boots AVD `avd_id` headless, detached from the app (its own process
/// group, so it keeps running when the app quits), its output to a log in
/// the temp dir. Returns once the emulator lists itself as running, or
/// with the log's first error if it exits first. Blocks: call it off the
/// main thread.
pub(crate) fn boot(avd_id: &str) -> Result<AndroidRunning, String> {
    use std::os::unix::process::CommandExt;
    let sdk = sdk().ok_or_else(|| "The Android SDK wasn't found".to_string())?;
    let dir = std::env::temp_dir().join("openorchestra-device");
    std::fs::create_dir_all(&dir).map_err(|e| format!("the emulator's log folder: {e}"))?;
    let log_path = dir.join(format!("emulator-{avd_id}-{}.log", std::process::id()));
    let log = std::fs::File::create(&log_path).map_err(|e| format!("the emulator's log: {e}"))?;
    let err_log = log.try_clone().map_err(|e| format!("the emulator's log: {e}"))?;
    let mut child = Command::new(emulator_path(&sdk))
        .args(boot_args(avd_id))
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err_log)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("The Android emulator could not start: {e}"))?;
    let started = Instant::now();
    let mut logged_slow_boot = false;
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let log = std::fs::read_to_string(&log_path).unwrap_or_default();
                return Err(boot_failure(&log, status.code()));
            }
            Ok(None) => {}
            Err(e) => break Err(format!("watching the emulator: {e}")),
        }
        if let Some(run) = super::discovery::running_emulator(child.id()).filter(|d| d.avd_id == avd_id) {
            break Ok(run);
        }
        if !logged_slow_boot && started.elapsed() > SLOW_BOOT {
            log::info!("device: {avd_id} hasn't listed itself after {} s", SLOW_BOOT.as_secs());
            logged_slow_boot = true;
        }
        std::thread::sleep(BOOT_POLL);
    };
    // The launcher execs into the emulator, which outlives this call: reap
    // it when it exits, or its zombie would pass the pid-alive check and
    // keep a killed emulator listed.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_is_found_from_android_home_then_default() {
        let home = Some(PathBuf::from("/Users/u"));
        let default = PathBuf::from("/Users/u/Library/Android/sdk");
        let os = |s: &str| Some(OsString::from(s));
        let everywhere = |_: &Path| true;
        // ANDROID_HOME first.
        assert_eq!(
            sdk_root(os("/opt/a"), os("/opt/b"), home.clone(), everywhere),
            Some(PathBuf::from("/opt/a"))
        );
        // Then ANDROID_SDK_ROOT; an empty variable is unset.
        assert_eq!(
            sdk_root(os(""), os("/opt/b"), home.clone(), everywhere),
            Some(PathBuf::from("/opt/b"))
        );
        // Then Android Studio's default.
        assert_eq!(sdk_root(None, None, home.clone(), everywhere), Some(default.clone()));
        // A variable pointing at no emulator doesn't hide the default.
        let only_default = |p: &Path| p == emulator_path(&default);
        assert_eq!(sdk_root(os("/stale"), None, home.clone(), only_default), Some(default));
        // Nowhere: no SDK, and Android adds nothing.
        assert_eq!(sdk_root(os("/opt/a"), None, home, |_: &Path| false), None);
        assert_eq!(sdk_root(None, None, None, everywhere), None);
    }

    #[test]
    fn the_avd_list_is_one_id_a_line() {
        assert_eq!(
            parse_avd_list("Medium_Phone_API_35\nPixel_9_Pro_API_35\n"),
            ["Medium_Phone_API_35", "Pixel_9_Pro_API_35"]
        );
        assert_eq!(
            parse_avd_list("INFO    | Storing crashdata in: /tmp/x\r\n\nPixel_9_Pro_API_35\r\n"),
            ["Pixel_9_Pro_API_35"]
        );
        assert!(parse_avd_list("").is_empty());
    }

    #[test]
    fn an_avd_boots_headless_on_the_gpu() {
        assert_eq!(
            boot_args("Pixel_9_Pro_API_35"),
            ["-avd", "Pixel_9_Pro_API_35", "-no-window", "-gpu", "host"]
        );
    }

    #[test]
    fn launcher_errors_win_over_follow_up_hints() {
        for (line, cause) in [
            ("emulator: ERROR: emulator is already running", "emulator is already running"),
            ("PANIC: hardware acceleration is required", "hardware acceleration is required"),
            ("emulator: FATAL | could not start the guest", "could not start the guest"),
            ("ERROR unable to start: unavailable", "unable to start: unavailable"),
        ] {
            let log = format!("INFO   | starting the emulator\n{line}\nPlease check your configuration.");
            assert_eq!(boot_failure(&log, Some(1)), cause);
        }
    }

    #[test]
    fn a_failed_boot_says_the_emulators_first_error() {
        let log = "INFO    | Android emulator version 35.2.10.0\n\
                   ERROR   | Unknown AVD name [Nope], use -list-avds to see valid list.\n\
                   ERROR   | HOME is defined but there is no file Nope.ini\n";
        assert_eq!(
            boot_failure(log, Some(1)),
            "Unknown AVD name [Nope], use -list-avds to see valid list."
        );
        let fatal = "INFO    | x\nFATAL   | Running multiple emulators with the same AVD\n";
        assert_eq!(boot_failure(fatal, Some(1)), "Running multiple emulators with the same AVD");
        assert_eq!(boot_failure("INFO    | x\nsomething broke\n\n", Some(1)), "something broke");
        assert_eq!(boot_failure("", Some(3)), "The emulator exited with code 3");
        assert_eq!(boot_failure("  \n", None), "The emulator was stopped by a signal");
    }
}
