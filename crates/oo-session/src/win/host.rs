//! The session host: a detached process that owns one ConPTY and the program
//! running in it, for as long as that program runs.
//!
//! It keeps a terminal model of the screen (`vt100`, with scrollback) fed by
//! everything the program prints, so an attaching client is first sent the
//! screen as it stands now (scrollback, then the visible rows, cursor and
//! input modes) and then the live output. It is the same thing tmux does on
//! attach, and it is what lets a pane that was closed, or an app that was
//! restarted, come back to an agent mid-conversation.
//!
//! The program runs in a job object that is killed when the host goes away,
//! so a crashed host never leaves an unreachable agent behind.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::msg;
use super::pipe::{self, wide, Pipe, Security};
use crate::quote;

/// Lines of scrollback kept for redrawing on attach.
const SCROLLBACK: usize = 3000;

/// What `__host` was asked to run.
#[derive(Debug, Default)]
pub struct HostArgs {
    pub label: String,
    pub name: String,
    pub cwd: Option<String>,
    /// `K=V` overrides on top of the inherited environment.
    pub env: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    /// Program and arguments; empty for the default shell.
    pub command: Vec<String>,
}

/// Append a line to the session's log (`%TEMP%\oo-session\<name>.log`).
/// The host has no console or stdio; this is where its errors go.
pub fn log(name: &str, line: &str) {
    use std::io::Write;
    let dir = std::env::temp_dir().join("oo-session");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{name}.log")))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// The program a session runs when none is given: the user's shell as a
/// login + interactive shell (tmux's `default-shell`). `SHELL` when it names
/// a Windows file (openOrchestra sets it to Git Bash), else Git Bash where Git
/// for Windows installs it, else `cmd.exe`.
pub fn default_command() -> Vec<String> {
    let from_env = std::env::var("SHELL")
        .ok()
        .filter(|s| std::path::Path::new(s).is_file());
    let git_bash = || {
        [
            std::env::var_os("ProgramFiles").map(|p| std::path::PathBuf::from(p).join(r"Git\bin\bash.exe")),
            std::env::var_os("LOCALAPPDATA")
                .map(|p| std::path::PathBuf::from(p).join(r"Programs\Git\bin\bash.exe")),
        ]
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
    };
    match from_env.or_else(git_bash) {
        Some(shell) => vec![shell, "-li".into()],
        None => vec![std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())],
    }
}

/// A sorted, double-NUL-terminated UTF-16 environment block: ours, with
/// `overrides` (`K=V`) applied. Names compare case-insensitively, as Windows
/// does.
fn env_block(overrides: &[String]) -> Vec<u16> {
    let mut vars: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (k, v) in std::env::vars_os() {
        let (k, v) = (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned());
        vars.insert(k.to_uppercase(), (k, v));
    }
    for kv in overrides {
        if let Some((k, v)) = kv.split_once('=') {
            vars.insert(k.to_uppercase(), (k.to_string(), v.to_string()));
        }
    }
    let mut block = Vec::new();
    for (_, (k, v)) in vars {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

type CreatePseudoConsoleFn =
    unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> windows_sys::core::HRESULT;
type ResizePseudoConsoleFn = unsafe extern "system" fn(HPCON, COORD) -> windows_sys::core::HRESULT;
type ClosePseudoConsoleFn = unsafe extern "system" fn(HPCON);

/// The pseudoconsole functions: Windows Terminal's `conpty.dll` (which runs
/// the `OpenConsole.exe` beside it) when it sits beside this exe, else the
/// one built into Windows. The app's own panes use the bundled one too (as
/// alacritty_terminal loads it), and the built-in one hands console-API
/// programs (Codex, PowerShell's line editor) arrow keys and mouse reports
/// as text, so both layers must be the bundled one to behave alike.
struct ConptyApi {
    create: CreatePseudoConsoleFn,
    resize: ResizePseudoConsoleFn,
    close: ClosePseudoConsoleFn,
}

fn conpty() -> &'static ConptyApi {
    static API: std::sync::OnceLock<ConptyApi> = std::sync::OnceLock::new();
    API.get_or_init(|| {
        load_bundled_conpty().unwrap_or(ConptyApi {
            create: CreatePseudoConsole,
            resize: ResizePseudoConsole,
            close: ClosePseudoConsole,
        })
    })
}

fn load_bundled_conpty() -> Option<ConptyApi> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    /// What `GetProcAddress` returns, unwrapped from its `FARPROC` option.
    type Proc = unsafe extern "system" fn() -> isize;
    let dll = std::env::current_exe().ok()?.parent()?.join("conpty.dll");
    if !dll.is_file() {
        return None;
    }
    unsafe {
        // A full path: never a conpty.dll from the cwd or PATH.
        let module = LoadLibraryW(wide(&dll.to_string_lossy()).as_ptr());
        if module.is_null() {
            return None;
        }
        let create = GetProcAddress(module, c"CreatePseudoConsole".as_ptr().cast())?;
        let resize = GetProcAddress(module, c"ResizePseudoConsole".as_ptr().cast())?;
        let close = GetProcAddress(module, c"ClosePseudoConsole".as_ptr().cast())?;
        Some(ConptyApi {
            create: std::mem::transmute::<Proc, CreatePseudoConsoleFn>(create),
            resize: std::mem::transmute::<Proc, ResizePseudoConsoleFn>(resize),
            close: std::mem::transmute::<Proc, ClosePseudoConsoleFn>(close),
        })
    }
}

struct Child {
    process: HANDLE,
    pid: u32,
    hpc: HPCON,
    input: HANDLE,
    output: HANDLE,
    job: HANDLE,
}

/// Start `argv` on a new pseudoconsole of `cols`×`rows`.
fn spawn(argv: &[String], cwd: Option<&str>, env: &[String], cols: u16, rows: u16) -> io::Result<Child> {
    unsafe {
        let (mut in_read, mut in_write, mut out_read, mut out_write): (HANDLE, HANDLE, HANDLE, HANDLE) =
            (null_mut(), null_mut(), null_mut(), null_mut());
        if CreatePipe(&mut in_read, &mut in_write, null(), 0) == 0
            || CreatePipe(&mut out_read, &mut out_write, null(), 0) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut hpc: HPCON = 0;
        let size = COORD { X: cols as i16, Y: rows as i16 };
        let hr = (conpty().create)(size, in_read, out_write, 0, &mut hpc);
        // The pseudoconsole holds its own duplicates.
        CloseHandle(in_read);
        CloseHandle(out_write);
        if hr < 0 {
            CloseHandle(in_write);
            CloseHandle(out_read);
            return Err(io::Error::from_raw_os_error(hr));
        }

        let mut attr_size = 0usize;
        InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut attr_size);
        let mut attr_buf = vec![0u64; attr_size.div_ceil(8)];
        let list = attr_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        let fail = |hpc: HPCON| {
            let e = io::Error::last_os_error();
            (conpty().close)(hpc);
            CloseHandle(in_write);
            CloseHandle(out_read);
            e
        };
        if InitializeProcThreadAttributeList(list, 1, 0, &mut attr_size) == 0 {
            return Err(fail(hpc));
        }
        if UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            hpc as *const c_void,
            std::mem::size_of::<HPCON>(),
            null_mut(),
            null(),
        ) == 0
        {
            DeleteProcThreadAttributeList(list);
            return Err(fail(hpc));
        }

        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = list;
        // Explicitly no stdio handles: otherwise the program can pick up
        // the host's own (NUL) instead of the pseudoconsole.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;

        let mut cmdline = wide(&quote::windows_command_line(argv));
        let env = env_block(env);
        let cwd = cwd.map(wide);
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            null(),
            cmdline.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            env.as_ptr().cast(),
            cwd.as_ref().map_or(null(), |c| c.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(list);
        if ok == 0 {
            return Err(fail(hpc));
        }

        // Kill-on-close job: the program and everything it starts end with
        // the host, never outliving the only way to reach them.
        let job = CreateJobObjectW(null(), null());
        if !job.is_null() {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            AssignProcessToJobObject(job, pi.hProcess);
        }
        ResumeThread(pi.hThread);
        CloseHandle(pi.hThread);
        Ok(Child {
            process: pi.hProcess,
            pid: pi.dwProcessId,
            hpc,
            input: in_write,
            output: out_read,
            job,
        })
    }
}

/// Start `argv` as a detached process that inherits **no** handles, and
/// return its process handle.
///
/// `std::process::Command` always lets the child inherit every inheritable
/// handle, including our stdout: a host started from a command whose output
/// someone reads to EOF (the app's `run_bounded`) would hold that pipe open
/// for the whole life of the session, and from a pane it would hold the
/// pane's console. So the host is started by hand.
pub fn spawn_detached(argv: &[String]) -> io::Result<HANDLE> {
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS, STARTUPINFOW,
    };
    let mut last = None;
    // Out of any job the pane's terminal put us in, if that job allows it.
    for breakaway in [CREATE_BREAKAWAY_FROM_JOB, 0] {
        let mut cmdline = wide(&quote::windows_command_line(argv));
        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            CreateProcessW(
                null(),
                cmdline.as_mut_ptr(),
                null(),
                null(),
                0,
                DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | breakaway,
                null(),
                null(),
                &si,
                &mut pi,
            )
        };
        if ok != 0 {
            unsafe { CloseHandle(pi.hThread) };
            return Ok(pi.hProcess);
        }
        last = Some(io::Error::last_os_error());
    }
    Err(last.unwrap_or_else(|| io::ErrorKind::Other.into()))
}

/// Every process in the session's job, in the order they joined it.
fn job_members(job: HANDLE) -> Vec<u32> {
    use windows_sys::Win32::System::JobObjects::{
        JobObjectBasicProcessIdList, QueryInformationJobObject,
    };
    if job.is_null() {
        return Vec::new();
    }
    // JOBOBJECT_BASIC_PROCESS_ID_LIST: two u32 counts, then ULONG_PTR ids.
    let mut buf = vec![0usize; 1 + 2048];
    let ok = unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicProcessIdList,
            buf.as_mut_ptr().cast(),
            (buf.len() * std::mem::size_of::<usize>()) as u32,
            null_mut(),
        )
    };
    if ok == 0 {
        return Vec::new();
    }
    let in_list = unsafe { *(buf.as_ptr().cast::<u32>().add(1)) } as usize;
    buf[1..1 + in_list.min(buf.len() - 1)]
        .iter()
        .map(|&pid| pid as u32)
        .collect()
}

type Outbox = mpsc::Sender<(u8, Vec<u8>)>;

struct Screen {
    parser: vt100::Parser,
    cols: u16,
    rows: u16,
    /// Attached clients' outboxes, by connection id.
    clients: Vec<(u64, Outbox)>,
}

struct Shared {
    screen: Mutex<Screen>,
    /// `None` once the session is closing.
    hpc: Mutex<Option<HPCON>>,
    input: Mutex<HANDLE>,
    job: HANDLE,
    pid: u32,
    start_command: String,
    options: Mutex<BTreeMap<String, String>>,
    next_id: AtomicU64,
}

// The raw handles are used under their mutexes or are immutable.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

/// Bytes that redraw `parser`'s terminal on a fresh one: scrollback first
/// (so the pane can scroll back through it), then the screen, cursor,
/// drawing attributes and input modes (mouse reporting, bracketed paste).
pub fn redraw(parser: &mut vt100::Parser) -> Vec<u8> {
    let mut out = b"\x1b[0m\x1b[H\x1b[2J".to_vec();
    let screen = parser.screen_mut();
    let (rows, cols) = screen.size();
    if screen.alternate_screen() {
        out.extend_from_slice(b"\x1b[?1049h");
    } else {
        screen.set_scrollback(usize::MAX);
        let total = screen.scrollback();
        if total > 0 {
            // `set_scrollback(k)` shows the k-th line back at the top.
            let mut k = total;
            while k > 0 {
                screen.set_scrollback(k);
                let take = k.min(rows as usize);
                for row in screen.rows_formatted(0, cols).take(take) {
                    out.extend_from_slice(&row);
                    out.extend_from_slice(b"\x1b[0m\r\n");
                }
                k -= take;
            }
            screen.set_scrollback(0);
            // Push the tail of the scrollback off the screen before the
            // screen itself is drawn over it.
            out.extend(std::iter::repeat_n(b'\n', rows as usize));
        }
    }
    out.extend(screen.contents_formatted());
    out.extend(screen.attributes_formatted());
    out.extend(screen.input_mode_formatted());
    out
}

impl Shared {
    fn write_input(&self, bytes: &[u8]) {
        let handle = self.input.lock().unwrap_or_else(|e| e.into_inner());
        let mut rest = bytes;
        while !rest.is_empty() {
            let mut n = 0u32;
            let ok = unsafe { WriteFile(*handle, rest.as_ptr(), rest.len() as u32, &mut n, null_mut()) };
            if ok == 0 || n == 0 {
                return;
            }
            rest = &rest[n as usize..];
        }
    }

    fn resize(&self, screen: &mut Screen, cols: u16, rows: u16) {
        if (cols, rows) == (screen.cols, screen.rows) {
            return;
        }
        if let Some(hpc) = *self.hpc.lock().unwrap_or_else(|e| e.into_inner()) {
            let hr = unsafe { (conpty().resize)(hpc, COORD { X: cols as i16, Y: rows as i16 }) };
            // Refused: keep the old size, so the model matches the console
            // and a later request for this size tries again.
            if hr < 0 {
                return;
            }
        }
        screen.parser.screen_mut().set_size(rows, cols);
        screen.cols = cols;
        screen.rows = rows;
    }

    /// `key\tvalue` lines: `pid`, `start`, `size`, `cursor`, `clients`, then one
    /// `opt\t<name>\t<value>` per option set.
    fn info(&self) -> Vec<u8> {
        let flat = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        let (cols, rows, clients, (cy, cx)) = {
            let s = self.screen.lock().unwrap_or_else(|e| e.into_inner());
            (s.cols, s.rows, s.clients.len(), s.parser.screen().cursor_position())
        };
        let members: Vec<String> = job_members(self.job).iter().map(u32::to_string).collect();
        let mut out = format!(
            "pid\t{}\nstart\t{}\nsize\t{cols}x{rows}\ncursor\t{cx}x{cy}\nclients\t{clients}\nprocs\t{}\n",
            self.pid,
            flat(&self.start_command),
            members.join(",")
        );
        for (k, v) in self.options.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            out.push_str(&format!("opt\t{}\t{}\n", flat(k), flat(v)));
        }
        out.into_bytes()
    }

    fn kill(&self) {
        unsafe { TerminateJobObject(self.job, 1) };
    }
}

/// One client connection, until it closes.
fn serve(conn: Arc<Pipe>, shared: Arc<Shared>) {
    let (tx, rx) = mpsc::channel::<(u8, Vec<u8>)>();
    let writer_conn = conn.clone();
    let writer = std::thread::spawn(move || {
        for (kind, payload) in rx {
            if writer_conn.send(kind, &payload).is_err() {
                break;
            }
        }
    });
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    while let Ok(Some((kind, payload))) = conn.recv() {
        match kind {
            msg::ATTACH => {
                let (cols, rows) = super::parse_size(&payload).unwrap_or((80, 24));
                let mut screen = shared.screen.lock().unwrap_or_else(|e| e.into_inner());
                // A watcher (`-C`, `ignore-size`) leaves the size alone.
                if payload.get(4) != Some(&msg::ATTACH_IGNORE_SIZE) {
                    shared.resize(&mut screen, cols, rows);
                }
                // Under the lock, so nothing printed in between is lost or
                // sent twice.
                let snapshot = redraw(&mut screen.parser);
                let _ = tx.send((msg::OUTPUT, snapshot));
                screen.clients.push((id, tx.clone()));
            }
            msg::INPUT => shared.write_input(&payload),
            msg::RESIZE => {
                if let Some((cols, rows)) = super::parse_size(&payload) {
                    let mut screen = shared.screen.lock().unwrap_or_else(|e| e.into_inner());
                    shared.resize(&mut screen, cols, rows);
                }
            }
            msg::INFO => {
                let _ = tx.send((msg::INFO_REPLY, shared.info()));
            }
            msg::SET_OPTION => {
                let text = String::from_utf8_lossy(&payload);
                let (k, v) = text.split_once('\0').unwrap_or((&text, ""));
                shared
                    .options
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(k.to_string(), v.to_string());
                let _ = tx.send((msg::OK, Vec::new()));
            }
            msg::KILL => {
                // Answered first and directly: the host exits right after.
                let _ = conn.send(msg::OK, &[]);
                shared.kill();
            }
            msg::PING => {
                let _ = tx.send((msg::OK, Vec::new()));
            }
            msg::CAPTURE => {
                let screen = shared.screen.lock().unwrap_or_else(|e| e.into_inner());
                let text = if payload.first() == Some(&1) {
                    screen.parser.screen().contents_formatted()
                } else {
                    screen.parser.screen().contents().into_bytes()
                };
                let _ = tx.send((msg::OUTPUT, text));
            }
            _ => {}
        }
    }
    shared
        .screen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clients
        .retain(|(i, _)| *i != id);
    drop(tx);
    let _ = writer.join();
}

/// Run a session host. Returns only on failure to start; otherwise the
/// process exits when the program does.
/// The next listening instance. A failure (pipe instances or memory
/// running out) is retried with backoff rather than ending the accept loop:
/// the program would keep running with no way to attach or control it.
fn listen_retrying(path: &str, security: &Security, name: &str) -> Pipe {
    let mut wait = Duration::from_millis(100);
    loop {
        match Pipe::listen(path, false, security) {
            Ok(p) => return p,
            Err(e) => {
                log(name, &format!("listen: {e}; retrying in {wait:?}"));
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_secs(5));
            }
        }
    }
}

pub fn run(args: HostArgs) -> i32 {
    let name = args.name.clone();
    let cwd = args
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok().map(|d| d.to_string_lossy().into_owned()));
    // The host must not hold a worktree's directory open (Windows would then
    // refuse to delete it); the program gets `cwd` explicitly.
    if let Some(root) = std::env::var_os("SystemRoot") {
        let _ = std::env::set_current_dir(root);
    }

    let security = match Security::current_user() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            log(&name, &format!("security descriptor: {e}"));
            return 3;
        }
    };
    let path = pipe::path(&args.label, &args.name);
    let first = match Pipe::listen(&path, true, &security) {
        Ok(p) => p,
        Err(e) => {
            log(&name, &format!("cannot claim {path}: {e}"));
            return 3;
        }
    };
    let argv = if args.command.is_empty() {
        default_command()
    } else {
        args.command.clone()
    };
    let (cols, rows) = (args.cols.clamp(2, 4000), args.rows.clamp(2, 2000));
    let child = match spawn(&argv, cwd.as_deref(), &args.env, cols, rows) {
        Ok(c) => c,
        Err(e) => {
            // The program only: arguments can carry secrets.
            let program = argv.first().map(String::as_str).unwrap_or("");
            log(&name, &format!("cannot start {program:?}: {e}"));
            return 4;
        }
    };

    let shared = Arc::new(Shared {
        screen: Mutex::new(Screen {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK),
            cols,
            rows,
            clients: Vec::new(),
        }),
        hpc: Mutex::new(Some(child.hpc)),
        input: Mutex::new(child.input),
        job: child.job,
        pid: child.pid,
        start_command: quote::start_command(&argv),
        options: Mutex::new(BTreeMap::new()),
        next_id: AtomicU64::new(1),
    });

    // Program output → screen model and attached clients.
    let reader = {
        let shared = shared.clone();
        let output = child.output as usize;
        std::thread::spawn(move || {
            let output = output as HANDLE;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let mut n = 0u32;
                let ok = unsafe { ReadFile(output, buf.as_mut_ptr(), buf.len() as u32, &mut n, null_mut()) };
                if ok == 0 || n == 0 {
                    break;
                }
                let bytes = &buf[..n as usize];
                let mut screen = shared.screen.lock().unwrap_or_else(|e| e.into_inner());
                screen.parser.process(bytes);
                screen
                    .clients
                    .retain(|(_, tx)| tx.send((msg::OUTPUT, bytes.to_vec())).is_ok());
            }
            unsafe { CloseHandle(output) };
        })
    };

    // Connections. A fresh instance is listening again as soon as one is
    // taken, so clients rarely see the pipe busy (they retry when they do).
    {
        let shared = shared.clone();
        let name = name.clone();
        std::thread::spawn(move || {
            let mut instance = first;
            loop {
                if let Err(e) = instance.accept() {
                    log(&name, &format!("accept: {e}"));
                    std::thread::sleep(Duration::from_millis(100));
                    instance = listen_retrying(&path, &security, &name);
                    continue;
                }
                let conn = Arc::new(instance);
                let shared = shared.clone();
                std::thread::spawn(move || serve(conn, shared));
                instance = listen_retrying(&path, &security, &name);
            }
        });
    }

    let mut code = 0u32;
    unsafe {
        WaitForSingleObject(child.process, INFINITE);
        GetExitCodeProcess(child.process, &mut code);
    }
    // The program is gone: close its console so the reader drains to EOF.
    // Take the handle in its own statement: in an `if let` (edition 2021)
    // the guard would live through the close, which can wait for that
    // drain, while a resize holding `screen` waits on it and the reader
    // waits on `screen`.
    let hpc = shared.hpc.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(hpc) = hpc {
        unsafe { (conpty().close)(hpc) };
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while !reader.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    {
        let mut screen = shared.screen.lock().unwrap_or_else(|e| e.into_inner());
        for (_, tx) in screen.clients.drain(..) {
            let _ = tx.send((msg::EXIT, code.to_le_bytes().to_vec()));
        }
    }
    // Let the writers deliver the exit before the pipes close with us.
    std::thread::sleep(Duration::from_millis(200));
    // Anything the program left running ends with the session (as tmux
    // hangs up on a pane's processes).
    shared.kill();
    std::process::exit(0);
}
