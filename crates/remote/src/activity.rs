//! Who typed into which session from a device: the latest input per session
//! (the Mac shows it on the tab for a few seconds) and an audit log of it.
//! The log records who, where and when, never what was typed.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use os_compat::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// Input in one burst (no gap this long) is one audit line.
const BURST: Duration = Duration::from_secs(30);
/// Past this size the log is rotated to `input.log.1`.
const LOG_CAP: u64 = 1024 * 1024;

/// A device's latest input into a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteInput {
    pub session: String,
    pub device: String,
    pub at: SystemTime,
}

pub(crate) struct InputActivity {
    path: PathBuf,
    /// Latest input per session: (device name, when).
    latest: Mutex<HashMap<String, (String, SystemTime)>>,
    /// When each (device, session) burst was last written to the log.
    logged: Mutex<HashMap<(String, String), SystemTime>>,
}

impl InputActivity {
    pub(crate) fn new(state_dir: &Path) -> InputActivity {
        InputActivity {
            path: state_dir.join("remote").join("input.log"),
            latest: Mutex::new(HashMap::new()),
            logged: Mutex::new(HashMap::new()),
        }
    }

    /// `device` typed into `session` at `now`.
    pub(crate) fn record(&self, session: &str, device: &str, now: SystemTime) {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_string(), (device.to_string(), now));
        let key = (device.to_string(), session.to_string());
        let mut logged = self.logged.lock().unwrap_or_else(|e| e.into_inner());
        let new_burst = logged
            .get(&key)
            .is_none_or(|t| now.duration_since(*t).map_or(true, |gap| gap >= BURST));
        logged.insert(key, now);
        drop(logged);
        if new_burst {
            let line = format!("{}\t{device}\ttyped into {session}\n", rfc3339(now));
            if let Err(e) = append(&self.path, &line) {
                eprintln!("oo-remote: input log: {e}");
            }
        }
    }

    /// Input within `window` before `now`, by session.
    pub(crate) fn recent(&self, now: SystemTime, window: Duration) -> Vec<RemoteInput> {
        let latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<RemoteInput> = latest
            .iter()
            .filter(|(_, (_, at))| now.duration_since(*at).is_ok_and(|age| age < window))
            .map(|(session, (device, at))| RemoteInput {
                session: session.clone(),
                device: device.clone(),
                at: *at,
            })
            .collect();
        out.sort_by(|a, b| a.session.cmp(&b.session));
        out
    }
}

fn rfc3339(t: SystemTime) -> String {
    time::OffsetDateTime::from(t)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Append `line` (owner-only file), rotating once past [`LOG_CAP`].
fn append(path: &Path, line: &str) -> io::Result<()> {
    if fs::metadata(path).is_ok_and(|m| m.len() > LOG_CAP) {
        fs::rename(path, path.with_extension("log.1"))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    // Windows has no modes: an owner-only ACL (what the phone typed).
    os_compat::fs::restrict_to_user(path)?;
    file.write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use os_compat::fs::PermissionsExt;

    fn setup() -> (tempfile::TempDir, InputActivity) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("remote")).unwrap();
        let a = InputActivity::new(dir.path());
        (dir, a)
    }

    #[test]
    fn a_burst_is_one_private_log_line_without_the_text() {
        let (dir, a) = setup();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        a.record("s1", "iPhone", t0);
        a.record("s1", "iPhone", t0 + Duration::from_secs(5));
        a.record("s1", "iPhone", t0 + Duration::from_secs(60));
        a.record("s2", "iPhone", t0 + Duration::from_secs(61));
        let path = dir.path().join("remote/input.log");
        let log = fs::read_to_string(&path).unwrap();
        assert_eq!(log.lines().count(), 3, "{log}");
        assert!(log.lines().all(|l| l.contains("iPhone\ttyped into s")));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn recent_input_fades_after_the_window() {
        let (_dir, a) = setup();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        a.record("s1", "Pixel", t0);
        let w = Duration::from_secs(10);
        assert_eq!(a.recent(t0 + Duration::from_secs(3), w)[0].device, "Pixel");
        assert!(a.recent(t0 + w, w).is_empty());
    }

    #[test]
    fn the_log_rotates_past_its_cap() {
        let (dir, _a) = setup();
        let path = dir.path().join("remote/input.log");
        fs::write(&path, vec![b'x'; (LOG_CAP + 1) as usize]).unwrap();
        append(&path, "new\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        assert!(dir.path().join("remote/input.log.1").is_file());
    }
}
