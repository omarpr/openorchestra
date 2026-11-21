//! Windows stand-ins for the handful of `std::os::unix` APIs openOrchestra
//! uses. On Unix every item is a plain re-export of std (or a thin libc call),
//! so behaviour there is unchanged. On Windows, POSIX permission bits do not
//! exist: `mode()` setters are no-ops, `from_mode` only maps the owner-write
//! bit onto the read-only attribute, and `mode()` reports owner-only `0o600`.
//! Files under the user's profile are already private to that user by ACL.

pub mod fs {
    #[cfg(unix)]
    pub use std::os::unix::fs::{symlink, DirBuilderExt, OpenOptionsExt, PermissionsExt};

    #[cfg(not(unix))]
    pub use shim::*;

    #[cfg(not(unix))]
    mod shim {
        use std::fs::{DirBuilder, OpenOptions, Permissions};
        use std::io;
        use std::path::Path;

        pub trait OpenOptionsExt {
            fn mode(&mut self, mode: u32) -> &mut Self;
        }
        impl OpenOptionsExt for OpenOptions {
            fn mode(&mut self, _mode: u32) -> &mut Self {
                self
            }
        }

        pub trait DirBuilderExt {
            fn mode(&mut self, mode: u32) -> &mut Self;
        }
        impl DirBuilderExt for DirBuilder {
            fn mode(&mut self, _mode: u32) -> &mut Self {
                self
            }
        }

        pub trait PermissionsExt {
            fn mode(&self) -> u32;
            fn set_mode(&mut self, mode: u32);
            fn from_mode(mode: u32) -> Self;
        }
        impl PermissionsExt for Permissions {
            fn mode(&self) -> u32 {
                if self.readonly() {
                    0o400
                } else {
                    0o600
                }
            }
            fn set_mode(&mut self, mode: u32) {
                self.set_readonly(mode & 0o200 == 0);
            }
            fn from_mode(mode: u32) -> Self {
                // `Permissions` has no public constructor on Windows; borrow
                // one from a *file* that always exists and set its one bit.
                // Not a directory: its attributes carry FILE_ATTRIBUTE_DIRECTORY,
                // and applying that to a file fails with ERROR_INVALID_PARAMETER.
                let mut p = std::env::current_exe()
                    .and_then(std::fs::metadata)
                    .expect("current exe metadata")
                    .permissions();
                p.set_mode(mode);
                p
            }
        }

        /// Windows needs to know the target kind. Fails with Access Denied
        /// unless Developer Mode is on (or the process is elevated).
        pub fn symlink(original: impl AsRef<Path>, link: impl AsRef<Path>) -> io::Result<()> {
            let (original, link) = (original.as_ref(), link.as_ref());
            let target = if original.is_absolute() {
                original.to_path_buf()
            } else {
                link.parent().unwrap_or(Path::new(".")).join(original)
            };
            if target.is_dir() {
                std::os::windows::fs::symlink_dir(original, link)
            } else {
                std::os::windows::fs::symlink_file(original, link)
            }
        }
    }
}

pub mod ffi {
    #[cfg(unix)]
    pub use std::os::unix::ffi::OsStrExt;

    #[cfg(not(unix))]
    pub trait OsStrExt {
        fn as_bytes(&self) -> &[u8];
    }
    #[cfg(not(unix))]
    impl OsStrExt for std::ffi::OsStr {
        fn as_bytes(&self) -> &[u8] {
            self.as_encoded_bytes()
        }
    }
}

pub mod process {
    /// Whether a process with this pid exists (`kill(pid, 0)` on Unix).
    pub fn is_alive(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        #[cfg(unix)]
        {
            // SAFETY: signal 0 only checks for existence.
            unsafe { libc_kill(pid as i32, 0) == 0 }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
            use windows_sys::Win32::System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            };
            // SAFETY: plain Win32 calls; the handle is closed before return.
            unsafe {
                let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if h.is_null() {
                    return false;
                }
                let mut code = 0u32;
                let ok = GetExitCodeProcess(h, &mut code) != 0;
                CloseHandle(h);
                ok && code == STILL_ACTIVE as u32
            }
        }
    }

    /// Force-kill `pid` and, where the platform has them, its group
    /// (`killpg(SIGKILL)` on Unix, `taskkill /T /F` on Windows).
    pub fn kill_tree(pid: u32) {
        #[cfg(unix)]
        // SAFETY: SIGKILL to a process group we spawned.
        unsafe {
            libc_kill(-(pid as i32), 9);
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    #[cfg(unix)]
    extern "C" {
        #[link_name = "kill"]
        fn libc_kill(pid: i32, sig: i32) -> i32;
    }
}

/// Listening TCP sockets with their owning pid (what `lsof -iTCP
/// -sTCP:LISTEN` answers on Unix). Windows only: uses the IP Helper table,
/// which, unlike `netstat` output, is not localized.
#[cfg(windows)]
pub mod net {
    use std::net::{Ipv4Addr, Ipv6Addr};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID,
        MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

    /// `(pid, addr, port)`; the wildcard address is reported as `*`, as lsof does.
    pub fn tcp_listeners() -> Vec<(u32, String, u16)> {
        let mut out = Vec::new();
        if let Some(buf) = table(AF_INET as u32) {
            // SAFETY: the buffer holds a MIB_TCPTABLE_OWNER_PID written by the OS.
            let t = unsafe { &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID) };
            let rows = unsafe {
                std::slice::from_raw_parts(
                    t.table.as_ptr() as *const MIB_TCPROW_OWNER_PID,
                    t.dwNumEntries as usize,
                )
            };
            for r in rows {
                let ip = Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes());
                let addr = if ip.is_unspecified() { "*".into() } else { ip.to_string() };
                out.push((r.dwOwningPid, addr, port(r.dwLocalPort)));
            }
        }
        if let Some(buf) = table(AF_INET6 as u32) {
            // SAFETY: as above, for the IPv6 table.
            let t = unsafe { &*(buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID) };
            let rows = unsafe {
                std::slice::from_raw_parts(
                    t.table.as_ptr() as *const MIB_TCP6ROW_OWNER_PID,
                    t.dwNumEntries as usize,
                )
            };
            for r in rows {
                let ip = Ipv6Addr::from(r.ucLocalAddr);
                let addr = if ip.is_unspecified() { "*".into() } else { ip.to_string() };
                out.push((r.dwOwningPid, addr, port(r.dwLocalPort)));
            }
        }
        out
    }

    fn port(raw: u32) -> u16 {
        u16::from_be((raw & 0xffff) as u16)
    }

    /// The raw table, 8-byte aligned, retrying while it grows between calls.
    fn table(family: u32) -> Option<Vec<u64>> {
        let mut size = 0u32;
        for _ in 0..4 {
            let mut buf = vec![0u64; (size as usize).div_ceil(8).max(1)];
            // SAFETY: `buf` is at least `size` bytes; the OS updates `size`.
            let rc = unsafe {
                GetExtendedTcpTable(
                    buf.as_mut_ptr().cast(),
                    &mut size,
                    0,
                    family,
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                )
            };
            match rc {
                0 => return Some(buf),
                122 /* ERROR_INSUFFICIENT_BUFFER */ => continue,
                _ => return None,
            }
        }
        None
    }
}

/// Keep the system from idle-sleeping (Windows; macOS uses `caffeinate`).
#[cfg(windows)]
pub mod power {
    use std::io;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use windows_sys::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
    };

    /// Holds `ES_SYSTEM_REQUIRED` until dropped. The request belongs to the
    /// thread that makes it, so a thread of its own makes it and waits.
    pub struct StayAwake {
        stop: Option<mpsc::Sender<()>>,
        thread: Option<JoinHandle<()>>,
    }

    impl StayAwake {
        pub fn start() -> io::Result<StayAwake> {
            let (stop, stopped) = mpsc::channel::<()>();
            let (ready, started) = mpsc::channel::<bool>();
            let thread = std::thread::Builder::new()
                .name("stay-awake".into())
                .spawn(move || {
                    // SAFETY: plain flag calls on this thread's own state.
                    let ok = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) } != 0;
                    let _ = ready.send(ok);
                    if ok {
                        // Until the sender drops.
                        let _ = stopped.recv();
                        unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
                    }
                })?;
            if started.recv().unwrap_or(false) {
                Ok(StayAwake { stop: Some(stop), thread: Some(thread) })
            } else {
                let _ = thread.join();
                Err(io::Error::other("SetThreadExecutionState refused"))
            }
        }
    }

    impl Drop for StayAwake {
        fn drop(&mut self) {
            self.stop.take();
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }
}
