//! `xcrun simctl` wrappers and the parsing of its JSON.

use std::path::Path;
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

/// A JPEG screenshot of the device. simctl writes only to a file, so it goes
/// to `<dir>/<udid>.jpg`, is read back and removed.
pub(crate) fn screenshot_jpeg(udid: &str, dir: &Path) -> Result<Vec<u8>, String> {
    let path = dir.join(format!("{udid}.jpg"));
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
