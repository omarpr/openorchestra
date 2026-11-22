//! The tmux-compatible command line: global flags (`-V`, `-L label`), the
//! hidden `__host` entry point, and the commands the app uses.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::GetExitCodeProcess;

use super::host::{self, HostArgs};
use super::pipe::{self, Pipe};
use super::procs::{self, Proc};
use super::{client, control, msg};
use crate::cmdline::{self, Command};
use crate::{format, keys};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const START_TIMEOUT: Duration = Duration::from_secs(10);

pub fn run(args: &[String]) -> i32 {
    let mut label = "default".to_string();
    let mut control = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-V" => {
                println!("oo-session {}", crate::VERSION);
                return 0;
            }
            "-L" | "-S" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("oo-session: {} needs a value", args[i]);
                    return 1;
                };
                label = value.clone();
                i += 2;
            }
            // Control mode: `attach-session` speaks tmux's `-C` protocol.
            "-C" | "-CC" => {
                control = true;
                i += 1;
            }
            // tmux client flags with no meaning here.
            "-2" | "-u" | "-l" | "-v" | "-N" => i += 1,
            _ => break,
        }
    }
    let rest = &args[i..];
    if rest.first().map(String::as_str) == Some("__host") {
        return host::run(parse_host(&label, &rest[1..]));
    }
    if rest.is_empty() {
        eprintln!("usage: oo-session [-L label] command [flags] [; command …]");
        return 1;
    }
    let mut ctx = Ctx::new(&label);
    ctx.control = control;
    for words in cmdline::split(rest) {
        let Some(cmd) = cmdline::parse(&words) else { continue };
        let code = exec(&ctx, &cmd);
        if code != 0 {
            return code;
        }
    }
    0
}

fn parse_host(label: &str, args: &[String]) -> HostArgs {
    let mut h = HostArgs {
        label: label.to_string(),
        cols: 80,
        rows: 24,
        ..Default::default()
    };
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "-s" => h.name = value,
            "-c" => h.cwd = Some(value),
            "-e" => h.env.push(value),
            "-x" => h.cols = value.parse().unwrap_or(80),
            "-y" => h.rows = value.parse().unwrap_or(24),
            "--" => {
                h.command = args[i + 1..].to_vec();
                break;
            }
            _ => {
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    h
}

/// What a session tells about itself (the host's `INFO_REPLY`).
#[derive(Default)]
struct Info {
    pid: u32,
    start: String,
    size: (u16, u16),
    /// `(x, y)`; `None` from a host that predates reporting it.
    cursor: Option<(u16, u16)>,
    clients: usize,
    /// Every process in the session's job.
    members: Vec<u32>,
    options: HashMap<String, String>,
}

fn parse_info(text: &str) -> Info {
    let mut info = Info::default();
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("pid"), Some(v), _) => info.pid = v.parse().unwrap_or(0),
            (Some("start"), Some(v), _) => info.start = v.to_string(),
            (Some("size"), Some(v), _) => {
                if let Some((c, r)) = v.split_once('x') {
                    info.size = (c.parse().unwrap_or(0), r.parse().unwrap_or(0));
                }
            }
            (Some("cursor"), Some(v), _) => {
                if let Some((x, y)) = v.split_once('x') {
                    info.cursor = x.parse().ok().zip(y.parse().ok());
                }
            }
            (Some("clients"), Some(v), _) => info.clients = v.parse().unwrap_or(0),
            (Some("procs"), Some(v), _) => {
                info.members = v.split(',').filter_map(|p| p.parse().ok()).collect()
            }
            (Some("opt"), Some(k), v) => {
                info.options.insert(k.to_string(), v.unwrap_or("").to_string());
            }
            _ => {}
        }
    }
    info
}

/// One request/response round trip to a session.
fn request(label: &str, session: &str, kind: u8, payload: &[u8]) -> io::Result<(u8, Vec<u8>)> {
    let conn = Pipe::connect(&pipe::path(label, session), REQUEST_TIMEOUT)?;
    conn.send(kind, payload)?;
    conn.recv()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "session closed"))
}

/// Lookups for one command sequence, cached: each session is asked once,
/// and the process list is taken once.
struct Ctx<'a> {
    label: &'a str,
    /// `-C`: `attach-session` runs a control-mode client.
    control: bool,
    /// `(pid, name)` of the session a control client is attached to, so its
    /// `%<pid>` targets resolve without asking every session.
    attached: Option<(u32, &'a str)>,
    infos: RefCell<HashMap<String, Option<std::rc::Rc<Info>>>>,
    procs: OnceCell<Vec<Proc>>,
}

impl<'a> Ctx<'a> {
    fn new(label: &'a str) -> Self {
        Self {
            label,
            control: false,
            attached: None,
            infos: RefCell::new(HashMap::new()),
            procs: OnceCell::new(),
        }
    }

    /// The session a tmux target names: `name`, `=name`, `name:` and
    /// `name:0.0` (one window, one pane), or the ids `%N` / `$N` that
    /// `#{pane_id}` / `#{session_id}` print. An id no session has is
    /// returned as is, so the command fails with "can't find session".
    fn resolve(&self, target: &str) -> String {
        let t = target.strip_prefix('=').unwrap_or(target);
        let t = t.split_once(':').map_or(t, |(session, _)| session);
        let id = t.strip_prefix('%').or_else(|| t.strip_prefix('$'));
        if let Some(pid) = id.and_then(|n| n.parse::<u32>().ok()) {
            if let Some((_, session)) = self.attached.filter(|&(attached, _)| attached == pid) {
                return session.to_string();
            }
            let found = self
                .sessions()
                .into_iter()
                .find(|s| self.info(s).is_some_and(|i| i.pid == pid));
            if let Some(session) = found {
                return session;
            }
        }
        t.to_string()
    }

    /// The command's `-t` target, resolved; an error when it has none.
    fn target(&self, cmd: &Command) -> Result<String, i32> {
        match cmd.value('t') {
            Some(t) => Ok(self.resolve(t)),
            None => {
                eprintln!("{}: no target session (-t)", cmd.name);
                Err(1)
            }
        }
    }

    fn info(&self, session: &str) -> Option<std::rc::Rc<Info>> {
        if let Some(cached) = self.infos.borrow().get(session) {
            return cached.clone();
        }
        let info = request(self.label, session, msg::INFO, &[])
            .ok()
            .filter(|(kind, _)| *kind == msg::INFO_REPLY)
            .map(|(_, text)| std::rc::Rc::new(parse_info(&String::from_utf8_lossy(&text))));
        self.infos
            .borrow_mut()
            .insert(session.to_string(), info.clone());
        info
    }

    fn forget(&self, session: &str) {
        self.infos.borrow_mut().remove(session);
    }

    fn var(&self, session: &str, name: &str) -> Option<String> {
        match name {
            "session_name" => return Some(session.to_string()),
            "pane_in_mode" | "pane_dead" | "window_index" | "pane_index" => {
                return Some("0".into())
            }
            _ => {}
        }
        let info = self.info(session)?;
        match name {
            "pane_pid" => Some(info.pid.to_string()),
            // One pane per session, so the program's pid (unique while it
            // runs) serves as both ids; `resolve` maps them back.
            "pane_id" => Some(format!("%{}", info.pid)),
            "session_id" => Some(format!("${}", info.pid)),
            // An older host does not say: the top-left corner.
            "cursor_x" => Some(info.cursor.map_or(0, |c| c.0).to_string()),
            "cursor_y" => Some(info.cursor.map_or(0, |c| c.1).to_string()),
            "pane_start_command" => Some(info.start.clone()),
            "pane_current_command" => {
                procs::foreground_in(self.procs.get_or_init(procs::snapshot), info.pid, &info.members)
            }
            // oo-session's own: every process the session started, comma-
            // separated. Windows parent chains break (an MSYS fork exits after
            // starting a native program), the job does not; Ports uses this to
            // tell which servers a session started.
            "pane_job_pids" => Some(
                info.members
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            "session_attached" => Some(info.clients.to_string()),
            "pane_width" | "window_width" | "client_width" => Some(info.size.0.to_string()),
            "pane_height" | "window_height" | "client_height" => Some(info.size.1.to_string()),
            option if option.starts_with('@') => info.options.get(option).cloned(),
            _ => None,
        }
    }

    fn expand(&self, session: &str, fmt: &str) -> String {
        format::expand(fmt, &|name| self.var(session, name))
    }

    fn sessions(&self) -> Vec<String> {
        pipe::list(self.label)
    }

    fn live(&self, session: &str) -> bool {
        self.sessions().iter().any(|s| s == session)
    }
}

/// Send `kind` to `session` and wait until the host has handled it, so a
/// following command (`send-keys Enter`) cannot overtake it.
fn deliver(label: &str, session: &str, kind: u8, payload: &[u8]) -> bool {
    let sent = Pipe::connect(&pipe::path(label, session), REQUEST_TIMEOUT).and_then(|c| {
        c.send(kind, payload)?;
        c.send(msg::PING, &[])?;
        c.recv()
    });
    matches!(sent, Ok(Some((msg::OK, _))))
}

fn exec(ctx: &Ctx, cmd: &Command) -> i32 {
    match cmd.name.as_str() {
        "list-sessions" => {
            let fmt = cmd.value('F').unwrap_or("#{session_name}");
            for s in ctx.sessions() {
                println!("{}", ctx.expand(&s, fmt));
            }
            0
        }
        "has-session" => match ctx.target(cmd) {
            Ok(t) if ctx.live(&t) => 0,
            Ok(t) => {
                eprintln!("can't find session: {t}");
                1
            }
            Err(code) => code,
        },
        "list-panes" => {
            let sessions = if cmd.has('a') {
                ctx.sessions()
            } else {
                match ctx.target(cmd) {
                    Ok(t) => vec![t],
                    Err(code) => return code,
                }
            };
            let fmt = cmd.value('F').unwrap_or("#{session_name}: #{pane_pid}");
            for s in sessions {
                // A session that ended between listing and asking is skipped,
                // as tmux would not list it.
                if ctx.info(&s).is_some() {
                    println!("{}", ctx.expand(&s, fmt));
                }
            }
            0
        }
        "display-message" => {
            if !cmd.has('p') {
                return 0;
            }
            let target = match ctx.target(cmd) {
                Ok(t) => t,
                Err(code) => return code,
            };
            if ctx.info(&target).is_none() {
                eprintln!("can't find session: {target}");
                return 1;
            }
            let fmt = cmd.args.join(" ");
            println!("{}", ctx.expand(&target, &fmt));
            0
        }
        "new-session" => new_session(ctx, cmd),
        "attach-session" => {
            let target = match cmd.value('t') {
                Some(t) => ctx.resolve(t),
                None => match ctx.sessions().into_iter().next() {
                    Some(s) => s,
                    None => {
                        eprintln!("no sessions");
                        return 1;
                    }
                },
            };
            if !ctx.control {
                return client::attach(ctx.label, &target);
            }
            // Control mode never sizes the session (as `-f ignore-size`).
            let Some(info) = ctx.info(&target) else {
                eprintln!("can't find session: {target}");
                return 1;
            };
            let label = ctx.label;
            let attached = Some((info.pid, target.as_str()));
            control::attach(label, &target, info.pid, info.size, &|cmd| {
                // Fresh per command (no stale cursor or job), but knowing
                // which session `%<pid>` most often names.
                let mut ctx = Ctx::new(label);
                ctx.attached = attached;
                exec(&ctx, cmd)
            })
        }
        "kill-session" => match ctx.target(cmd) {
            Ok(t) => kill(ctx, &t),
            Err(code) => code,
        },
        "kill-server" => {
            let sessions = ctx.sessions();
            for s in &sessions {
                kill(ctx, s);
            }
            // One that ended before its turn is fine; one still running is not.
            i32::from(sessions.iter().any(|s| ctx.live(s)))
        }
        "set-option" => {
            if cmd.has('g') || cmd.has('u') {
                return 0;
            }
            let target = match ctx.target(cmd) {
                Ok(t) => t,
                Err(code) => return code,
            };
            let (Some(name), value) = (cmd.args.first(), cmd.args.get(1)) else {
                eprintln!("set-option: no option name");
                return 1;
            };
            let payload = format!("{name}\0{}", value.map(String::as_str).unwrap_or(""));
            ctx.forget(&target);
            match request(ctx.label, &target, msg::SET_OPTION, payload.as_bytes()) {
                Ok((msg::OK, _)) => 0,
                _ => {
                    eprintln!("can't find session: {target}");
                    1
                }
            }
        }
        "send-keys" => {
            // Copy-mode commands (`-X cancel`): there is no copy mode here.
            if cmd.has('X') {
                return 0;
            }
            let target = match ctx.target(cmd) {
                Ok(t) => t,
                Err(code) => return code,
            };
            let bytes = if cmd.has('H') {
                match keys::hex_bytes(&cmd.args) {
                    Some(bytes) => bytes,
                    None => {
                        eprintln!("send-keys: invalid hex key");
                        return 1;
                    }
                }
            } else {
                keys::send_keys_bytes(&cmd.args, cmd.has('l'))
            };
            if deliver(ctx.label, &target, msg::INPUT, &bytes) {
                0
            } else {
                eprintln!("can't find session: {target}");
                1
            }
        }
        "resize-window" => {
            let target = match ctx.target(cmd) {
                Ok(t) => t,
                Err(code) => return code,
            };
            let Some(info) = ctx.info(&target) else {
                eprintln!("can't find session: {target}");
                return 1;
            };
            // `-A`/`-a` (fit to clients) and no size: nothing to do, as the
            // attached client sizes the session whenever its pane resizes.
            let size = |flag, current| match cmd.value(flag) {
                Some(v) => v.parse::<u16>().ok(),
                None => Some(current),
            };
            let (Some(cols), Some(rows)) = (size('x', info.size.0), size('y', info.size.1)) else {
                eprintln!("resize-window: bad size");
                return 1;
            };
            ctx.forget(&target);
            if deliver(ctx.label, &target, msg::RESIZE, &super::size_payload(cols, rows)) {
                0
            } else {
                eprintln!("can't find session: {target}");
                1
            }
        }
        "capture-pane" => {
            let target = match ctx.target(cmd) {
                Ok(t) => t,
                Err(code) => return code,
            };
            let escapes = u8::from(cmd.has('e'));
            match request(ctx.label, &target, msg::CAPTURE, &[escapes]) {
                Ok((msg::OUTPUT, text)) => {
                    use std::io::Write;
                    let mut out = std::io::stdout().lock();
                    let _ = out.write_all(&text);
                    if !text.ends_with(b"\n") {
                        let _ = out.write_all(b"\n");
                    }
                    0
                }
                _ => {
                    eprintln!("can't find session: {target}");
                    1
                }
            }
        }
        "if-shell" => {
            if !cmd.has('F') {
                eprintln!("if-shell: only -F conditions are supported");
                return 1;
            }
            let session = cmd.value('t').map(|t| ctx.resolve(t)).unwrap_or_default();
            let Some(cond) = cmd.args.first() else { return 1 };
            let branch = if format::truthy(&ctx.expand(&session, cond)) {
                cmd.args.get(1)
            } else {
                cmd.args.get(2)
            };
            match branch {
                Some(line) => {
                    let words: Vec<String> = line.split_whitespace().map(String::from).collect();
                    match cmdline::parse(&words) {
                        Some(inner) => exec(ctx, &inner),
                        None => 0,
                    }
                }
                None => 0,
            }
        }
        // Accepted and meaningless here.
        "set-environment" | "refresh-client" | "select-pane" | "select-window"
        | "detach-client" | "start-server" => 0,
        other => {
            eprintln!("oo-session: unknown command: {other}");
            1
        }
    }
}

/// End a session and wait (briefly) for it to be gone, so its name can be
/// reused right away.
fn kill(ctx: &Ctx, session: &str) -> i32 {
    if request(ctx.label, session, msg::KILL, &[]).is_err() && !ctx.live(session) {
        eprintln!("can't find session: {session}");
        return 1;
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while ctx.live(session) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    // A later command in this sequence must ask again, not reuse its INFO.
    ctx.forget(session);
    // The host answers before it terminates anything: still listed after
    // the wait means the kill failed.
    if ctx.live(session) {
        eprintln!("oo-session: session {session} is still running");
        return 1;
    }
    0
}

/// The next unused numeric session name, as tmux picks one.
fn free_name(ctx: &Ctx) -> String {
    let live = ctx.sessions();
    (0..)
        .map(|n: u32| n.to_string())
        .find(|n| !live.contains(n))
        .unwrap_or_default()
}

fn new_session(ctx: &Ctx, cmd: &Command) -> i32 {
    let name = cmd
        .value('s')
        .map(String::from)
        .unwrap_or_else(|| free_name(ctx));
    if ctx.live(&name) {
        if cmd.has('A') {
            return if cmd.has('d') { 0 } else { client::attach(ctx.label, &name) };
        }
        eprintln!("duplicate session: {name}");
        return 1;
    }
    let console = client::console_size();
    let cols = cmd
        .value('x')
        .and_then(|v| v.parse().ok())
        .or(console.map(|c| c.0))
        .unwrap_or(80u16);
    let rows = cmd
        .value('y')
        .and_then(|v| v.parse().ok())
        .or(console.map(|c| c.1))
        .unwrap_or(24u16);
    let cwd = cmd
        .value('c')
        .map(String::from)
        .or_else(|| std::env::current_dir().ok().map(|d| d.to_string_lossy().into_owned()));

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("oo-session: {e}");
            return 1;
        }
    };
    let mut argv: Vec<String> = vec![exe.to_string_lossy().into_owned()];
    argv.extend(["-L", ctx.label, "__host", "-s", &name].map(String::from));
    argv.extend(["-x".into(), cols.to_string(), "-y".into(), rows.to_string()]);
    if let Some(cwd) = &cwd {
        argv.extend(["-c".into(), cwd.clone()]);
    }
    for e in cmd.values('e') {
        argv.extend(["-e".into(), e.to_string()]);
    }
    argv.push("--".into());
    argv.extend(cmd.args.iter().cloned());
    // Detached (no console of the pane's), so closing the pane leaves the
    // session running.
    let process = match host::spawn_detached(&argv) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("oo-session: cannot start a session host: {e}");
            return 1;
        }
    };
    let exited = || {
        let mut code = 0u32;
        // STILL_ACTIVE (259) while it runs.
        (unsafe { GetExitCodeProcess(process, &mut code) } != 0 && code != 259).then_some(code)
    };

    // Ready once the host answers on its pipe (its program is running).
    let deadline = Instant::now() + START_TIMEOUT;
    let ready = loop {
        if matches!(request(ctx.label, &name, msg::PING, &[]), Ok((msg::OK, _))) {
            break true;
        }
        if let Some(status) = exited() {
            eprintln!(
                "oo-session: session {name} did not start (exit {status}); see {}",
                host::log_path(&name).display()
            );
            break false;
        }
        if Instant::now() >= deadline {
            eprintln!("oo-session: session {name} did not start in time");
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    unsafe { CloseHandle(process) };
    if !ready {
        return 1;
    }
    if cmd.has('d') {
        0
    } else {
        client::attach(ctx.label, &name)
    }
}

#[cfg(test)]
mod tests {
    use super::run;

    #[test]
    fn a_global_flag_without_its_value_is_a_usage_error() {
        for flag in ["-L", "-S"] {
            assert_eq!(run(&[flag.to_string()]), 1, "{flag}");
        }
    }
}
