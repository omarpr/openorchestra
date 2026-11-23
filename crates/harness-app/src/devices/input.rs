//! Input to the simulator the Device tab shows: touch, keys and the Home
//! button, through `oo-device-input`, a small Swift helper that drives
//! CoreDevice's remote HID (`helpers/device-input`; why a helper, and the
//! calls it makes: "Input spike findings (CoreDevice)" in
//! docs/superpowers/specs/2026-11-23-device-tab-controls-design.md).
//!
//! One [`InputHelper`] per watched device, long-lived: CoreDevice's sends
//! are queued, so a process that exits right after one loses it. The UI only
//! enqueues lines; a writer thread starts the process and feeds its stdin,
//! and reader threads drain its stdout and stderr, so nothing here blocks
//! the UI thread. The helper ends when the handle is dropped (stdin closes,
//! and it is killed if it hasn't gone a second later).
//!
//! The helper is built and bundled only on a Mac whose Xcode has the
//! frameworks (`script/build-device-input-helper.sh`); without it, or once it
//! fails, the picture is watch-only. Elsewhere than macOS it is never found.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use gpui::Keystroke;

/// The helper's file name, in the bundle's `Contents/Helpers` or next to a
/// dev build's binary.
const HELPER: &str = "oo-device-input";
/// The picture's tooltip while it can't take input.
pub(crate) const UNAVAILABLE_TIP: &str = "Simulator input isn't available with this Xcode";
/// Starts per device before input is left off for it.
pub(crate) const MAX_STARTS: u32 = 3;
/// The pause after a failed start before the next may begin.
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(2);
/// The most characters one paste types.
pub(crate) const MAX_PASTE: usize = 2000;
/// How long the bar's Home waits for the helper's answer before bringing
/// SpringBoard forward instead.
pub(crate) const HOME_WAIT: Duration = Duration::from_secs(5);
/// A Home line's deadline, from when it is sent: a helper that reads it
/// later (still starting, say) answers "late" and leaves it unpressed.
/// Well inside [`HOME_WAIT`], so a press always answers in time and Home
/// never happens twice.
pub(crate) const HOME_DEADLINE: Duration = Duration::from_secs(3);
/// How long a stopped helper gets to exit before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(1);
/// USB HID usages (keyboard page).
const LEFT_SHIFT: u16 = 0xE1;
const ESCAPE: u16 = 0x29;

/// A finger's phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TouchPhase {
    Down,
    Move,
    Up,
}

/// One command to the helper.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Line {
    /// x and y are 0…1 ratios of the portrait screen, top-left origin.
    Touch(TouchPhase, f64, f64),
    /// A USB HID keyboard usage, down or up.
    Key(u16, bool),
    /// Press Home, unless the helper reads this after `deadline_ms` (ms
    /// since the Unix epoch).
    Home { deadline_ms: u64 },
}

/// `at` in ms since the Unix epoch, the helper's clock for deadlines.
pub(crate) fn epoch_ms(at: std::time::SystemTime) -> u64 {
    at.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl Line {
    /// The command as the helper reads it, without its newline.
    pub(crate) fn encode(&self) -> String {
        // Never off the screen, and never "NaN".
        let ratio = |v: f64| if v.is_nan() { 0.5 } else { v.clamp(0.0, 1.0) };
        match self {
            Line::Touch(phase, x, y) => {
                let verb = match phase {
                    TouchPhase::Down => "down",
                    TouchPhase::Move => "move",
                    TouchPhase::Up => "up",
                };
                format!("{verb} {:.6} {:.6}", ratio(*x), ratio(*y))
            }
            Line::Key(usage, down) => format!("key {usage} {}", if *down { "down" } else { "up" }),
            Line::Home { deadline_ms } => format!("home {deadline_ms}"),
        }
    }

    /// Home, due within [`HOME_DEADLINE`] from now.
    pub(crate) fn home_now() -> Line {
        let deadline = std::time::SystemTime::now() + HOME_DEADLINE;
        Line::Home {
            deadline_ms: epoch_ms(deadline),
        }
    }

    fn is_move(&self) -> bool {
        matches!(self, Line::Touch(TouchPhase::Move, ..))
    }
}

/// Lines waiting to be written, with each run of moves cut to its last: a
/// helper that fell behind catches up with where the finger is. Downs, ups
/// and keys are all kept, in order.
pub(crate) fn coalesce(lines: Vec<Line>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    for line in lines {
        if line.is_move() && out.last().is_some_and(Line::is_move) {
            out.pop();
        }
        out.push(line);
    }
    out
}

/// A line the helper printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    /// The device and its HID were found.
    Ready,
    /// A command went (its verb).
    Ok(String),
    /// A command failed: "<verb>: <why>".
    Err(String),
}

pub(crate) fn parse_reply(line: &str) -> Option<Reply> {
    let line = line.trim_end();
    if line == "ready" {
        return Some(Reply::Ready);
    }
    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    match word {
        "ok" => Some(Reply::Ok(rest.to_string())),
        "err" => Some(Reply::Err(rest.to_string())),
        _ => None,
    }
}

/// What a helper tells the UI, with the serial it was started with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HelperEvent {
    Ready(u64),
    /// Its first command went: it works with this Xcode and device.
    Worked(u64),
    /// It exited, couldn't start, complained on stderr, or a command failed.
    Failed(u64, String),
}

/// The Home presses waiting for the helper's answer, oldest first.
type HomeWaiters = Arc<Mutex<VecDeque<Sender<Result<(), String>>>>>;

/// A running `oo-device-input` for one simulator.
pub(crate) struct InputHelper {
    lines: Sender<Line>,
    home_waiters: HomeWaiters,
}

impl InputHelper {
    /// Starts the helper for booted simulator `udid`, off the calling
    /// thread; what becomes of it arrives on `events`, tagged `serial`.
    /// Lines sent before it is ready wait in its stdin. Err only when there
    /// is no helper to start.
    pub(crate) fn spawn(
        udid: &str,
        serial: u64,
        events: UnboundedSender<HelperEvent>,
    ) -> Result<InputHelper, String> {
        let path = helper_path().ok_or_else(|| format!("no {HELPER} with this build"))?;
        let (lines, rx) = mpsc::channel();
        let home_waiters = HomeWaiters::default();
        let udid = udid.to_string();
        let waiters = home_waiters.clone();
        std::thread::Builder::new()
            .name("oo-device-input".into())
            .spawn(move || run(&path, &udid, serial, rx, events, waiters))
            .map_err(|e| format!("starting the input thread: {e}"))?;
        Ok(InputHelper {
            lines,
            home_waiters,
        })
    }

    fn send(&self, line: Line) {
        let _ = self.lines.send(line);
    }

    /// A finger at (x, y), 0…1 ratios of the portrait screen.
    pub(crate) fn touch(&self, phase: TouchPhase, x: f64, y: f64) {
        self.send(Line::Touch(phase, x, y));
    }

    pub(crate) fn key(&self, usage: u16, down: bool) {
        self.send(Line::Key(usage, down));
    }

    /// A key pressed and released, inside Shift when `shift`.
    pub(crate) fn press(&self, usage: u16, shift: bool) {
        if shift {
            self.key(LEFT_SHIFT, true);
        }
        self.key(usage, true);
        self.key(usage, false);
        if shift {
            self.key(LEFT_SHIFT, false);
        }
    }

    /// Types `text` key by key (a US keyboard; see [`text_keys`]).
    pub(crate) fn text(&self, text: &str) {
        for (usage, shift) in text_keys(text) {
            self.press(usage, shift);
        }
    }

    /// Presses Home, if the helper gets to it within [`HOME_DEADLINE`]. The
    /// receiver gets the helper's answer (an error when it was late), or
    /// hangs up if the helper ends first.
    pub(crate) fn home(&self) -> Receiver<Result<(), String>> {
        let (tx, rx) = mpsc::channel();
        self.home_waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(tx);
        self.send(Line::home_now());
        rx
    }
}

/// The writer thread: starts the helper, then writes what the UI sends
/// until the handle is dropped or the helper goes; then sees it ends.
fn run(
    path: &Path,
    udid: &str,
    serial: u64,
    lines: Receiver<Line>,
    events: UnboundedSender<HelperEvent>,
    home_waiters: HomeWaiters,
) {
    let fail = |why: String| {
        let _ = events.unbounded_send(HelperEvent::Failed(serial, why));
    };
    let mut child = match Command::new(path)
        .arg(udid)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return fail(format!("starting {}: {e}", path.display())),
    };
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        reap(child);
        return fail(format!("{HELPER} has no pipes"));
    };
    let replies = {
        let events = events.clone();
        std::thread::Builder::new()
            .name("oo-device-input-out".into())
            .spawn(move || read_replies(BufReader::new(stdout), serial, events, home_waiters))
    };
    let complaints = {
        let events = events.clone();
        std::thread::Builder::new()
            .name("oo-device-input-err".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::info!("device: {line}");
                    let _ = events.unbounded_send(HelperEvent::Failed(serial, line));
                }
            })
    };
    if replies.is_err() || complaints.is_err() {
        reap(child);
        return fail("starting the input reader threads failed".into());
    }
    write_lines(stdin, &lines);
    reap(child);
}

/// Writes each batch of waiting lines, coalesced, until the sender is
/// dropped or the helper's stdin closes. Dropping `stdin` at the end tells
/// the helper to finish.
fn write_lines(stdin: ChildStdin, lines: &Receiver<Line>) {
    let mut stdin = std::io::BufWriter::new(stdin);
    while let Ok(first) = lines.recv() {
        let mut batch = vec![first];
        batch.extend(lines.try_iter());
        let wrote = coalesce(batch)
            .iter()
            .try_for_each(|line| writeln!(stdin, "{}", line.encode()))
            .and_then(|()| stdin.flush());
        if wrote.is_err() {
            return;
        }
    }
}

/// The helper's stdout, until it closes.
fn read_replies(
    stdout: impl BufRead,
    serial: u64,
    events: UnboundedSender<HelperEvent>,
    home_waiters: HomeWaiters,
) {
    let answer_home = |answer: Result<(), String>| {
        let waiter = home_waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front();
        if let Some(waiter) = waiter {
            let _ = waiter.send(answer);
        }
    };
    let mut worked = false;
    for line in stdout.lines().map_while(Result::ok) {
        let reply = parse_reply(&line);
        if let Some(Reply::Ok(verb)) = &reply
            && verb != "home late"
            && !std::mem::replace(&mut worked, true)
        {
            let _ = events.unbounded_send(HelperEvent::Worked(serial));
        }
        match reply {
            Some(Reply::Ready) => {
                let _ = events.unbounded_send(HelperEvent::Ready(serial));
            }
            Some(Reply::Ok(verb)) if verb == "home" => answer_home(Ok(())),
            Some(Reply::Ok(verb)) if verb == "home late" => {
                answer_home(Err("the input helper got to Home too late".into()))
            }
            Some(Reply::Ok(_)) => {}
            Some(Reply::Err(why)) => {
                if why.starts_with("home:") {
                    answer_home(Err(why.clone()));
                }
                let _ = events.unbounded_send(HelperEvent::Failed(serial, why));
            }
            None => log::debug!("device: {HELPER} said {line:?}"),
        }
    }
    // Gone: nobody answers the Home presses still waiting.
    home_waiters.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let _ = events.unbounded_send(HelperEvent::Failed(serial, format!("{HELPER} stopped")));
}

/// Waits a moment for the helper to finish (its last sends leave in that
/// time), then kills it.
fn reap(mut child: Child) {
    let deadline = Instant::now() + EXIT_GRACE;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Where the helper is for the executable at `exe`, in the order to look:
/// a bundle's `Contents/Helpers`, else next to the binary (`cargo run`), or
/// one level up for a test binary in `target/debug/deps`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn helper_candidates(exe: &Path) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    if dir.file_name().is_some_and(|name| name == "MacOS") {
        return dir.parent().map(|c| c.join("Helpers").join(HELPER)).into_iter().collect();
    }
    let mut found = vec![dir.join(HELPER)];
    if dir.file_name().is_some_and(|name| name == "deps") {
        found.extend(dir.parent().map(|d| d.join(HELPER)));
    }
    found
}

/// The helper this build carries, if any. Looked for once.
#[cfg(target_os = "macos")]
pub(crate) fn helper_path() -> Option<PathBuf> {
    static PATH: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        let found = helper_candidates(&exe).into_iter().find(|p| p.is_file());
        match &found {
            Some(path) => log::info!("device: simulator input through {}", path.display()),
            None => log::info!("device: no {HELPER} in this build; simulators are watch-only"),
        }
        found
    })
    .clone()
}

/// iOS simulators need macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) fn helper_path() -> Option<PathBuf> {
    None
}

/// When a device's helper may be started again after failing.
#[derive(Clone, Debug, Default)]
pub(crate) struct Retry {
    failures: u32,
    last_failure: Option<Instant>,
}

impl Retry {
    pub(crate) fn may_start(&self, now: Instant) -> bool {
        !self.gave_up()
            && self
                .last_failure
                .is_none_or(|at| now.saturating_duration_since(at) >= RETRY_AFTER)
    }

    pub(crate) fn failed(&mut self, now: Instant) {
        self.failures += 1;
        self.last_failure = Some(now);
    }

    /// One answered a command: it works with this Xcode after all. (Coming
    /// up isn't enough: one that came up and failed each time would be
    /// restarted for ever.)
    pub(crate) fn worked(&mut self) {
        *self = Retry::default();
    }

    pub(crate) fn gave_up(&self) -> bool {
        self.failures >= MAX_STARTS
    }
}

/// What a key pressed while the picture has focus does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyAction {
    /// Press this USB HID usage on the device, inside Shift when `shift`.
    Press { usage: u16, shift: bool },
    /// ⌘V: type the Mac's clipboard.
    Paste,
    /// Esc: give the keyboard back to the app.
    Unfocus,
    /// Not the device's: an app shortcut (every other ⌘ chord), or a key
    /// with nothing to send.
    Pass,
}

/// A keystroke as gpui reports it, as the device should get it. Printable
/// keys go by the character they type (gpui reports shift-1 as "!" with
/// Shift cleared), on a US layout.
pub(crate) fn key_action(stroke: &Keystroke) -> KeyAction {
    let m = &stroke.modifiers;
    if m.platform {
        let paste = stroke.key == "v" && !m.shift && !m.alt && !m.control;
        return if paste { KeyAction::Paste } else { KeyAction::Pass };
    }
    if m.alt || m.control {
        return KeyAction::Pass;
    }
    let press = |usage, shift| KeyAction::Press { usage, shift };
    let named = match stroke.key.as_str() {
        "escape" if m.shift => return press(ESCAPE, false),
        "escape" => return KeyAction::Unfocus,
        "enter" => Some(0x28),
        "backspace" => Some(0x2A),
        "tab" => Some(0x2B),
        "space" => Some(0x2C),
        "home" => Some(0x4A),
        "pageup" => Some(0x4B),
        "delete" => Some(0x4C),
        "end" => Some(0x4D),
        "pagedown" => Some(0x4E),
        "right" => Some(0x4F),
        "left" => Some(0x50),
        "down" => Some(0x51),
        "up" => Some(0x52),
        _ => None,
    };
    if let Some(usage) = named {
        return press(usage, m.shift);
    }
    let typed = stroke.key_char.as_deref().unwrap_or(&stroke.key);
    let mut chars = typed.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => usage_for_char(c).map_or(KeyAction::Pass, |(u, s)| press(u, s || m.shift)),
        _ => KeyAction::Pass,
    }
}

/// The key (USB HID usage) that types `c` on a US keyboard, and whether
/// Shift is held for it.
pub(crate) fn usage_for_char(c: char) -> Option<(u16, bool)> {
    const SHIFTED_DIGITS: &str = "!@#$%^&*()";
    const PUNCTUATION: [(char, char, u16); 11] = [
        ('-', '_', 0x2D),
        ('=', '+', 0x2E),
        ('[', '{', 0x2F),
        (']', '}', 0x30),
        ('\\', '|', 0x31),
        (';', ':', 0x33),
        ('\'', '"', 0x34),
        ('`', '~', 0x35),
        (',', '<', 0x36),
        ('.', '>', 0x37),
        ('/', '?', 0x38),
    ];
    let from = |base: char, start: u16| start + (c as u32 - base as u32) as u16;
    match c {
        'a'..='z' => Some((from('a', 0x04), false)),
        'A'..='Z' => Some((from('A', 0x04), true)),
        '1'..='9' => Some((from('1', 0x1E), false)),
        '0' => Some((0x27, false)),
        ' ' => Some((0x2C, false)),
        '\n' => Some((0x28, false)),
        '\t' => Some((0x2B, false)),
        _ => {
            if let Some(i) = SHIFTED_DIGITS.chars().position(|d| d == c) {
                return Some((0x1E + i as u16, true));
            }
            PUNCTUATION.iter().find_map(|&(plain, shifted, usage)| {
                (c == plain).then_some((usage, false)).or((c == shifted).then_some((usage, true)))
            })
        }
    }
}

/// The key presses that type `text`: a Windows line end is one Return,
/// characters with no US key are skipped, and at most [`MAX_PASTE`].
pub(crate) fn text_keys(text: &str) -> Vec<(u16, bool)> {
    text.replace("\r\n", "\n")
        .chars()
        .filter_map(usage_for_char)
        .take(MAX_PASTE)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Keystroke, Modifiers};

    fn stroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    fn plain(key: &str, key_char: Option<&str>) -> Keystroke {
        stroke(key, key_char, Modifiers::default())
    }

    fn shifted(key: &str, key_char: Option<&str>) -> Keystroke {
        stroke(key, key_char, Modifiers { shift: true, ..Default::default() })
    }

    fn cmd(key: &str, shift: bool) -> Keystroke {
        stroke(key, None, Modifiers { platform: true, shift, ..Default::default() })
    }

    fn press(usage: u16, shift: bool) -> KeyAction {
        KeyAction::Press { usage, shift }
    }

    #[test]
    fn keys_map_to_hid_usages() {
        // Letters, from what they type: Shift is held for a capital.
        assert_eq!(key_action(&plain("a", Some("a"))), press(0x04, false));
        assert_eq!(key_action(&plain("z", Some("z"))), press(0x1D, false));
        assert_eq!(key_action(&shifted("a", Some("A"))), press(0x04, true));
        // Digits; gpui reports shift-1 as "!" with shift cleared.
        assert_eq!(key_action(&plain("1", Some("1"))), press(0x1E, false));
        assert_eq!(key_action(&plain("0", Some("0"))), press(0x27, false));
        assert_eq!(key_action(&plain("!", Some("!"))), press(0x1E, true));
        assert_eq!(key_action(&plain(")", Some(")"))), press(0x27, true));
        // Punctuation, plain and shifted.
        assert_eq!(key_action(&plain(".", Some("."))), press(0x37, false));
        assert_eq!(key_action(&plain("?", Some("?"))), press(0x38, true));
        assert_eq!(key_action(&plain("@", Some("@"))), press(0x1F, true));
        assert_eq!(key_action(&plain("-", Some("-"))), press(0x2D, false));
        assert_eq!(key_action(&plain("_", Some("_"))), press(0x2D, true));
        assert_eq!(key_action(&plain("'", Some("'"))), press(0x34, false));
        assert_eq!(key_action(&plain("\"", Some("\""))), press(0x34, true));
        // Named keys.
        assert_eq!(key_action(&plain("space", Some(" "))), press(0x2C, false));
        assert_eq!(key_action(&plain("enter", Some("\n"))), press(0x28, false));
        assert_eq!(key_action(&plain("backspace", None)), press(0x2A, false));
        assert_eq!(key_action(&plain("delete", None)), press(0x4C, false));
        assert_eq!(key_action(&plain("tab", Some("\t"))), press(0x2B, false));
        assert_eq!(key_action(&shifted("tab", None)), press(0x2B, true));
        assert_eq!(key_action(&plain("right", None)), press(0x4F, false));
        assert_eq!(key_action(&plain("left", None)), press(0x50, false));
        assert_eq!(key_action(&plain("down", None)), press(0x51, false));
        assert_eq!(key_action(&plain("up", None)), press(0x52, false));
        assert_eq!(key_action(&shifted("left", None)), press(0x50, true));
        // Esc gives focus back; Shift-Esc is the device's Esc.
        assert_eq!(key_action(&plain("escape", None)), KeyAction::Unfocus);
        assert_eq!(key_action(&shifted("escape", None)), press(0x29, false));
        // Nothing to send: left where it was.
        assert_eq!(key_action(&plain("é", Some("é"))), KeyAction::Pass);
        assert_eq!(key_action(&plain("f5", None)), KeyAction::Pass);
        let alt = Modifiers { alt: true, ..Default::default() };
        assert_eq!(key_action(&stroke("e", Some("´"), alt)), KeyAction::Pass);
        let ctrl = Modifiers { control: true, ..Default::default() };
        assert_eq!(key_action(&stroke("c", None, ctrl)), KeyAction::Pass);
    }

    #[test]
    fn typed_text_becomes_key_presses() {
        assert_eq!(
            text_keys("Hi 2!\n"),
            [(0x0B, true), (0x0C, false), (0x2C, false), (0x1F, false), (0x1E, true), (0x28, false)]
        );
        // A character with no key on a US keyboard is skipped, and a
        // Windows line end is one Return.
        assert_eq!(text_keys("aé\r\nb"), [(0x04, false), (0x28, false), (0x05, false)]);
        // A huge paste is cut short.
        assert_eq!(text_keys(&"a".repeat(MAX_PASTE + 50)).len(), MAX_PASTE);
    }

    #[test]
    fn cmd_chords_stay_with_the_app_except_paste() {
        assert_eq!(key_action(&cmd("v", false)), KeyAction::Paste);
        for (key, shift) in [("c", false), ("w", false), ("t", false), ("q", false), ("v", true), ("a", false)] {
            assert_eq!(key_action(&cmd(key, shift)), KeyAction::Pass, "cmd {key} shift {shift}");
        }
        // Every app-wide shortcut keeps working while the picture has focus,
        // including the ones with no ⌘ (the global Enter is bound, but goes
        // nowhere from the picture: it types Return there).
        for entry in crate::shortcuts::entries() {
            if entry.scope != crate::shortcuts::Scope::Global {
                continue;
            }
            for keys in entry.keys.split_whitespace() {
                let stroke = Keystroke::parse_with_os(keys, crate::shortcuts::host_os()).unwrap();
                if stroke.modifiers.platform {
                    assert_eq!(key_action(&stroke), KeyAction::Pass, "{keys}");
                }
            }
        }
    }

    #[test]
    fn helper_lines_are_well_formed() {
        assert_eq!(Line::Touch(TouchPhase::Down, 0.5, 0.25).encode(), "down 0.500000 0.250000");
        assert_eq!(Line::Touch(TouchPhase::Move, 1.0, 0.0).encode(), "move 1.000000 0.000000");
        assert_eq!(Line::Touch(TouchPhase::Up, 0.125, 0.875).encode(), "up 0.125000 0.875000");
        assert_eq!(Line::Key(0x04, true).encode(), "key 4 down");
        assert_eq!(Line::Key(0xE1, false).encode(), "key 225 up");
        // Off the screen or not a number: kept on it.
        assert_eq!(Line::Touch(TouchPhase::Move, -0.5, 1.5).encode(), "move 0.000000 1.000000");
        assert_eq!(Line::Touch(TouchPhase::Up, f64::NAN, 0.5).encode(), "up 0.500000 0.500000");
        // One line each, no matter what.
        for line in [Line::Touch(TouchPhase::Down, 1e300, -1e300), Line::Key(u16::MAX, false)] {
            let text = line.encode();
            assert!(!text.contains('\n') && !text.is_empty(), "{text:?}");
        }
        // And what the helper answers.
        assert_eq!(parse_reply("ready"), Some(Reply::Ready));
        assert_eq!(parse_reply("ok home"), Some(Reply::Ok("home".into())));
        assert_eq!(
            parse_reply("err key: no keyboard"),
            Some(Reply::Err("key: no keyboard".into()))
        );
        assert_eq!(parse_reply("ok"), Some(Reply::Ok(String::new())));
        assert_eq!(parse_reply("hello"), None);
    }

    #[test]
    fn moves_between_a_down_and_its_up_are_coalesced() {
        use TouchPhase::*;
        let t = |p, x| Line::Touch(p, x, 0.5);
        let sent = coalesce(vec![
            t(Down, 0.1),
            t(Move, 0.2),
            t(Move, 0.3),
            t(Move, 0.4),
            t(Up, 0.4),
            t(Down, 0.5),
            t(Move, 0.6),
            Line::Key(4, true),
            t(Move, 0.7),
            t(Move, 0.8),
        ]);
        assert_eq!(
            sent,
            vec![
                t(Down, 0.1),
                t(Move, 0.4),
                t(Up, 0.4),
                t(Down, 0.5),
                t(Move, 0.6),
                Line::Key(4, true),
                t(Move, 0.8),
            ]
        );
    }

    #[test]
    fn a_failing_helper_is_retried_a_few_times_then_left() {
        let start = std::time::Instant::now();
        let later = |ms| start + std::time::Duration::from_millis(ms);
        let mut retry = Retry::default();
        assert!(retry.may_start(start));
        retry.failed(start);
        // Not straight away, then again.
        assert!(!retry.may_start(later(100)));
        assert!(retry.may_start(later(RETRY_AFTER.as_millis() as u64)));
        for n in 2..=MAX_STARTS {
            retry.failed(later(10_000 * n as u64));
        }
        assert!(retry.gave_up());
        assert!(!retry.may_start(later(1_000_000)));
        // One that answered a command counts as working again.
        retry.worked();
        assert!(!retry.gave_up());
        assert!(retry.may_start(later(1_000_000)));
    }

    #[test]
    fn a_late_home_is_left_unpressed() {
        // The helper presses Home only before the line's deadline (ms since
        // the epoch), and that falls well inside the bar's wait: a Home
        // given up on (SpringBoard brought forward instead) never presses
        // later, when a slow helper comes up.
        assert_eq!(Line::Home { deadline_ms: 1234 }.encode(), "home 1234");
        assert!(HOME_DEADLINE + Duration::from_secs(1) <= HOME_WAIT);
        let before = epoch_ms(std::time::SystemTime::now());
        let Line::Home { deadline_ms } = Line::home_now() else {
            panic!();
        };
        let ahead = deadline_ms - before;
        assert!(ahead >= HOME_DEADLINE.as_millis() as u64 && ahead < HOME_DEADLINE.as_millis() as u64 + 1000);
    }

    #[test]
    fn the_replies_say_ready_working_failed_and_answer_home() {
        let (events, mut rx) = futures::channel::mpsc::unbounded();
        let waiters = HomeWaiters::default();
        let (on_time, late) = (mpsc::channel(), mpsc::channel());
        waiters.lock().unwrap().extend([on_time.0, late.0]);
        let out = "ready\nok down\nok up\nok home\nok home late\nerr key: no keyboard\n";
        read_replies(std::io::Cursor::new(out), 3, events, waiters.clone());
        assert_eq!(on_time.1.try_recv(), Ok(Ok(())));
        assert!(late.1.try_recv().unwrap().is_err());
        let mut got = Vec::new();
        while let Ok(event) = rx.try_recv() {
            got.push(event);
        }
        assert_eq!(
            got,
            [
                HelperEvent::Ready(3),
                // Once, for the first command that went.
                HelperEvent::Worked(3),
                HelperEvent::Failed(3, "key: no keyboard".into()),
                HelperEvent::Failed(3, "oo-device-input stopped".into()),
            ]
        );
    }

    #[test]
    fn the_helper_is_found_in_the_bundle_or_next_to_the_binary() {
        use std::path::{Path, PathBuf};
        assert_eq!(
            helper_candidates(Path::new("/Applications/openOrchestra.app/Contents/MacOS/openOrchestra")),
            [PathBuf::from("/Applications/openOrchestra.app/Contents/Helpers/oo-device-input")]
        );
        assert_eq!(
            helper_candidates(Path::new("/r/target/debug/harness-app")),
            [PathBuf::from("/r/target/debug/oo-device-input")]
        );
        // A test binary, one level down.
        assert_eq!(
            helper_candidates(Path::new("/r/target/debug/deps/harness_app-1234")),
            [
                PathBuf::from("/r/target/debug/deps/oo-device-input"),
                PathBuf::from("/r/target/debug/oo-device-input"),
            ]
        );
    }

    /// A screenshot of `udid`, saved as `<dir>/<name>.png`, as a coarse luma
    /// grid.
    #[cfg(target_os = "macos")]
    fn shot(udid: &str, dir: &Path, name: &str) -> Vec<f64> {
        let path = dir.join(format!("{name}.png"));
        let path_str = path.to_string_lossy();
        crate::devices::simctl::simctl(&["io", udid, "screenshot", "--type=png", &path_str]).unwrap();
        let image = image::open(&path).unwrap().to_luma8();
        let (w, h) = (image.width(), image.height());
        let mut grid = Vec::new();
        for gy in 0..48 {
            for gx in 0..24 {
                let (x, y) = ((2 * gx + 1) * w / 48, (2 * gy + 1) * h / 96);
                grid.push(f64::from(image.get_pixel(x, y)[0]));
            }
        }
        grid
    }

    #[cfg(target_os = "macos")]
    fn differ(a: &[f64], b: &[f64]) -> f64 {
        a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f64>() / a.len() as f64
    }

    /// Manual, on a simulator created and booted for the purpose (never
    /// anyone else's), with the helper built next to the test binary
    /// (`bash script/build-device-input-helper.sh
    /// crates/harness-app/target/debug/oo-device-input`):
    /// `OO_TEST_SIM_UDID=<udid> cargo test helper_taps_home_and_types --
    /// --ignored --nocapture`. Presses Home, taps Settings → General, swipes,
    /// and types a URL into Safari's address bar; each step must change the
    /// screen, and the screenshots are kept (`OO_TEST_SHOTS`, else a temp
    /// dir) to check by eye.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn helper_taps_home_and_types() {
        use std::thread::sleep;
        let udid = std::env::var("OO_TEST_SIM_UDID").expect("set OO_TEST_SIM_UDID");
        let dir = std::env::var_os("OO_TEST_SHOTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("oo-device-input-test"));
        std::fs::create_dir_all(&dir).unwrap();
        let simctl = |args: &[&str]| crate::devices::simctl::simctl(args).map(drop);
        let ms = Duration::from_millis;
        // Every step is tried, so a miss shows whether input died or one
        // event went astray.
        let missed = std::cell::RefCell::new(Vec::new());
        let check = |ok: bool, what: &'static str| {
            eprintln!("{} {what}", if ok { "ok  " } else { "MISS" });
            if !ok {
                missed.borrow_mut().push(what);
            }
        };

        let (events, mut rx) = futures::channel::mpsc::unbounded();
        let started = Instant::now();
        let helper = InputHelper::spawn(&udid, 7, events).expect("the helper, built");
        loop {
            match rx.try_recv() {
                Ok(HelperEvent::Ready(7)) => break,
                Ok(other) => panic!("{other:?}"),
                Err(_) if started.elapsed() < Duration::from_secs(10) => sleep(ms(20)),
                Err(_) => panic!("not ready in 10 s"),
            }
        }
        eprintln!("ready in {:?}", started.elapsed());
        let tap = |x, y| {
            helper.touch(TouchPhase::Down, x, y);
            sleep(ms(80));
            helper.touch(TouchPhase::Up, x, y);
        };

        // Home, from Settings.
        simctl(&["launch", &udid, "com.apple.Preferences"]).unwrap();
        sleep(ms(4000));
        let settings = shot(&udid, &dir, "a0-settings");
        assert_eq!(helper.home().recv_timeout(Duration::from_secs(5)), Ok(Ok(())));
        sleep(ms(1500));
        let home = shot(&udid, &dir, "a1-home");
        check(differ(&settings, &home) > 8.0, "Home");

        // Settings' icon on the default home screen, then General.
        let _ = simctl(&["terminate", &udid, "com.apple.Preferences"]);
        sleep(ms(500));
        tap(0.855, 0.487);
        sleep(ms(5000));
        let opened = shot(&udid, &dir, "b1-settings");
        check(differ(&home, &opened) > 8.0, "tap Settings");
        tap(0.3, 0.465);
        sleep(ms(1500));
        let general = shot(&udid, &dir, "b2-general");
        check(differ(&opened, &general) > 3.0, "tap General");

        // A swipe up scrolls it.
        helper.touch(TouchPhase::Down, 0.5, 0.75);
        for i in 1..=15 {
            sleep(ms(20));
            helper.touch(TouchPhase::Move, 0.5, 0.75 - 0.03 * f64::from(i));
        }
        helper.touch(TouchPhase::Up, 0.5, 0.3);
        sleep(ms(1200));
        let scrolled = shot(&udid, &dir, "c1-scrolled");
        check(differ(&general, &scrolled) > 3.0, "swipe");

        // Safari: the address bar, a URL typed over the old one, Return.
        simctl(&["openurl", &udid, "https://example.com"]).unwrap();
        sleep(ms(5000));
        let page = shot(&udid, &dir, "d0-page");
        tap(0.5, 0.933);
        sleep(ms(1500));
        let bar = shot(&udid, &dir, "d1-bar");
        check(differ(&page, &bar) > 3.0, "tap the address bar");
        helper.text("Hello OO 42");
        sleep(ms(1500));
        let typed = shot(&udid, &dir, "d2-typed");
        check(differ(&bar, &typed) > 1.0, "type");
        for _ in 0.."Hello OO 42".len() {
            helper.press(0x2A, false);
        }
        // Safari's suggestions trail a burst of keys (and can complete it to
        // a page in its history, or take a Return meant for the field):
        // give it a moment, as the spike did.
        sleep(ms(800));
        helper.text("example.org");
        sleep(ms(800));
        helper.press(0x28, false);
        sleep(ms(5000));
        let loaded = shot(&udid, &dir, "d3-loaded");
        check(differ(&typed, &loaded) > 3.0, "Return loads the URL");
        // Let the last sends leave before the helper goes.
        sleep(ms(500));
        drop(helper);
        eprintln!("screenshots in {}", dir.display());
        assert!(missed.borrow().is_empty(), "{:?}", missed.borrow());
    }
}
