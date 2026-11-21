//! Per-user named pipes: one per session, framed messages over them.
//!
//! The pipe name is the session registry: `\\.\pipe\oo-session@<user>@<label>@<session>`
//! exists exactly while that session's host is alive, so listing sessions is
//! listing pipes. The pipe's DACL admits only this user (and SYSTEM), it
//! rejects remote clients, and the host claims the name with
//! `FILE_FLAG_FIRST_PIPE_INSTANCE` so nobody can squat on it.
//!
//! Handles are overlapped so one thread can block reading while another
//! writes: synchronous I/O on a single handle is serialized by Windows, which
//! would stall a client's keystrokes behind its pending read.

use std::io;
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_MORE_DATA,
    ERROR_NO_MORE_FILES, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FindClose, FindFirstFileW, FindNextFileW, ReadFile, WriteFile,
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

/// NUL-terminated UTF-16.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Keep a name component free of the `@` separator and of `\`, which pipe
/// names cannot contain.
fn component(s: &str) -> String {
    s.chars()
        .map(|c| if c == '@' || c == '\\' || c == '/' { '_' } else { c })
        .collect()
}

/// `oo-session@<user>@<label>@`: every pipe of this user's `label` server.
pub fn prefix(label: &str) -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    format!("oo-session@{}@{}@", component(&user), component(label))
}

/// The full pipe path for a session.
pub fn path(label: &str, session: &str) -> String {
    format!(r"\\.\pipe\{}{}", prefix(label), component(session))
}

/// Names of the live sessions of `label`, sorted.
pub fn list(label: &str) -> Vec<String> {
    let prefix = prefix(label);
    let mut out = Vec::new();
    unsafe {
        let mut data: WIN32_FIND_DATAW = std::mem::zeroed();
        let find = FindFirstFileW(wide(r"\\.\pipe\*").as_ptr(), &mut data);
        if find == INVALID_HANDLE_VALUE {
            return out;
        }
        loop {
            let name = from_wide(&data.cFileName);
            if let Some(session) = name.strip_prefix(&prefix) {
                if !session.is_empty() {
                    out.push(session.to_string());
                }
            }
            if FindNextFileW(find, &mut data) == 0 {
                let _ = GetLastError() == ERROR_NO_MORE_FILES;
                break;
            }
        }
        FindClose(find);
    }
    out.sort();
    out.dedup();
    out
}

/// This process's user SID as a string (`S-1-5-21-…`).
fn user_sid() -> Option<String> {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
        CloseHandle(token);
        if ok == 0 {
            return None;
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s: *mut u16 = null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
            return None;
        }
        let mut n = 0;
        while *s.add(n) != 0 {
            n += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
        LocalFree(s.cast());
        Some(sid)
    }
}

/// Security attributes granting this user and SYSTEM, nobody else.
pub struct Security {
    sd: PSECURITY_DESCRIPTOR,
    pub attrs: SECURITY_ATTRIBUTES,
}

impl Security {
    pub fn current_user() -> io::Result<Self> {
        let sddl = match user_sid() {
            Some(sid) => format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})"),
            None => "D:P(A;;GA;;;SY)(A;;GA;;;OW)".to_string(),
        };
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(&sddl).as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            sd,
            attrs: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd,
                bInheritHandle: 0,
            },
        })
    }
}

// The descriptor is immutable once built.
unsafe impl Send for Security {}
unsafe impl Sync for Security {}

impl Drop for Security {
    fn drop(&mut self) {
        unsafe { LocalFree(self.sd) };
    }
}

/// One end of a connected pipe.
pub struct Pipe {
    handle: HANDLE,
    write_lock: Mutex<()>,
}

// The handle is only used through overlapped calls, each with its own event.
unsafe impl Send for Pipe {}
unsafe impl Sync for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// An overlapped operation's result, waited for.
fn overlapped(
    handle: HANDLE,
    start: impl FnOnce(*mut OVERLAPPED) -> i32,
) -> io::Result<u32> {
    unsafe {
        let event = CreateEventW(null(), 1, 0, null());
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut ov: OVERLAPPED = std::mem::zeroed();
        ov.hEvent = event;
        let mut result = Ok(0);
        if start(&mut ov) == 0 {
            let err = GetLastError();
            if err != ERROR_IO_PENDING {
                result = Err(io::Error::from_raw_os_error(err as i32));
            }
        }
        if result.is_ok() {
            let mut n = 0u32;
            result = if GetOverlappedResult(handle, &ov, &mut n, 1) == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(n)
            };
        }
        CloseHandle(event);
        result
    }
}

fn is_eof(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error().map(|c| c as u32),
        Some(ERROR_BROKEN_PIPE) | Some(ERROR_PIPE_NOT_CONNECTED)
    )
}

impl Pipe {
    /// Create a listening instance of the pipe at `path`. `first` claims the
    /// name: it fails if any instance (ours or anyone's) already exists.
    pub fn listen(path: &str, first: bool, security: &Security) -> io::Result<Self> {
        let mut open = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if first {
            open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let handle = unsafe {
            CreateNamedPipeW(
                wide(path).as_ptr(),
                open,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                &security.attrs,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle,
            write_lock: Mutex::new(()),
        })
    }

    /// Wait for a client to connect to this instance.
    pub fn accept(&self) -> io::Result<()> {
        match overlapped(self.handle, |ov| unsafe { ConnectNamedPipe(self.handle, ov) }) {
            Ok(_) => Ok(()),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Connect to the session pipe at `path`, waiting up to `timeout` while
    /// every instance is busy.
    pub fn connect(path: &str, timeout: Duration) -> io::Result<Self> {
        let deadline = Instant::now() + timeout;
        let name = wide(path);
        loop {
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                return Ok(Self {
                    handle,
                    write_lock: Mutex::new(()),
                });
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) || Instant::now() >= deadline {
                return Err(err);
            }
            unsafe { WaitNamedPipeW(name.as_ptr(), 100) };
        }
    }

    /// Read some bytes; `Ok(0)` at end of stream (the other end closed).
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let ptr = buf.as_mut_ptr();
        match overlapped(self.handle, |ov| unsafe {
            ReadFile(self.handle, ptr, len, null_mut(), ov)
        }) {
            Ok(n) => Ok(n as usize),
            Err(e) if e.raw_os_error() == Some(ERROR_MORE_DATA as i32) => Ok(len as usize),
            Err(e) if is_eof(&e) => Ok(0),
            Err(e) => Err(e),
        }
    }

    fn read_exact(&self, mut buf: &mut [u8]) -> io::Result<bool> {
        let mut first = true;
        while !buf.is_empty() {
            let n = self.read(buf)?;
            if n == 0 {
                if first {
                    return Ok(false);
                }
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            first = false;
            buf = &mut buf[n..];
        }
        Ok(true)
    }

    fn write_all(&self, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            let len = buf.len().min(1 << 20) as u32;
            let ptr = buf.as_ptr();
            let n = overlapped(self.handle, |ov| unsafe {
                WriteFile(self.handle, ptr, len, null_mut(), ov)
            })?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            buf = &buf[n as usize..];
        }
        Ok(())
    }

    /// Send one message. Safe to call from several threads: each message
    /// goes out whole.
    pub fn send(&self, kind: u8, payload: &[u8]) -> io::Result<()> {
        let mut frame = Vec::with_capacity(5 + payload.len());
        frame.push(kind);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(payload);
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.write_all(&frame)
    }

    /// Receive one message; `None` once the other end has closed.
    pub fn recv(&self) -> io::Result<Option<(u8, Vec<u8>)>> {
        let mut header = [0u8; 5];
        if !self.read_exact(&mut header)? {
            return Ok(None);
        }
        let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if len > 64 << 20 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized message"));
        }
        let mut payload = vec![0u8; len];
        if len > 0 && !self.read_exact(&mut payload)? {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(Some((header[0], payload)))
    }
}
