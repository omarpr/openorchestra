//! Finding running Android emulators: each one writes
//! `~/Library/Caches/TemporaryItems/avd/running/pid_<pid>.ini` (its qemu
//! pid) while it runs and removes it when it exits. One killed with
//! `kill -9` leaves its file behind until the next emulator boots, so a
//! file only counts while its pid is alive and still an emulator (the pid
//! may since have gone to another process).

use std::path::{Path, PathBuf};

use crate::devices::AndroidRunning;

/// The emulator described by a discovery file's text, the file being
/// `pid_<pid>.ini`. Lenient: unknown keys are ignored, but without
/// `avd.id`, `grpc.port` or `port.serial` it is no use (None).
pub(crate) fn parse_running_ini(text: &str, pid: u32) -> Option<AndroidRunning> {
    let mut fields = std::collections::HashMap::new();
    for line in text.lines() {
        // Only the first '=' splits: a base64 token ends in '=', and
        // `cmdline` has them too.
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.trim(), value.trim());
        }
    }
    let field = |key: &str| fields.get(key).copied().filter(|v| !v.is_empty());
    let avd_id = field("avd.id")?.to_string();
    Some(AndroidRunning {
        pid,
        name: field("avd.name").map_or_else(|| avd_id.replace('_', " "), str::to_string),
        grpc_port: field("grpc.port")?.parse().ok()?,
        token: field("grpc.token").map(str::to_string),
        serial_port: field("port.serial")?.parse().ok()?,
        avd_dir: field("avd.dir").map(PathBuf::from).unwrap_or_default(),
        avd_id,
    })
}

/// The pid in a discovery file's name (`pid_<pid>.ini`); None for any
/// other file, or pid 0 (`kill(0, 0)` would ask about our own group).
fn pid_of_file(name: &str) -> Option<u32> {
    let pid: u32 = name.strip_prefix("pid_")?.strip_suffix(".ini")?.parse().ok()?;
    (pid != 0).then_some(pid)
}

/// The emulators listed in `dir` whose pid `alive` says still runs, by
/// name. An AVD running more than once (`-read-only`) is listed once, as
/// its newest run (the highest pid): they share its id, and the picker's
/// rows need one each.
pub(crate) fn running_emulators_in(dir: &Path, alive: impl Fn(u32) -> bool) -> Vec<AndroidRunning> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<AndroidRunning> = entries
        .flatten()
        .filter_map(|entry| {
            let pid = pid_of_file(entry.file_name().to_str()?)?;
            if !alive(pid) {
                return None;
            }
            parse_running_ini(&std::fs::read_to_string(entry.path()).ok()?, pid)
        })
        .collect();
    // Each AVD's runs together, newest first: the first is kept.
    found.sort_by(|a, b| a.avd_id.cmp(&b.avd_id).then_with(|| b.pid.cmp(&a.pid)));
    found.dedup_by(|later, first| later.avd_id == first.avd_id);
    found.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.avd_id.cmp(&b.avd_id)));
    found
}

/// Whether process `pid` exists. EPERM (someone else's) counts as alive.
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 sends nothing; it only checks the pid exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether a live process named `name` is an emulator: its qemu (the pid
/// in the file), whose name starts `qemu-system`. A pid that was reused
/// after its emulator was killed is someone else. A name that can't be
/// read counts as the emulator.
fn is_emulator_process(name: Option<&str>) -> bool {
    name.is_none_or(|name| name.starts_with("qemu-system"))
}

/// Process `pid`'s name; None when it can't be read.
fn process_name(pid: u32) -> Option<String> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut buf = [0u8; 256];
    // SAFETY: proc_name writes at most `buf.len()` bytes into `buf`.
    let len = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let len = usize::try_from(len).ok().filter(|&n| n > 0 && n <= buf.len())?;
    Some(String::from_utf8_lossy(&buf[..len]).into_owned())
}

/// Whether `pid` still runs and is still the emulator that wrote its file.
pub(crate) fn still_emulator(pid: u32) -> bool {
    pid_alive(pid) && is_emulator_process(process_name(pid).as_deref())
}

/// Look up a particular process without deduplicating by AVD name.
pub(crate) fn running_emulator(pid: u32) -> Option<AndroidRunning> {
    let path = running_dir()?.join(format!("pid_{pid}.ini"));
    let dev = parse_running_ini(&std::fs::read_to_string(path).ok()?, pid)?;
    still_emulator(pid).then_some(dev)
}

/// A live qemu PID on its own cannot distinguish a reused PID.
pub(crate) fn is_same_run(dev: &AndroidRunning) -> bool {
    running_emulator(dev.pid).as_ref() == Some(dev)
}

/// How long ago the emulator `pid` listed itself (its file's age); None
/// when that can't be read.
pub(crate) fn listed_for(pid: u32) -> Option<std::time::Duration> {
    let file = running_dir()?.join(format!("pid_{pid}.ini"));
    let written = std::fs::metadata(file).and_then(|m| m.modified()).ok()?;
    std::time::SystemTime::now().duration_since(written).ok()
}

/// Where emulators list themselves while they run.
fn running_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(home.join("Library/Caches/TemporaryItems/avd/running"))
}

/// The running emulators, skipping files whose emulator has died. Reads a
/// directory: call it off the main thread.
pub(crate) fn running_emulators() -> Vec<AndroidRunning> {
    running_dir().map_or_else(Vec::new, |dir| running_emulators_in(&dir, still_emulator))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A discovery file with the field names the spike (2026-11-24) found,
    /// as `key=value` lines. The AVD and its ports are this machine's; the
    /// build, cmdline and paths are made up, and the token is `t`.
    const PIXEL: &str = "\
emulator.build=13400103
port.serial=5554
port.adb=5555
avd.name=Pixel 9 Pro API 35
avd.id=Pixel_9_Pro_API_35
avd.dir=/Users/u/.android/avd/Pixel_9_Pro_API_35.avd
cmdline=\"/Users/u/Library/Android/sdk/emulator/qemu/darwin-aarch64/qemu-system-aarch64-headless\" \"-netdelay\" \"none\" \"-avd\" \"Pixel_9_Pro_API_35\" \"-gpu=host\"
grpc.port=8554
grpc.token=t
grpc.allowlist=/Users/u/.android/emulator_access.json
grpc.jwks=/Users/u/Library/Caches/TemporaryItems/avd/running/123/jwks/x
grpc.jwk_active=/Users/u/Library/Caches/TemporaryItems/avd/running/123/jwks/x/active.jwk
emulator.version=35.2.10
";

    fn pixel(pid: u32) -> AndroidRunning {
        AndroidRunning {
            pid,
            avd_id: "Pixel_9_Pro_API_35".into(),
            name: "Pixel 9 Pro API 35".into(),
            grpc_port: 8554,
            token: Some("t".into()),
            serial_port: 5554,
            avd_dir: "/Users/u/.android/avd/Pixel_9_Pro_API_35.avd".into(),
        }
    }

    #[test]
    fn parses_a_running_emulator_ini() {
        assert_eq!(parse_running_ini(PIXEL, 123), Some(pixel(123)));
        // A base64 token ends in '='; only the first '=' splits a line.
        let padded = PIXEL.replace("grpc.token=t", "grpc.token=ab+c/d==");
        assert_eq!(parse_running_ini(&padded, 1).unwrap().token.as_deref(), Some("ab+c/d=="));
        // CRLF endings and spaces around '=' are fine.
        let crlf = PIXEL.replace('\n', "\r\n").replace("grpc.port=", "grpc.port = ");
        assert_eq!(parse_running_ini(&crlf, 123), Some(pixel(123)));
    }

    #[test]
    fn an_ini_without_what_connecting_needs_is_skipped() {
        let without = |key: &str| {
            PIXEL.lines().filter(|l| !l.starts_with(key)).collect::<Vec<_>>().join("\n")
        };
        assert_eq!(parse_running_ini(&without("grpc.port="), 1), None);
        assert_eq!(parse_running_ini(&without("avd.id="), 1), None);
        assert_eq!(parse_running_ini(&without("port.serial="), 1), None);
        assert_eq!(parse_running_ini(&PIXEL.replace("grpc.port=8554", "grpc.port=x"), 1), None);
        // No token or name: kept, the name made from the id.
        let bare = parse_running_ini(&without("grpc.token=").replace("avd.name=", "x="), 1).unwrap();
        assert_eq!(bare.token, None);
        assert_eq!(bare.name, "Pixel 9 Pro API 35");
        assert_eq!(parse_running_ini("", 1), None);
    }

    #[test]
    fn a_dead_pid_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
        write("pid_222.ini", PIXEL);
        // Left behind by an emulator killed with -9.
        write("pid_111.ini", &PIXEL.replace("Pixel_9_Pro_API_35", "Medium_Phone_API_35"));
        // Not discovery files.
        write("pid_0.ini", PIXEL);
        write("pid_x.ini", PIXEL);
        write("notes.txt", PIXEL);
        std::fs::create_dir(dir.path().join("222")).unwrap();
        let got = running_emulators_in(dir.path(), |pid| pid != 111);
        assert_eq!(got, vec![pixel(222)]);
        // Both alive: sorted by name.
        let got = running_emulators_in(dir.path(), |_| true);
        let ids: Vec<&str> = got.iter().map(|e| e.avd_id.as_str()).collect();
        assert_eq!(ids, ["Medium_Phone_API_35", "Pixel_9_Pro_API_35"]);
        // No directory (no emulator ever ran): none.
        assert!(running_emulators_in(&dir.path().join("missing"), |_| true).is_empty());
    }

    /// One AVD running twice (`-read-only`): both share its id, so only
    /// the newest (the higher pid) is listed, and ids stay unique. A dead
    /// one never hides the live one.
    #[test]
    fn an_avd_running_twice_is_listed_once() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
        let medium = PIXEL.replace("Pixel_9_Pro_API_35", "Medium_Phone_API_35");
        write("pid_300.ini", PIXEL);
        write("pid_222.ini", PIXEL);
        write("pid_401.ini", &PIXEL.replace("grpc.port=8554", "grpc.port=8556"));
        write("pid_111.ini", &medium);
        let got = running_emulators_in(dir.path(), |_| true);
        let rows: Vec<(&str, u32)> = got.iter().map(|e| (e.avd_id.as_str(), e.pid)).collect();
        assert_eq!(rows, [("Medium_Phone_API_35", 111), ("Pixel_9_Pro_API_35", 401)]);
        assert_eq!(got[1].grpc_port, 8556);
        // The newest gone: the one before it.
        let got = running_emulators_in(dir.path(), |pid| pid != 401);
        let rows: Vec<(&str, u32)> = got.iter().map(|e| (e.avd_id.as_str(), e.pid)).collect();
        assert_eq!(rows, [("Medium_Phone_API_35", 111), ("Pixel_9_Pro_API_35", 300)]);
    }

    #[test]
    fn a_reused_pid_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
        write("pid_222.ini", PIXEL);
        // Killed with -9, and its pid since taken by a shell.
        write("pid_111.ini", &PIXEL.replace("Pixel_9_Pro_API_35", "Medium_Phone_API_35"));
        let name = |pid: u32| if pid == 222 { "qemu-system-aarch64-headless" } else { "zsh" };
        let got = running_emulators_in(dir.path(), |pid| is_emulator_process(Some(name(pid))));
        assert_eq!(got, vec![pixel(222)]);
        // A name that can't be read (someone else's process): kept.
        assert!(is_emulator_process(None));
        assert!(is_emulator_process(Some("qemu-system-x86_64")));
        assert!(!is_emulator_process(Some("emulator")));
        // For real: this test process is alive but no emulator.
        let me = std::process::id();
        assert!(process_name(me).is_some_and(|n| !n.is_empty()), "{:?}", process_name(me));
        assert!(!still_emulator(me));
    }

    #[test]
    fn our_own_pid_is_alive_and_a_reaped_one_is_not() {
        assert!(pid_alive(std::process::id()));
        // pid 1 (launchd) isn't ours to signal: EPERM still means alive.
        assert!(pid_alive(1));
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!pid_alive(pid));
    }
}
