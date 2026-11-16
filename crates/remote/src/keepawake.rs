//! Keep the Mac from idle-sleeping while remote is on: a
//! `caffeinate -i -w <own pid>` child (it also exits by itself if the app
//! dies). Dropping the handle kills it.

use std::io;
use std::process::{Child, Command, Stdio};

pub struct KeepAwake {
    child: Child,
}

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

impl Drop for KeepAwake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
