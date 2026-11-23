//! `xcrun simctl` wrappers and the parsing of its JSON.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;

use super::DeviceInfo;
use crate::ports::run_bounded;

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

/// The booted devices in `simctl list devices booted -j` output, sorted by
/// name then OS.
pub(crate) fn parse_booted(json: &str) -> Result<Vec<DeviceInfo>, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let runtimes = root
        .get("devices")
        .and_then(Value::as_object)
        .ok_or_else(|| "no \"devices\" object in simctl output".to_string())?;
    let mut out = Vec::new();
    for (runtime, devices) in runtimes {
        let os = runtime_os(runtime);
        for d in devices.as_array().into_iter().flatten() {
            let field = |k: &str| d.get(k).and_then(Value::as_str);
            if field("state") != Some("Booted") {
                continue;
            }
            let (Some(udid), Some(name)) = (field("udid"), field("name")) else {
                continue;
            };
            let device_type = field("deviceTypeIdentifier")
                .map(|t| t.strip_prefix(TYPE_PREFIX).unwrap_or(t))
                .unwrap_or_default();
            out.push(DeviceInfo {
                udid: udid.to_string(),
                name: name.to_string(),
                os: os.clone(),
                device_type: device_type.to_string(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.os.cmp(&b.os)));
    Ok(out)
}

/// Booted simulators, via `xcrun simctl list devices booted -j`.
pub(crate) fn list_booted() -> Result<Vec<DeviceInfo>, ListError> {
    let ran = run_bounded("xcrun", &["simctl", "list", "devices", "booted", "-j"], TIMEOUT)
        .ok_or(ListError::NoXcode)?;
    if ran.code != Some(0) {
        return Err(if ran.stderr.contains("unable to find utility") {
            ListError::NoXcode
        } else {
            ListError::Failed(ran.stderr.trim().to_string())
        });
    }
    parse_booted(&ran.stdout).map_err(ListError::Failed)
}

/// A file of its own for each screenshot, `<dir>/<udid>-<pid>-<n>.jpg`: two
/// windows, a loop restarted while its last shot is still landing, or the
/// Dev and release apps (which share the temp dir) never write, read or
/// delete each other's.
fn shot_path(dir: &Path, udid: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{udid}-{}-{n}.jpg", std::process::id()))
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
    use super::shot_path;
    use std::path::Path;

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
