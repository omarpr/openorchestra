//! Windows stand-ins for the handful of `std::os::unix` APIs openOrchestra
//! uses. On Unix every item is a plain re-export of std (or a thin libc call),
//! so behaviour there is unchanged. On Windows, POSIX permission bits do not
//! exist: `mode()` setters are no-ops, `from_mode` only maps the owner-write
//! bit onto the read-only attribute, and `mode()` reports owner-only `0o600`.
//! Files under the user's profile are already private to that user by ACL.
//!
//! [`Os`] is the one switch for behaviour that differs by OS but compiles
//! everywhere (a path spelling, an executable suffix): code matches on
//! [`Os::CURRENT`] instead of scattering `cfg!(windows)`, so every OS's
//! answer sits side by side and each can be unit-tested on any machine.
//! `#[cfg]` stays for code that only compiles on one OS (the modules below).

/// The operating systems the app distinguishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    /// Linux and the other Unixes: the Mac's tools minus the Mac's APIs.
    Linux,
}

impl Os {
    /// The OS this build runs on.
    pub const CURRENT: Os = if cfg!(target_os = "macos") {
        Os::Mac
    } else if cfg!(windows) {
        Os::Windows
    } else {
        Os::Linux
    };

    /// Every OS, for tests that check each one's answers.
    pub const ALL: [Os; 3] = [Os::Mac, Os::Windows, Os::Linux];

    /// `path` as a POSIX shell reads it. Agents and their hooks run through
    /// Git Bash on Windows, which loses an unquoted `\` and misreads a mixed
    /// `C:\Users\…/a2a\x.sh`, but reads `C:/Users/…`; elsewhere a path is
    /// already that.
    pub fn shell_path(self, path: &std::path::Path) -> String {
        let shown = path.display().to_string();
        match self {
            Os::Windows => shown.replace('\\', "/"),
            Os::Mac | Os::Linux => shown,
        }
    }

    /// The file names `program` can have on `PATH`: Windows native installs
    /// are `name.exe`, and npm's extensionless sh launcher is what Git Bash
    /// runs.
    pub fn executable_names(self, program: &str) -> Vec<String> {
        match self {
            Os::Windows => vec![program.to_string(), format!("{program}.exe")],
            Os::Mac | Os::Linux => vec![program.to_string()],
        }
    }

    /// What can run an npm-installed `program` directly (not through a
    /// shell). On Windows npm installs an extensionless sh script, which only
    /// a shell runs, beside a `.cmd` launcher (or a native `.exe`) Windows
    /// runs itself; elsewhere the file itself.
    pub fn direct_launcher(self, program: &std::path::Path) -> std::path::PathBuf {
        if self == Os::Windows && program.extension().is_none() {
            // npm's wrapper for a native binary (OpenCode) ends in
            // `exec "$basedir/node_modules/…/x.exe" "$@"`: run that exe. A
            // `.cmd` is a worse answer — Node refuses to spawn one without a
            // shell (CVE-2024-27980).
            if let Some(exe) = npm_native_target(program) {
                return exe;
            }
            for ext in ["exe", "cmd"] {
                let sibling = program.with_extension(ext);
                if sibling.is_file() {
                    return sibling;
                }
            }
        }
        program.to_path_buf()
    }
}

/// The `.exe` an npm shell wrapper runs, if it runs one directly (not
/// through `node`).
fn npm_native_target(wrapper: &std::path::Path) -> Option<std::path::PathBuf> {
    // A wrapper is a few hundred bytes; anything big is the program itself.
    if std::fs::metadata(wrapper).ok()?.len() > 4096 {
        return None;
    }
    let text = std::fs::read_to_string(wrapper).ok()?;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("exec \"$basedir/"))?;
    let rest = line.strip_prefix("exec \"$basedir/")?;
    let rel = &rest[..rest.find('"')?];
    if !rel.to_ascii_lowercase().ends_with(".exe") {
        return None; // `exec node "$basedir/…js"`: not a native binary.
    }
    let exe = wrapper.parent()?.join(rel);
    exe.is_file().then_some(exe)
}

#[cfg(test)]
mod os_tests {
    use super::Os;
    use std::path::Path;

    #[test]
    fn shell_path_uses_forward_slashes_only_on_windows() {
        let p = Path::new(r"C:\Users\me\a2a\complete.sh");
        assert_eq!(Os::Windows.shell_path(p), "C:/Users/me/a2a/complete.sh");
        let unix = Path::new("/Users/me/a2a/complete.sh");
        for os in [Os::Mac, Os::Linux] {
            assert_eq!(os.shell_path(unix), "/Users/me/a2a/complete.sh");
        }
    }

    #[test]
    fn executable_names_add_exe_only_on_windows() {
        assert_eq!(Os::Windows.executable_names("claude"), ["claude", "claude.exe"]);
        assert_eq!(Os::Mac.executable_names("claude"), ["claude"]);
        assert_eq!(Os::Linux.executable_names("claude"), ["claude"]);
    }

    /// npm's wrappers, as `npm install -g` writes them on Windows.
    const NATIVE_WRAPPER: &str = "#!/bin/sh\nbasedir=$(dirname \"$(echo \"$0\" | sed -e 's,\\\\,/,g')\")\n\ncase `uname` in\n    *CYGWIN*|*MINGW*|*MSYS*) basedir=`cygpath -w \"$basedir\"`;;\nesac\n\nexec \"$basedir/node_modules/opencode-ai/bin/opencode.exe\"   \"$@\"\n";
    const NODE_WRAPPER: &str = "#!/bin/sh\nbasedir=$(dirname \"$0\")\nif [ -x \"$basedir/node\" ]; then\n  exec \"$basedir/node\"  \"$basedir/node_modules/@openai/codex/bin/codex.js\" \"$@\"\nelse\n  exec node  \"$basedir/node_modules/@openai/codex/bin/codex.js\" \"$@\"\nfi\n";

    #[test]
    fn an_npm_wrapper_of_a_native_binary_launches_the_binary() {
        let dir = tempdir();
        let exe = dir.join("node_modules/opencode-ai/bin/opencode.exe");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"MZ").unwrap();
        std::fs::write(dir.join("opencode"), NATIVE_WRAPPER).unwrap();
        std::fs::write(dir.join("opencode.cmd"), "@echo off").unwrap();
        assert_eq!(Os::Windows.direct_launcher(&dir.join("opencode")), exe);
        // Elsewhere the wrapper is left alone.
        assert_eq!(Os::Mac.direct_launcher(&dir.join("opencode")), dir.join("opencode"));
    }

    #[test]
    fn an_npm_wrapper_of_a_node_script_falls_back_to_its_cmd() {
        let dir = tempdir();
        std::fs::write(dir.join("codex"), NODE_WRAPPER).unwrap();
        std::fs::write(dir.join("codex.cmd"), "@echo off").unwrap();
        assert_eq!(Os::Windows.direct_launcher(&dir.join("codex")), dir.join("codex.cmd"));
    }

    fn tempdir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "os-compat-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

pub mod fs {
    #[cfg(unix)]
    pub use std::os::unix::fs::{symlink, DirBuilderExt, OpenOptionsExt, PermissionsExt};

    /// Let only this user (and SYSTEM) read or write the *file* at `path`:
    /// on Windows, what mode 0600 does on Unix, where this is a no-op (the
    /// mode does it). A Windows file otherwise inherits its folder's access,
    /// and a folder outside the user's profile (`HOME` pointed elsewhere)
    /// can let every local user read it. Files only: on a folder Windows
    /// would rewrite the access of everything inside.
    #[cfg(unix)]
    pub fn restrict_to_user(_path: &std::path::Path) -> std::io::Result<()> {
        Ok(())
    }

    #[cfg(windows)]
    pub fn restrict_to_user(path: &std::path::Path) -> std::io::Result<()> {
        use std::io;
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::{null, null_mut};
        use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
        use windows_sys::Win32::Security::Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::{
            GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACL,
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            TOKEN_QUERY, TOKEN_USER,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
            s.encode_wide().chain(Some(0)).collect()
        }
        // SAFETY: plain Win32 calls on buffers we own; every allocation the
        // system hands back is freed before return.
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
            CloseHandle(token);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut sid_text: *mut u16 = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid_text) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut n = 0;
            while *sid_text.add(n) != 0 {
                n += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_text, n));
            LocalFree(sid_text.cast());

            // Protected (no inherited entries), full access for SYSTEM and
            // this user, nothing for anyone else.
            let sddl = format!("D:P(A;;FA;;;SY)(A;;FA;;;{sid})");
            let mut sd: PSECURITY_DESCRIPTOR = null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(std::ffi::OsStr::new(&sddl)).as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let (mut present, mut defaulted) = (0, 0);
            let mut dacl: *mut ACL = null_mut();
            let got = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted);
            let rc = if got == 0 {
                Err(io::Error::last_os_error())
            } else {
                match SetNamedSecurityInfoW(
                    wide(path.as_os_str()).as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    dacl,
                    null(),
                ) {
                    0 => Ok(()),
                    err => Err(io::Error::from_raw_os_error(err as i32)),
                }
            };
            LocalFree(sd);
            rc
        }
    }

    #[cfg(all(test, windows))]
    #[test]
    fn a_restricted_file_stays_usable_by_its_user() {
        let path = std::env::temp_dir().join(format!("os-compat-restrict-{}", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        restrict_to_user(&path).unwrap();
        std::fs::write(&path, b"y").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"y");
        std::fs::remove_file(&path).unwrap();
    }

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
            use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE, WAIT_TIMEOUT};
            use windows_sys::Win32::System::Threading::{
                GetExitCodeProcess, OpenProcess, WaitForSingleObject,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            };
            // A process object is signaled once the process has exited. Its
            // exit code can't tell for sure: a process may exit with 259,
            // which is also STILL_ACTIVE. That is the fallback only for a
            // process we may query but not wait on.
            // SAFETY: plain Win32 calls; each handle is closed before return.
            unsafe {
                let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
                if !h.is_null() {
                    let running = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
                    CloseHandle(h);
                    return running;
                }
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
            let _ = no_window(&mut std::process::Command::new("taskkill"))
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    /// Start `command` without a console window of its own. The Windows app
    /// is a windowed program with no console, so every console program it
    /// runs (git, gh, curl, an agent's `--version`) would otherwise open a
    /// window for as long as it runs. A no-op elsewhere.
    pub fn no_window(command: &mut std::process::Command) -> &mut std::process::Command {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        command
    }

    /// [`no_window`]: `std::process::Command::new(program)` that never opens a
    /// console window.
    pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
        let mut command = std::process::Command::new(program);
        no_window(&mut command);
        command
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
        GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
        TCP_TABLE_OWNER_PID_LISTENER,
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
                    t.table.as_ptr(),
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
                    t.table.as_ptr(),
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

/// The environment the OS hands a process, re-read after start.
pub mod env {
    /// Add to this process's `PATH` the directories the system `PATH` has
    /// gained since the app started, and say whether it changed.
    ///
    /// On Windows an installer (winget, an .msi) writes the new directory to
    /// the registry; running processes keep the `PATH` they started with, so
    /// without this a Re-check after installing still reports "not found" and
    /// a new terminal cannot run what was just installed. Directories are only
    /// added, never removed or reordered. Elsewhere there is no system `PATH`
    /// to re-read: a no-op.
    #[cfg(not(windows))]
    pub fn refresh_path() -> bool {
        false
    }

    #[cfg(windows)]
    pub fn refresh_path() -> bool {
        use std::os::windows::ffi::OsStrExt;
        use std::path::PathBuf;
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs: Vec<PathBuf> = std::env::split_paths(&current).collect();
        let before = dirs.len();
        for system in registry::system_path() {
            for dir in std::env::split_paths(&system) {
                // A NUL would make set_var panic, so never let one through.
                let usable = !dir.as_os_str().is_empty()
                    && !dir.as_os_str().encode_wide().any(|c| c == 0);
                if usable && !dirs.iter().any(|d| same_dir(d, &dir)) {
                    dirs.push(dir);
                }
            }
        }
        if dirs.len() == before {
            return false;
        }
        match std::env::join_paths(dirs) {
            Ok(path) => {
                // Rust's own env access is locked; Windows' environment
                // block is safe to update from any thread.
                std::env::set_var("PATH", path);
                true
            }
            Err(_) => false,
        }
    }

    /// Windows paths compare case-insensitively, with or without a trailing
    /// separator.
    #[cfg(windows)]
    fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
        let norm = |p: &std::path::Path| {
            p.to_string_lossy()
                .trim_end_matches(['\\', '/'])
                .to_lowercase()
        };
        norm(a) == norm(b)
    }

    #[cfg(windows)]
    mod registry {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
        };

        /// The machine `PATH` then the user's, `%VARS%` expanded.
        pub fn system_path() -> Vec<OsString> {
            [
                (
                    HKEY_LOCAL_MACHINE,
                    r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
                ),
                (HKEY_CURRENT_USER, "Environment"),
            ]
            .into_iter()
            .filter_map(|(root, key)| read(root, key, "Path"))
            .collect()
        }

        fn wide(s: &str) -> Vec<u16> {
            std::ffi::OsStr::new(s).encode_wide().chain([0]).collect()
        }

        /// A string value; `REG_EXPAND_SZ` comes back expanded (RegGetValueW
        /// does that unless told not to, and reports it as `REG_SZ`).
        fn read(root: HKEY, key: &str, name: &str) -> Option<OsString> {
            let (key, name) = (wide(key), wide(name));
            let mut buf: Vec<u16> = vec![0; 2048];
            // The expanded size is only known after a try; grow and retry.
            for _ in 0..4 {
                let mut bytes = (buf.len() * 2) as u32;
                // SAFETY: valid NUL-terminated names; `bytes` is the buffer's size.
                let rc = unsafe {
                    RegGetValueW(
                        root,
                        key.as_ptr(),
                        name.as_ptr(),
                        RRF_RT_REG_SZ,
                        std::ptr::null_mut(),
                        buf.as_mut_ptr().cast(),
                        &mut bytes,
                    )
                };
                match rc {
                    ERROR_SUCCESS => {
                        // After expansion the reported size can exceed the
                        // string: it ends at the first NUL, and what follows
                        // is leftover buffer.
                        let filled = &buf[..(bytes as usize / 2).min(buf.len())];
                        let end = filled.iter().position(|&c| c == 0).unwrap_or(filled.len());
                        return Some(OsString::from_wide(&filled[..end]));
                    }
                    ERROR_MORE_DATA => buf.resize(bytes as usize / 2 + 256, 0),
                    _ => return None,
                }
            }
            None
        }

        #[cfg(test)]
        #[test]
        fn refreshing_keeps_path_settable() {
            // Would panic inside set_var on a NUL; then PATH must still split.
            super::refresh_path();
            let path = std::env::var_os("PATH").unwrap();
            assert!(std::env::split_paths(&path).count() > 0);
        }

        #[cfg(test)]
        #[test]
        fn reads_the_machine_path() {
            let path = system_path();
            let all: Vec<String> = path.iter().map(|p| p.to_string_lossy().into()).collect();
            assert!(
                all.iter().any(|p| p.to_lowercase().contains(r"\windows\system32")),
                "{all:?}"
            );
            assert!(all.iter().all(|p| !p.contains("%SystemRoot%")), "not expanded: {all:?}");
            // Expansion can report more bytes than the string holds; what
            // follows its NUL must not come along (it crashed set_var).
            assert!(all.iter().all(|p| !p.contains('\0')), "NUL inside: {all:?}");
        }
    }
}
