//! Keep the computer from idle-sleeping while remote is on. macOS: a
//! `caffeinate -i -w <own pid>` child (it also exits by itself if the app
//! dies). Windows: an `ES_SYSTEM_REQUIRED` execution-state request
//! (`os_compat::power`), which Windows drops with the process. Dropping the
//! handle releases either.

use std::io;
#[cfg(not(windows))]
use std::process::{Child, Command, Stdio};

#[cfg(not(windows))]
pub struct KeepAwake {
    child: Child,
}

#[cfg(not(windows))]
impl KeepAwake {
    pub fn start() -> io::Result<KeepAwake> {
        let child = Command::new("/usr/bin/caffeinate")
            .arg("-i")
            .arg("-w")
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(KeepAwake { child })
    }

    /// The caffeinate process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

#[cfg(not(windows))]
impl Drop for KeepAwake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
pub struct KeepAwake {
    _request: os_compat::power::StayAwake,
}

#[cfg(windows)]
impl KeepAwake {
    pub fn start() -> io::Result<KeepAwake> {
        Ok(KeepAwake { _request: os_compat::power::StayAwake::start()? })
    }

    /// The process holding the request: this one.
    pub fn pid(&self) -> u32 {
        std::process::id()
    }
}

// `caffeinate` is macOS-only (the app is too); the Linux CI job has none.
#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only checks for existence.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[test]
    fn caffeinate_runs_until_dropped() {
        let k = KeepAwake::start().expect("caffeinate");
        let pid = k.pid();
        assert!(alive(pid));
        let out = Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let cmd = String::from_utf8_lossy(&out.stdout);
        assert!(
            cmd.contains(&format!("caffeinate -i -w {}", std::process::id())),
            "{cmd}"
        );
        drop(k);
        assert!(!alive(pid), "caffeinate still running after drop");
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_held_and_released() {
        // `powercfg /requests` needs admin; that the request is accepted
        // and released without hanging is what can be checked here.
        let k = KeepAwake::start().expect("SetThreadExecutionState");
        assert_eq!(k.pid(), std::process::id());
        drop(k);
    }
}
