//! Quoting argv for a Windows command line, and for display.

/// Append `arg` to a Windows command line, always double-quoted, by the
/// rules the Microsoft C runtime (and Git Bash's msys runtime) parse with:
/// backslashes are literal except before a `"`, where they are doubled, and
/// the quote itself is escaped. Always quoting keeps msys from treating `'`
/// as a quote and from expanding `*` against the current directory.
pub fn push_windows_arg(out: &mut String, arg: &str) {
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

/// A whole Windows command line for `argv`.
pub fn windows_command_line(argv: &[String]) -> String {
    let mut out = String::new();
    for (i, arg) in argv.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        push_windows_arg(&mut out, arg);
    }
    out
}

/// `argv` as one string, the way tmux shows `#{pane_start_command}`: words
/// that need it are double-quoted with `\`, `"`, `$` and `` ` `` escaped.
pub fn start_command(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            let plain = !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
            if plain {
                a.clone()
            } else {
                let mut q = String::from("\"");
                for c in a.chars() {
                    if matches!(c, '\\' | '"' | '$' | '`') {
                        q.push('\\');
                    }
                    q.push(c);
                }
                q.push('"');
                q
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn windows_quoting_follows_the_crt_rules() {
        assert_eq!(windows_command_line(&s(&["a b", ""])), r#""a b" """#);
        assert_eq!(windows_command_line(&s(&[r#"say "hi""#])), r#""say \"hi\"""#);
        assert_eq!(windows_command_line(&s(&[r"C:\dir\"])), r#""C:\dir\\""#);
        assert_eq!(windows_command_line(&s(&[r#"a\"b"#])), r#""a\\\"b""#);
    }

    #[test]
    fn start_command_looks_like_tmuxs() {
        let argv = s(&["/bin/zsh", "-lic", r#"'/x/claude' '--flag'; exec "$SHELL" -li"#]);
        assert_eq!(
            start_command(&argv),
            r#"/bin/zsh -lic "'/x/claude' '--flag'; exec \"\$SHELL\" -li""#
        );
    }
}
