//! The attach client: the terminal pane's process while a session is shown,
//! as `tmux attach` is on macOS. It puts its console in raw VT mode, then
//! relays keystrokes to the host and the host's output to the console, and
//! reports size changes. Closing the pane ends only this process; the
//! session keeps running in its host.

use std::ptr::null_mut;
use std::sync::Arc;
use std::time::Duration;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{
    GetConsoleCP, GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle,
    ReadConsoleW, SetConsoleCP, SetConsoleCtrlHandler, SetConsoleMode, SetConsoleOutputCP,
    CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, CTRL_BREAK_EVENT, CTRL_C_EVENT,
    DISABLE_NEWLINE_AUTO_RETURN, ENABLE_EXTENDED_FLAGS,
    ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

use super::msg;
use super::pipe::{self, Pipe};

const CP_UTF8: u32 = 65001;

/// This process's console in raw mode; the original modes come back on drop.
struct RawConsole {
    input: HANDLE,
    output: HANDLE,
    input_mode: Option<CONSOLE_MODE>,
    output_mode: Option<CONSOLE_MODE>,
    cps: (u32, u32),
}

impl RawConsole {
    fn enter() -> Self {
        unsafe {
            let input = GetStdHandle(STD_INPUT_HANDLE);
            let output = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode: CONSOLE_MODE = 0;
            let input_mode = (GetConsoleMode(input, &mut mode) != 0).then_some(mode);
            if input_mode.is_some() {
                // No line editing, echo or Ctrl+C processing: every key,
                // Ctrl+C included, goes to the session as the VT bytes the
                // terminal sent. EXTENDED_FLAGS without QUICK_EDIT keeps the
                // console from grabbing the mouse.
                SetConsoleMode(input, ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS);
            }
            let output_mode = (GetConsoleMode(output, &mut mode) != 0).then_some(mode);
            if let Some(m) = output_mode {
                SetConsoleMode(
                    output,
                    m | ENABLE_PROCESSED_OUTPUT
                        | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                        | DISABLE_NEWLINE_AUTO_RETURN,
                );
            }
            let cps = (GetConsoleCP(), GetConsoleOutputCP());
            SetConsoleCP(CP_UTF8);
            SetConsoleOutputCP(CP_UTF8);
            // Ctrl+C / Ctrl+Break are the session's, not ours.
            SetConsoleCtrlHandler(Some(keep_running), 1);
            Self {
                input,
                output,
                input_mode,
                output_mode,
                cps,
            }
        }
    }
}

/// Handles Ctrl+C and Ctrl+Break so neither ends the client. Ctrl+Break is
/// a signal whatever the input mode (raw mode only turns Ctrl+C into a
/// key), and its default handler would exit before `RawConsole` restores
/// the console's modes.
unsafe extern "system" fn keep_running(ctrl: u32) -> windows_sys::core::BOOL {
    (ctrl == CTRL_C_EVENT || ctrl == CTRL_BREAK_EVENT) as windows_sys::core::BOOL
}

impl Drop for RawConsole {
    fn drop(&mut self) {
        unsafe {
            if let Some(m) = self.input_mode {
                SetConsoleMode(self.input, m);
            }
            if let Some(m) = self.output_mode {
                SetConsoleMode(self.output, m);
            }
            if self.cps.0 != 0 {
                SetConsoleCP(self.cps.0);
            }
            if self.cps.1 != 0 {
                SetConsoleOutputCP(self.cps.1);
            }
        }
    }
}

/// The visible size of this process's console, `(cols, rows)`.
pub fn console_size() -> Option<(u16, u16)> {
    unsafe {
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) == 0 {
            return None;
        }
        let cols = (info.srWindow.Right - info.srWindow.Left + 1).max(1) as u16;
        let rows = (info.srWindow.Bottom - info.srWindow.Top + 1).max(1) as u16;
        Some((cols, rows))
    }
}

fn write_all(handle: HANDLE, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let mut n = 0u32;
        let ok = unsafe { WriteFile(handle, bytes.as_ptr(), bytes.len() as u32, &mut n, null_mut()) };
        if ok == 0 || n == 0 {
            return false;
        }
        bytes = &bytes[n as usize..];
    }
    true
}

/// Keystrokes → session. A console is read as UTF-16 (`ReadConsoleW`, which
/// in VT input mode yields the terminal's escape sequences) so no code page
/// gets in the way; anything else (a pipe) is relayed as bytes.
fn relay_input(conn: Arc<Pipe>, input: HANDLE, console: bool) {
    if console {
        let mut buf = [0u16; 4096];
        let mut pending: Option<u16> = None;
        loop {
            let mut n = 0u32;
            let ok = unsafe {
                ReadConsoleW(input, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n, null_mut())
            };
            if ok == 0 {
                return;
            }
            if n == 0 {
                continue;
            }
            let mut units: Vec<u16> = pending.take().into_iter().collect();
            units.extend_from_slice(&buf[..n as usize]);
            // A surrogate pair can straddle two reads.
            if matches!(units.last(), Some(u) if (0xD800..0xDC00).contains(u)) {
                pending = units.pop();
            }
            let text: String = char::decode_utf16(units)
                .map(|r| r.unwrap_or('\u{FFFD}'))
                .collect();
            if !text.is_empty() && conn.send(msg::INPUT, text.as_bytes()).is_err() {
                return;
            }
        }
    } else {
        let mut buf = [0u8; 4096];
        loop {
            let mut n = 0u32;
            let ok = unsafe { ReadFile(input, buf.as_mut_ptr(), buf.len() as u32, &mut n, null_mut()) };
            if ok == 0 || n == 0 {
                return;
            }
            if conn.send(msg::INPUT, &buf[..n as usize]).is_err() {
                return;
            }
        }
    }
}

/// Attach to session `name` until it ends or the connection drops. The exit
/// code is 0 either way, as `tmux attach` after a detach.
pub fn attach(label: &str, name: &str) -> i32 {
    let conn = match Pipe::connect(&pipe::path(label, name), Duration::from_secs(5)) {
        Ok(c) => Arc::new(c),
        Err(_) => {
            eprintln!("can't find session: {name}");
            return 1;
        }
    };
    let console = RawConsole::enter();
    let (cols, rows) = console_size().unwrap_or((80, 24));
    if conn.send(msg::ATTACH, &super::size_payload(cols, rows)).is_err() {
        return 1;
    }

    {
        let conn = conn.clone();
        let input = console.input as usize;
        let is_console = console.input_mode.is_some();
        std::thread::spawn(move || relay_input(conn, input as HANDLE, is_console));
    }
    // ConPTY resizes reach a console program only as a new buffer size, so
    // poll for it (cheap, and a resize is rarely urgent to the millisecond).
    {
        let conn = conn.clone();
        std::thread::spawn(move || {
            let mut last = (cols, rows);
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let Some(size) = console_size() else { continue };
                if size != last {
                    last = size;
                    if conn.send(msg::RESIZE, &super::size_payload(size.0, size.1)).is_err() {
                        return;
                    }
                }
            }
        });
    }

    loop {
        match conn.recv() {
            Ok(Some((msg::OUTPUT, bytes))) => {
                if !write_all(console.output, &bytes) {
                    break;
                }
            }
            Ok(Some((msg::EXIT, _))) => break,
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    drop(console);
    0
}
