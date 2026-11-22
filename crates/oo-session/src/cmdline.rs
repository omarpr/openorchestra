//! Parsing tmux-style command lines: commands separated by bare `;`
//! arguments, each a name, getopt-style flags, then positional arguments.

/// One command of a command sequence.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Command {
    /// Canonical name (aliases such as `ls` or `new` resolved).
    pub name: String,
    /// Flags in order; a flag that takes a value carries it.
    pub flags: Vec<(char, Option<String>)>,
    /// Positional arguments after the flags.
    pub args: Vec<String>,
}

impl Command {
    /// Whether the boolean flag `c` was given.
    pub fn has(&self, c: char) -> bool {
        self.flags.iter().any(|(f, _)| *f == c)
    }

    /// The last value given for flag `c`.
    pub fn value(&self, c: char) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(f, v)| *f == c && v.is_some())
            .and_then(|(_, v)| v.as_deref())
    }

    /// Every value given for flag `c`, in order (`-e A=1 -e B=2`).
    pub fn values(&self, c: char) -> Vec<&str> {
        self.flags
            .iter()
            .filter(|(f, _)| *f == c)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }

    /// The `-t` target with tmux's exact-match `=` prefix removed.
    pub fn target(&self) -> Option<&str> {
        self.value('t').map(|t| t.strip_prefix('=').unwrap_or(t))
    }
}

/// Split an argv into commands on bare `;` arguments (tmux's own rule).
/// Empty commands (`a ; ; b`, a trailing `;`) are dropped.
pub fn split(args: &[String]) -> Vec<Vec<String>> {
    args.split(|a| a == ";")
        .filter(|c| !c.is_empty())
        .map(<[String]>::to_vec)
        .collect()
}

/// The canonical name for a command or one of its aliases.
pub fn canonical(name: &str) -> &str {
    match name {
        "ls" => "list-sessions",
        "lsp" => "list-panes",
        "new" => "new-session",
        "a" | "at" | "attach" => "attach-session",
        "has" => "has-session",
        "kill-ses" => "kill-session",
        "set" => "set-option",
        "send" => "send-keys",
        "display" => "display-message",
        "if" => "if-shell",
        other => other,
    }
}

/// Flags of `command` that take a value.
fn value_flags(command: &str) -> &'static str {
    match command {
        "new-session" => "cefnstxyF",
        "attach-session" => "cft",
        "list-sessions" => "Ff",
        "list-panes" => "Fft",
        "send-keys" => "Nt",
        "display-message" => "cdt",
        "capture-pane" => "EbSt",
        "resize-window" => "txy",
        _ => "t",
    }
}

/// Parse one command (`words[0]` is its name). Flags stop at the first
/// non-flag word or `--`, as getopt does, so a program's own flags after it
/// (`new-session -d bash -lic …`) stay positional.
pub fn parse(words: &[String]) -> Option<Command> {
    let (name, rest) = words.split_first()?;
    let name = canonical(name).to_string();
    let takes_value = value_flags(&name);
    let mut cmd = Command {
        name,
        ..Default::default()
    };
    let mut i = 0;
    while i < rest.len() {
        let word = &rest[i];
        if word == "--" {
            i += 1;
            break;
        }
        if !word.starts_with('-') || word.len() < 2 {
            break;
        }
        let chars: Vec<char> = word[1..].chars().collect();
        for (j, &c) in chars.iter().enumerate() {
            if takes_value.contains(c) {
                let inline: String = chars[j + 1..].iter().collect();
                let value = if !inline.is_empty() {
                    Some(inline)
                } else {
                    i += 1;
                    rest.get(i).cloned()
                };
                cmd.flags.push((c, value));
                break;
            }
            cmd.flags.push((c, None));
        }
        i += 1;
    }
    cmd.args = rest[i.min(rest.len())..].to_vec();
    Some(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn splits_on_bare_semicolons_only() {
        let parts = split(&s(&["a", "x;y", ";", "b", ";", ";", "c", ";"]));
        assert_eq!(parts, vec![s(&["a", "x;y"]), s(&["b"]), s(&["c"])]);
    }

    #[test]
    fn new_session_keeps_the_programs_own_flags() {
        let c = parse(&s(&[
            "new-session", "-A", "-d", "-s", "harness_k_7", "-c", "C:\\repo", "-e", "A=1", "-eB=2",
            "bash", "-lic", "claude; exec bash -li",
        ]))
        .unwrap();
        assert!(c.has('A') && c.has('d'));
        assert_eq!(c.value('s'), Some("harness_k_7"));
        assert_eq!(c.value('c'), Some("C:\\repo"));
        assert_eq!(c.values('e'), vec!["A=1", "B=2"]);
        assert_eq!(c.args, s(&["bash", "-lic", "claude; exec bash -li"]));
    }

    #[test]
    fn aliases_and_exact_targets() {
        let c = parse(&s(&["has", "-t", "=harness_k_1"])).unwrap();
        assert_eq!(c.name, "has-session");
        assert_eq!(c.target(), Some("harness_k_1"));
        let c = parse(&s(&["ls", "-F", "#{session_name}"])).unwrap();
        assert_eq!(c.name, "list-sessions");
        assert_eq!(c.value('F'), Some("#{session_name}"));
    }

    #[test]
    fn send_keys_literal_and_copy_mode_commands() {
        let c = parse(&s(&["send-keys", "-t", "s", "-l", "hi -x there"])).unwrap();
        assert!(c.has('l'));
        assert_eq!(c.args, s(&["hi -x there"]));
        let c = parse(&s(&["send-keys", "-X", "-t", "s", "cancel"])).unwrap();
        assert!(c.has('X'));
        assert_eq!(c.args, s(&["cancel"]));
    }

    #[test]
    fn remote_bridge_commands() {
        let c = parse(&s(&["send-keys", "-t", "%12", "-H", "68", "0d"])).unwrap();
        assert!(c.has('H'));
        assert_eq!(c.target(), Some("%12"));
        assert_eq!(c.args, s(&["68", "0d"]));
        let c = parse(&s(&["resize-window", "-t", "%12", "-x", "40", "-y", "20"])).unwrap();
        assert_eq!((c.value('x'), c.value('y')), (Some("40"), Some("20")));
        let c = parse(&s(&["set-option", "-wu", "-t", "%12", "window-size"])).unwrap();
        assert!(c.has('w') && c.has('u'));
        let c = parse(&s(&["attach-session", "-t", "=s", "-f", "ignore-size"])).unwrap();
        assert_eq!(c.value('f'), Some("ignore-size"));
    }
}
