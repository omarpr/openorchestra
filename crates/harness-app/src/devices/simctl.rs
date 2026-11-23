//! `xcrun simctl` wrappers and the parsing of its JSON.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;

use super::DeviceInfo;
use crate::ports::{Ran, RunFailed, run_bounded, run_bounded_why};

const TIMEOUT: Duration = Duration::from_secs(5);
const RUNTIME_PREFIX: &str = "com.apple.CoreSimulator.SimRuntime.";
const TYPE_PREFIX: &str = "com.apple.CoreSimulator.SimDeviceType.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListError {
    NoXcode,
    Failed(String),
}

/// "com.apple.CoreSimulator.SimRuntime.iOS-27-2" -> "iOS 27.2".
fn runtime_os(key: &str) -> String {
    let rest = key.strip_prefix(RUNTIME_PREFIX).unwrap_or(key);
    let mut parts = rest.split('-');
    let platform = parts.next().unwrap_or(rest);
    let version: Vec<&str> = parts.collect();
    if version.is_empty() {
        platform.to_string()
    } else {
        format!("{platform} {}", version.join("."))
    }
}

/// The simulators in the default device set, as the picker groups them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DeviceList {
    pub booted: Vec<DeviceInfo>,
    pub shut_down: Vec<DeviceInfo>,
}

/// The available devices in `simctl list devices -j` output: booted ones and
/// shut-down ones, each sorted by name then OS. Devices on their way between
/// the two (Booting, Shutting Down, Creating) are in neither, nor is one
/// whose runtime is gone (`isAvailable: false`).
pub(crate) fn parse_devices(json: &str) -> Result<DeviceList, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let runtimes = root
        .get("devices")
        .and_then(Value::as_object)
        .ok_or_else(|| "no \"devices\" object in simctl output".to_string())?;
    let mut out = DeviceList::default();
    for (runtime, devices) in runtimes {
        let os = runtime_os(runtime);
        for d in devices.as_array().into_iter().flatten() {
            let field = |k: &str| d.get(k).and_then(Value::as_str);
            if d.get("isAvailable").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let group = match field("state") {
                Some("Booted") => &mut out.booted,
                Some("Shutdown") => &mut out.shut_down,
                _ => continue,
            };
            let (Some(udid), Some(name)) = (field("udid"), field("name")) else {
                continue;
            };
            let device_type = field("deviceTypeIdentifier")
                .map(|t| t.strip_prefix(TYPE_PREFIX).unwrap_or(t))
                .unwrap_or_default();
            group.push(DeviceInfo {
                udid: udid.to_string(),
                name: name.to_string(),
                os: os.clone(),
                device_type: device_type.to_string(),
            });
        }
    }
    for group in [&mut out.booted, &mut out.shut_down] {
        group.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.os.cmp(&b.os)));
    }
    Ok(out)
}

/// Every simulator in the default set, via `xcrun simctl list devices -j`.
pub(crate) fn list_devices() -> Result<DeviceList, ListError> {
    list_outcome(run_bounded_why(
        "xcrun",
        &["simctl", "list", "devices", "-j"],
        TIMEOUT,
    ))
}

/// What a `simctl list` run means for the tab.
fn list_outcome(ran: Result<Ran, RunFailed>) -> Result<DeviceList, ListError> {
    // A slow simctl (CoreSimulator starting, a busy Mac) is a failed list,
    // which keeps the view; only a missing xcrun means no Xcode.
    let ran = ran.map_err(|why| match why {
        RunFailed::Spawn => ListError::NoXcode,
        RunFailed::TimedOut => {
            ListError::Failed(format!("simctl list timed out after {} s", TIMEOUT.as_secs()))
        }
    })?;
    if ran.code != Some(0) {
        if ran.stderr.contains("unable to find utility") {
            return Err(ListError::NoXcode);
        }
        let stderr = ran.stderr.trim();
        return Err(ListError::Failed(match ran.code {
            _ if !stderr.is_empty() => stderr.to_string(),
            Some(code) => format!("simctl list failed with exit code {code}"),
            None => "simctl list was stopped by a signal".to_string(),
        }));
    }
    parse_devices(&ran.stdout).map_err(ListError::Failed)
}

/// A file of its own for each screenshot, `<dir>/<udid>-<pid>-<n>.jpg`: two
/// windows, a loop restarted while its last shot is still landing, or the
/// Dev and release apps (which share the temp dir) never write, read or
/// delete each other's.
fn shot_path(dir: &Path, udid: &str) -> PathBuf {
    shot_path_ext(dir, udid, "jpg")
}

/// [`shot_path`] with the file extension of the screenshot's format.
pub(crate) fn shot_path_ext(dir: &Path, udid: &str, ext: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{udid}-{}-{n}.{ext}", std::process::id()))
}

/// `xcrun simctl <args>` run to the end, whatever its exit code. Err only
/// when it could not run or ran past the timeout.
pub(crate) fn simctl_ran(args: &[&str]) -> Result<Ran, String> {
    simctl_ran_within(args, TIMEOUT)
}

/// [`simctl_ran`] with its own time limit.
fn simctl_ran_within(args: &[&str], timeout: Duration) -> Result<Ran, String> {
    let mut full = vec!["simctl"];
    full.extend_from_slice(args);
    run_bounded_why("xcrun", &full, timeout).map_err(|why| match why {
        RunFailed::Spawn => "xcrun simctl could not start".to_string(),
        RunFailed::TimedOut => format!("simctl timed out after {} s", timeout.as_secs()),
    })
}

/// `xcrun simctl <args>`: its stdout, or what stderr says (or a sentence
/// about the exit code) when it fails.
pub(crate) fn simctl(args: &[&str]) -> Result<String, String> {
    simctl_within(args, TIMEOUT)
}

/// [`simctl`] with its own time limit, for the slow ones (boot, shutdown).
pub(crate) fn simctl_within(args: &[&str], timeout: Duration) -> Result<String, String> {
    let ran = simctl_ran_within(args, timeout)?;
    if ran.code == Some(0) {
        return Ok(ran.stdout);
    }
    Err(failure_text(&ran))
}

/// What a failed simctl run says, trimmed; never empty. CoreSimulator's
/// errors open with a line that only says an error happened ("An error was
/// encountered processing the command (domain=…, code=405):"); it is dropped
/// so the first line is the reason ("Unable to boot device in current
/// state: Booted").
pub(crate) fn failure_text(ran: &Ran) -> String {
    let stderr = ran.stderr.trim();
    let stderr = match stderr.split_once('\n') {
        Some((head, rest))
            if head.starts_with("An error was encountered processing the command")
                && !rest.trim().is_empty() =>
        {
            rest.trim()
        }
        _ => stderr,
    };
    match ran.code {
        _ if !stderr.is_empty() => stderr.to_string(),
        Some(code) => format!("simctl failed with exit code {code}"),
        None => "simctl was stopped by a signal".to_string(),
    }
}

/// A JPEG screenshot of the device. simctl writes only to a file, so it goes
/// to a fresh file in `dir` ([`shot_path`]), is read back and removed.
pub(crate) fn screenshot_jpeg(udid: &str, dir: &Path) -> Result<Vec<u8>, String> {
    let path = shot_path(dir, udid);
    let path_str = path.to_string_lossy();
    let ran = run_bounded(
        "xcrun",
        &["simctl", "io", udid, "screenshot", "--type=jpeg", &path_str],
        TIMEOUT,
    )
    .ok_or_else(|| "simctl screenshot timed out or could not start".to_string())?;
    if ran.code != Some(0) {
        let _ = std::fs::remove_file(&path);
        return Err(ran.stderr.trim().to_string());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&path);
    bytes
}

#[cfg(test)]
mod tests {
    use super::{ListError, Ran, RunFailed, failure_text, list_outcome, shot_path};
    use std::path::Path;

    fn exited(code: i32, stderr: &str) -> Result<Ran, RunFailed> {
        Ok(Ran {
            code: Some(code),
            stdout: String::new(),
            stderr: stderr.into(),
        })
    }

    #[test]
    fn a_slow_simctl_is_a_failed_list_not_a_missing_xcode() {
        assert!(
            matches!(list_outcome(Err(RunFailed::TimedOut)), Err(ListError::Failed(e)) if e.contains("timed out")),
            "{:?}",
            list_outcome(Err(RunFailed::TimedOut))
        );
    }

    #[test]
    fn only_a_missing_xcrun_or_simctl_means_no_xcode() {
        assert_eq!(list_outcome(Err(RunFailed::Spawn)), Err(ListError::NoXcode));
        assert_eq!(
            list_outcome(exited(72, "xcrun: error: unable to find utility \"simctl\", not a developer tool or in PATH")),
            Err(ListError::NoXcode)
        );
        assert_eq!(
            list_outcome(exited(69, "You have not agreed to the Xcode license agreements.\n")),
            Err(ListError::Failed("You have not agreed to the Xcode license agreements.".into()))
        );
    }

    #[test]
    fn a_silent_simctl_failure_still_says_something() {
        assert!(
            matches!(list_outcome(exited(1, "  ")), Err(ListError::Failed(e)) if e.contains("exit code 1")),
            "{:?}",
            list_outcome(exited(1, ""))
        );
    }

    #[test]
    fn a_simctl_error_starts_with_its_reason() {
        // Captured from `simctl boot` on a booted iOS 27.2 device.
        let ran = Ran {
            code: Some(149),
            stdout: String::new(),
            stderr: "An error was encountered processing the command (domain=com.apple.CoreSimulator.SimError, code=405):\nUnable to boot device in current state: Booted\n".into(),
        };
        assert_eq!(failure_text(&ran), "Unable to boot device in current state: Booted");
        // Nothing after the header: keep it rather than say nothing.
        let ran = Ran {
            stderr: "An error was encountered processing the command (domain=x, code=1):\n".into(),
            ..ran
        };
        assert_eq!(
            failure_text(&ran),
            "An error was encountered processing the command (domain=x, code=1):"
        );
        let ran = Ran { stderr: "Invalid device: X\n".into(), ..ran };
        assert_eq!(failure_text(&ran), "Invalid device: X");
    }

    #[test]
    fn device_screenshots_never_share_a_file() {
        let dir = Path::new("/tmp/openorchestra-device");
        let a = shot_path(dir, "UDID");
        let b = shot_path(dir, "UDID");
        assert_ne!(
            a, b,
            "two calls in one process (two windows, or a stop and start)"
        );
        assert!(a.starts_with(dir));
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("UDID-"), "{name}");
        // Another process (the Dev and release apps share the temp dir).
        assert!(
            name.contains(&format!("-{}-", std::process::id())),
            "{name}"
        );
        assert!(name.ends_with(".jpg"), "{name}");
    }
}
