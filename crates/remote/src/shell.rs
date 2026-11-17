//! Shell tabs as a conversation: a zsh hook (loaded through `ZDOTDIR`, after
//! the user's own startup files) appends one JSON line per command to a
//! per-tab log, and this module turns that log into display entries.
//!
//! Log records (one JSON object per line):
//! - `{"t":"hello","at":MS}` when the shell starts (the log exists from the
//!   first prompt, which is what makes the tab offer a conversation);
//! - `{"t":"start","n":N,"cmd":"…","cwd":"…","at":MS}` before a command runs;
//! - `{"t":"end","n":N,"exit":CODE,"at":MS,"out":"…"}` when it finishes, with
//!   the output read back from the tmux pane (at most the last 400 lines).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::conversation::{Entry, EntryKind, ToolLine, ToolResult, ToolStatus};

/// Lines of output shown inline under a command; the rest is one tap away.
const INLINE_LINES: usize = 40;
const SUMMARY_CAP: usize = 80;

const ZSHENV: &str = r#"# openOrchestra: load the user's .zshenv as if ZDOTDIR were theirs.
__oo_zd=$ZDOTDIR
ZDOTDIR=${OO_USER_ZDOTDIR:-$HOME}
[[ -r $ZDOTDIR/.zshenv ]] && source "$ZDOTDIR/.zshenv"
OO_USER_ZDOTDIR=$ZDOTDIR
ZDOTDIR=$__oo_zd
unset __oo_zd
"#;

const ZPROFILE: &str = r#"__oo_zd=$ZDOTDIR; ZDOTDIR=$OO_USER_ZDOTDIR
[[ -r $ZDOTDIR/.zprofile ]] && source "$ZDOTDIR/.zprofile"
ZDOTDIR=$__oo_zd; unset __oo_zd
"#;

const ZSHRC: &str = r#"__oo_zd=$ZDOTDIR; ZDOTDIR=$OO_USER_ZDOTDIR
[[ -r $ZDOTDIR/.zshrc ]] && source "$ZDOTDIR/.zshrc"
ZDOTDIR=$__oo_zd; unset __oo_zd
[[ -r ${ZDOTDIR}/oo-shell.zsh ]] && source "${ZDOTDIR}/oo-shell.zsh"
"#;

// .zlogin is the last file zsh reads: hand ZDOTDIR back to the user there, so
// shells they start from this one behave exactly as anywhere else.
const ZLOGIN: &str = r#"ZDOTDIR=$OO_USER_ZDOTDIR
[[ -r $ZDOTDIR/.zlogin ]] && source "$ZDOTDIR/.zlogin"
"#;

const HOOK: &str = r#"# openOrchestra Remote: one JSON line per command, for the phone's conversation view.
[[ -n $OO_SHELL_LOG && -n $TMUX_PANE ]] || return 0
zmodload zsh/datetime 2>/dev/null || return 0
[[ -z $__oo_loaded ]] || return 0
typeset -g __oo_loaded=1 __oo_cmd=""
typeset -gi __oo_n=0 __oo_start=-1

__oo_json() {
  local s=$1
  s=${s//\\/\\\\}; s=${s//\"/\\\"}; s=${s//$'\n'/\\n}; s=${s//$'\t'/\\t}
  s=${s//[[:cntrl:]]/}
  REPLY=$s
}
__oo_ms() { local t=$EPOCHREALTIME; REPLY=${t%.*}${${t#*.}[1,3]} }
# Absolute line of the cursor in the pane (history + row), and its column.
__oo_pos() {
  local p
  p=$(command tmux display-message -p -t "$TMUX_PANE" '#{history_size} #{cursor_y} #{cursor_x}' 2>/dev/null) || { REPLY=""; return 1 }
  REPLY=$p
}

# Owner-only file, whatever the user's umask.
__oo_write() { ( umask 077; print -r -- "$1" >>| "$OO_SHELL_LOG" ) }

# Only while the Mac shares this tab with a device: it creates "$OO_SHELL_LOG.on".
__oo_preexec() {
  [[ -e "$OO_SHELL_LOG.on" ]] || return 0
  __oo_cmd=$1
  (( __oo_n++ ))
  local -a p
  if __oo_pos; then p=(${=REPLY}); __oo_start=$(( p[1] + p[2] )); else __oo_start=-1; fi
  local c d
  __oo_json "$1"; c=$REPLY
  __oo_json "$PWD"; d=$REPLY
  __oo_ms
  __oo_write "{\"t\":\"start\",\"n\":$__oo_n,\"cmd\":\"$c\",\"cwd\":\"$d\",\"at\":$REPLY}"
}

__oo_precmd() {
  local ex=$?
  [[ -n $__oo_cmd ]] || return 0
  __oo_cmd=""
  local out=""
  local -a p
  if (( __oo_start >= 0 )) && __oo_pos; then
    p=(${=REPLY})
    local hs=$p[1] end=$(( p[1] + p[2] ))
    (( p[3] > 0 )) && (( end++ ))
    local first=$__oo_start
    (( end - first > 400 )) && first=$(( end - 400 ))
    if (( end > first )); then
      out=$(command tmux capture-pane -p -J -t "$TMUX_PANE" -S $(( first - hs )) -E $(( end - 1 - hs )) 2>/dev/null)
    fi
  fi
  __oo_json "$out"; out=$REPLY
  __oo_ms
  __oo_write "{\"t\":\"end\",\"n\":$__oo_n,\"exit\":$ex,\"at\":$REPLY,\"out\":\"$out\"}"
}

autoload -Uz add-zsh-hook
add-zsh-hook preexec __oo_preexec
# First, before any prompt theme draws: the output must be read back untouched.
precmd_functions=(__oo_precmd ${precmd_functions:#__oo_precmd})
__oo_ms
__oo_write "{\"t\":\"hello\",\"at\":$REPLY}"
"#;

/// Where the shell tab running in tmux session `session` logs its commands.
pub fn log_path(state_dir: &Path, session: &str) -> PathBuf {
    state_dir
        .join("remote")
        .join("shell")
        .join(format!("{session}.jsonl"))
}

/// Write the zsh startup files (idempotent) and return the `ZDOTDIR` to
/// start shell tabs with.
pub fn install(state_dir: &Path) -> io::Result<PathBuf> {
    let dir = state_dir.join("remote").join("zsh");
    private_dir(&dir)?;
    private_dir(&shell_dir(state_dir))?;
    for (name, body) in [
        (".zshenv", ZSHENV),
        (".zprofile", ZPROFILE),
        (".zshrc", ZSHRC),
        (".zlogin", ZLOGIN),
        ("oo-shell.zsh", HOOK),
    ] {
        let path = dir.join(name);
        if fs::read_to_string(&path).ok().as_deref() != Some(body) {
            fs::write(&path, body)?;
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(dir)
}

fn shell_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("remote").join("shell")
}

/// Create `dir` if needed and make it owner-only (fixing one made earlier
/// with looser permissions).
fn private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

/// The switch that lets the hook record `session`'s commands.
fn flag_path(state_dir: &Path, session: &str) -> PathBuf {
    let mut p = log_path(state_dir, session).into_os_string();
    p.push(".on");
    PathBuf::from(p)
}

const LOG_SUFFIX: &str = ".jsonl";
const FLAG_SUFFIX: &str = ".jsonl.on";

/// Let exactly the shell tabs in `shared` (tmux session names) record
/// commands; every other tab's hook goes quiet. An empty set turns all off.
pub fn sync_flags(state_dir: &Path, shared: &HashSet<String>) -> io::Result<()> {
    let dir = shell_dir(state_dir);
    private_dir(&dir)?;
    for entry in fs::read_dir(&dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(session) = name.strip_suffix(FLAG_SUFFIX) {
            if !shared.contains(session) {
                fs::remove_file(dir.join(&name))?;
            }
        }
    }
    for session in shared {
        let flag = flag_path(state_dir, session);
        if !flag.exists() {
            fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .mode(0o600)
                .open(flag)?;
        }
    }
    Ok(())
}

/// A log is trimmed once past this size...
const TRIM_AT: u64 = 2 * 1024 * 1024;
/// ...to about its last this many bytes.
const KEEP: usize = 512 * 1024;
/// A closed tab's log is deleted once untouched this long (a brand-new
/// shell may write before tmux lists its session).
const CLOSED_GRACE: Duration = Duration::from_secs(10 * 60);

/// Housekeeping for `live` (the tmux sessions that exist, from a listing
/// that succeeded): delete closed tabs' logs and switches, trim long logs,
/// and make every log owner-only.
pub fn collect(state_dir: &Path, live: &HashSet<String>, now: SystemTime) -> io::Result<()> {
    let dir = shell_dir(state_dir);
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(session) = name.strip_suffix(LOG_SUFFIX) else {
            continue;
        };
        let path = entry.path();
        let meta = entry.metadata()?;
        let idle = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or_default();
        if !live.contains(session) {
            if idle >= CLOSED_GRACE {
                fs::remove_file(&path)?;
                let _ = fs::remove_file(flag_path(state_dir, session));
            }
            continue;
        }
        if meta.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        if meta.len() > TRIM_AT {
            trim(&path)?;
        }
    }
    Ok(())
}

/// Keep about the last [`KEEP`] bytes of `path`, from a line start.
fn trim(path: &Path) -> io::Result<()> {
    let data = fs::read(path)?;
    let from = data.len().saturating_sub(KEEP);
    let from = data[from..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(data.len(), |i| from + i + 1);
    let tmp = path.with_extension("jsonl.tmp");
    fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?
        .write_all(&data[from..])?;
    fs::rename(tmp, path)
}

/// The environment a new shell tab's tmux session needs for the hook. The
/// user's own `ZDOTDIR` (if any) is passed on so their files are found.
pub fn session_env(zdotdir: &Path, log: &Path) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "ZDOTDIR".to_string(),
            zdotdir.to_string_lossy().into_owned(),
        ),
        (
            "OO_SHELL_LOG".to_string(),
            log.to_string_lossy().into_owned(),
        ),
    ];
    if let Ok(user) = std::env::var("ZDOTDIR") {
        if !user.is_empty() && Path::new(&user) != zdotdir {
            env.push(("OO_USER_ZDOTDIR".to_string(), user));
        }
    }
    env
}

struct Cmd {
    cmd: String,
    start: Option<i64>,
    end: Option<(i64, i64, String)>,
}

/// The log's commands in order, each with an id that stays the same when
/// older lines are trimmed away: its start time and number (`at-n`).
fn commands(text: &str) -> Vec<(String, Cmd)> {
    let mut order = Vec::new();
    let mut by_n: HashMap<u64, Cmd> = HashMap::new();
    for line in text.lines() {
        let Ok(r) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let Some(n) = r["n"].as_u64() else { continue };
        match r["t"].as_str() {
            Some("start") => {
                // A new shell in the same tab restarts numbering: start over.
                if by_n.contains_key(&n) {
                    let key = order.len() as u64 + 1_000_000;
                    if let Some(old) = by_n.remove(&n) {
                        by_n.insert(key, old);
                        if let Some(slot) = order.iter_mut().find(|k| **k == n) {
                            *slot = key;
                        }
                    }
                }
                order.push(n);
                by_n.insert(
                    n,
                    Cmd {
                        cmd: r["cmd"].as_str().unwrap_or_default().to_string(),
                        start: r["at"].as_i64(),
                        end: None,
                    },
                );
            }
            Some("end") => {
                if let Some(c) = by_n.get_mut(&n) {
                    c.end = Some((
                        r["exit"].as_i64().unwrap_or(0),
                        r["at"].as_i64().unwrap_or(0),
                        clean(r["out"].as_str().unwrap_or_default()),
                    ));
                }
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .enumerate()
        .filter_map(|(i, k)| {
            let c = by_n.remove(&k)?;
            let id = match c.start {
                Some(at) => format!("{at}-{}", k % 1_000_000),
                None => format!("i{i}"),
            };
            Some((id, c))
        })
        .collect()
}

/// Output as shown: trailing spaces and blank lines trimmed, and zsh's
/// "no newline at the end" mark (`%` padded to the pane's width) dropped.
fn clean(out: &str) -> String {
    let mut lines: Vec<&str> = out.lines().collect();
    if let Some(last) = lines.last_mut() {
        let body = last.trim_end();
        if body.ends_with('%') && last.len() - body.len() >= 2 {
            *last = &body[..body.len() - 1];
        }
    }
    let lines: Vec<&str> = lines.into_iter().map(str::trim_end).collect();
    let end = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map_or(0, |i| i + 1);
    let start = lines[..end]
        .iter()
        .position(|l| !l.is_empty())
        .unwrap_or(end);
    lines[start..end].join("\n")
}

fn rfc3339(ms: i64) -> Option<String> {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).ok()?;
    t.format(&time::format_description::well_known::Rfc3339)
        .ok()
}

/// A code fence longer than any backtick run in `s`.
fn fenced(s: &str) -> String {
    let longest = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}\n{s}\n{fence}")
}

fn duration_text(ms: i64) -> String {
    match ms {
        ms if ms < 1000 => format!("{ms} ms"),
        ms if ms < 60_000 => format!("{:.1} s", ms as f64 / 1000.0),
        ms => format!("{} min {} s", ms / 60_000, (ms % 60_000) / 1000),
    }
}

/// Each command: the command as the user's message, its last lines of output
/// inline, and a status line (exit code, time) whose tap shows all of it.
pub fn parse_entries(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for (i, c) in commands(text) {
        let i = &i;
        out.push(Entry {
            id: format!("c{i}"),
            kind: EntryKind::User,
            text: Some(format!("$ {}", c.cmd)),
            tool: None,
            duration_ms: None,
            at: c.start.and_then(rfc3339),
        });
        let Some((exit, end_at, output)) = &c.end else {
            out.push(Entry {
                id: format!("s{i}"),
                kind: EntryKind::Tool,
                text: None,
                tool: Some(ToolLine {
                    id: format!("o{i}"),
                    name: "$".into(),
                    summary: "running…".into(),
                    status: ToolStatus::Pending,
                }),
                duration_ms: None,
                at: None,
            });
            continue;
        };
        let lines: Vec<&str> = output.lines().collect();
        if !lines.is_empty() {
            let shown = &lines[lines.len().saturating_sub(INLINE_LINES)..];
            out.push(Entry {
                id: format!("r{i}"),
                kind: EntryKind::Assistant,
                text: Some(fenced(&shown.join("\n"))),
                tool: None,
                duration_ms: None,
                at: rfc3339(*end_at),
            });
        }
        let took = c.start.map(|s| (end_at - s).max(0));
        let mut summary = format!("exit {exit}");
        if let Some(ms) = took {
            summary.push_str(&format!(" · {}", duration_text(ms)));
        }
        if lines.len() > INLINE_LINES {
            summary.push_str(&format!(" · {} lines, tap for all", lines.len()));
        }
        summary.truncate(SUMMARY_CAP);
        out.push(Entry {
            id: format!("s{i}"),
            kind: EntryKind::Tool,
            text: None,
            tool: Some(ToolLine {
                id: format!("o{i}"),
                name: "$".into(),
                summary,
                status: if *exit == 0 {
                    ToolStatus::Ok
                } else {
                    ToolStatus::Error
                },
            }),
            duration_ms: took.map(|ms| ms as u64),
            at: None,
        });
    }
    out
}

/// The full output of the command whose status line is `tool_id` (`o{i}`).
pub fn tool_result(text: &str, tool_id: &str) -> Option<ToolResult> {
    let id = tool_id.strip_prefix('o')?;
    let (_, c) = commands(text).into_iter().find(|(key, _)| key == id)?;
    let (exit, _, output) = c.end?;
    Some(ToolResult {
        text: if output.is_empty() {
            "(no output)".into()
        } else {
            output
        },
        is_error: exit != 0,
        truncated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = r#"{"t":"hello","at":1000}
{"t":"start","n":1,"cmd":"ls","cwd":"/p","at":2000}
{"t":"end","n":1,"exit":0,"at":2150,"out":"a.txt  \nb.txt\n\n"}
{"t":"start","n":2,"cmd":"false","cwd":"/p","at":3000}
{"t":"end","n":2,"exit":1,"at":3001,"out":""}
{"t":"start","n":3,"cmd":"sleep 9","cwd":"/p","at":4000}
"#;

    #[test]
    fn commands_become_messages_output_and_status() {
        let e = parse_entries(LOG);
        let kinds: Vec<_> = e.iter().map(|e| (e.kind, e.id.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (EntryKind::User, "c2000-1"),
                (EntryKind::Assistant, "r2000-1"),
                (EntryKind::Tool, "s2000-1"),
                (EntryKind::User, "c3000-2"),
                (EntryKind::Tool, "s3000-2"),
                (EntryKind::User, "c4000-3"),
                (EntryKind::Tool, "s4000-3"),
            ]
        );
        assert_eq!(e[0].text.as_deref(), Some("$ ls"));
        assert_eq!(e[1].text.as_deref(), Some("```\na.txt\nb.txt\n```"));
        let t = e[2].tool.as_ref().unwrap();
        assert_eq!(
            (t.status, t.summary.as_str()),
            (ToolStatus::Ok, "exit 0 · 150 ms")
        );
        assert_eq!(e[4].tool.as_ref().unwrap().status, ToolStatus::Error);
        let running = e[6].tool.as_ref().unwrap();
        assert_eq!(
            (running.status, running.summary.as_str()),
            (ToolStatus::Pending, "running…")
        );
    }

    #[test]
    fn long_output_is_cut_inline_and_whole_on_tap() {
        let out: Vec<String> = (1..=100).map(|i| format!("line {i}")).collect();
        let log = format!(
            "{{\"t\":\"start\",\"n\":1,\"cmd\":\"seq\",\"at\":0}}\n{{\"t\":\"end\",\"n\":1,\"exit\":0,\"at\":5,\"out\":{}}}\n",
            serde_json::to_string(&out.join("\n")).unwrap()
        );
        let e = parse_entries(&log);
        let inline = e[1].text.as_deref().unwrap();
        assert!(
            inline.contains("line 61\n") && !inline.contains("line 60\n"),
            "{inline}"
        );
        assert!(e[2].tool.as_ref().unwrap().summary.contains("100 lines"));
        let full = tool_result(&log, "o0-1").unwrap();
        assert_eq!(full.text.lines().count(), 100);
        assert!(!full.is_error);
    }

    #[test]
    fn zsh_no_newline_mark_is_dropped() {
        assert_eq!(clean("no newline%                    "), "no newline");
        // A real trailing % (no padding) stays.
        assert_eq!(clean("100%"), "100%");
    }

    #[test]
    fn backticks_in_output_do_not_break_the_fence() {
        assert_eq!(fenced("a ``` b"), "````\na ``` b\n````");
    }

    #[test]
    fn a_restarted_shell_keeps_earlier_commands() {
        let log = r#"{"t":"start","n":1,"cmd":"one","at":0}
{"t":"end","n":1,"exit":0,"at":1,"out":""}
{"t":"hello","at":2}
{"t":"start","n":1,"cmd":"two","at":3}
{"t":"end","n":1,"exit":0,"at":4,"out":"x"}
"#;
        let users: Vec<_> = parse_entries(log)
            .into_iter()
            .filter(|e| e.kind == EntryKind::User)
            .map(|e| e.text.unwrap())
            .collect();
        assert_eq!(users, vec!["$ one", "$ two"]);
        assert_eq!(tool_result(log, "o3-1").unwrap().text, "x");
        assert_eq!(tool_result(log, "o0-1").unwrap().text, "(no output)");
    }

    #[test]
    fn install_writes_startup_files_once() {
        let dir = tempfile::tempdir().unwrap();
        let z = install(dir.path()).unwrap();
        for f in [".zshenv", ".zprofile", ".zshrc", ".zlogin", "oo-shell.zsh"] {
            assert!(z.join(f).is_file(), "{f}");
        }
        install(dir.path()).unwrap();
        assert!(dir.path().join("remote/shell").is_dir());
    }
}

#[cfg(test)]
mod stable_ids {
    use super::*;

    #[test]
    fn trimming_older_lines_keeps_ids_and_taps_on_the_right_command() {
        let full = r#"{"t":"start","n":1,"cmd":"one","at":10}
{"t":"end","n":1,"exit":0,"at":11,"out":"first"}
{"t":"start","n":2,"cmd":"two","at":20}
{"t":"end","n":2,"exit":0,"at":21,"out":"second"}
"#;
        let trimmed: String = full.lines().skip(2).map(|l| format!("{l}\n")).collect();
        let id = |text: &str| {
            parse_entries(text)
                .into_iter()
                .find(|e| e.text.as_deref() == Some("$ two"))
                .unwrap()
                .id
        };
        assert_eq!(id(full), id(&trimmed));
        assert_eq!(tool_result(&trimmed, "o20-2").unwrap().text, "second");
    }
}

#[cfg(test)]
mod housekeeping {
    use super::*;
    use std::collections::HashSet;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }
    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn switches_follow_the_shared_tabs_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let st = dir.path();
        sync_flags(st, &set(&["a", "b"])).unwrap();
        assert!(flag_path(st, "a").exists() && flag_path(st, "b").exists());
        assert_eq!(mode(&flag_path(st, "a")), 0o600);
        assert_eq!(mode(&shell_dir(st)), 0o700);
        sync_flags(st, &set(&["b"])).unwrap();
        assert!(!flag_path(st, "a").exists() && flag_path(st, "b").exists());
        sync_flags(st, &HashSet::new()).unwrap();
        assert!(!flag_path(st, "b").exists());
    }

    #[test]
    fn install_tightens_folders_made_before() {
        let dir = tempfile::tempdir().unwrap();
        let shell = shell_dir(dir.path());
        fs::create_dir_all(&shell).unwrap();
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).unwrap();
        let z = install(dir.path()).unwrap();
        assert_eq!(mode(&shell), 0o700);
        assert_eq!(mode(&z), 0o700);
        assert_eq!(mode(&z.join("oo-shell.zsh")), 0o600);
    }

    #[test]
    fn closed_tabs_logs_go_after_a_grace_period_live_ones_stay_private() {
        let dir = tempfile::tempdir().unwrap();
        let st = dir.path();
        install(st).unwrap();
        let (live, gone) = (log_path(st, "live"), log_path(st, "gone"));
        fs::write(&live, "{}\n").unwrap();
        fs::write(&gone, "{}\n").unwrap();
        fs::set_permissions(&live, fs::Permissions::from_mode(0o644)).unwrap();
        let now = SystemTime::now();
        collect(st, &set(&["live"]), now).unwrap();
        assert!(gone.exists(), "too fresh to delete");
        assert_eq!(mode(&live), 0o600);
        collect(
            st,
            &set(&["live"]),
            now + CLOSED_GRACE + Duration::from_secs(1),
        )
        .unwrap();
        assert!(!gone.exists() && live.exists());
    }

    #[test]
    fn long_logs_are_trimmed_at_a_line_start() {
        let dir = tempfile::tempdir().unwrap();
        let st = dir.path();
        install(st).unwrap();
        let p = log_path(st, "s");
        let line = format!("{}\n", "x".repeat(999));
        fs::write(&p, line.repeat(3000)).unwrap();
        collect(st, &set(&["s"]), SystemTime::now()).unwrap();
        let kept = fs::read_to_string(&p).unwrap();
        assert!(
            kept.len() <= KEEP && kept.len() > KEEP / 2,
            "{}",
            kept.len()
        );
        assert!(kept.lines().all(|l| l.len() == 999));
    }
}
