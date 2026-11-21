//! `send-keys` key names to the bytes a terminal would send for them.

/// The bytes for tmux key `name`, or `None` when it is not a key name (tmux
/// then types the argument as text).
pub fn key_bytes(name: &str) -> Option<Vec<u8>> {
    let fixed: &[u8] = match name {
        "Enter" | "KPEnter" => b"\r",
        "Escape" | "Esc" => b"\x1b",
        "Tab" => b"\t",
        "BTab" => b"\x1b[Z",
        "BSpace" => b"\x7f",
        "Space" => b" ",
        "Up" => b"\x1b[A",
        "Down" => b"\x1b[B",
        "Right" => b"\x1b[C",
        "Left" => b"\x1b[D",
        "Home" => b"\x1b[H",
        "End" => b"\x1b[F",
        "DC" | "Delete" => b"\x1b[3~",
        "IC" | "Insert" => b"\x1b[2~",
        "PPage" | "PageUp" => b"\x1b[5~",
        "NPage" | "PageDown" => b"\x1b[6~",
        _ => {
            // C-x: control character; M-x: ESC prefix.
            if let Some(k) = name.strip_prefix("C-") {
                let c = single(k)?;
                return match c {
                    'a'..='z' => Some(vec![c as u8 - b'a' + 1]),
                    'A'..='Z' => Some(vec![c as u8 - b'A' + 1]),
                    '@' | ' ' => Some(vec![0]),
                    '[' => Some(vec![0x1b]),
                    '\\' => Some(vec![0x1c]),
                    ']' => Some(vec![0x1d]),
                    '^' => Some(vec![0x1e]),
                    '_' => Some(vec![0x1f]),
                    '?' => Some(vec![0x7f]),
                    _ => None,
                };
            }
            if let Some(k) = name.strip_prefix("M-") {
                let mut out = vec![0x1b];
                out.extend(key_bytes(k).unwrap_or_else(|| k.as_bytes().to_vec()));
                return Some(out);
            }
            return None;
        }
    };
    Some(fixed.to_vec())
}

fn single(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

/// What `send-keys` types for its arguments: with `-l` the text as is,
/// otherwise each argument as a key name, or as text when it is not one.
pub fn send_keys_bytes(args: &[String], literal: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for arg in args {
        match (literal, key_bytes(arg)) {
            (false, Some(bytes)) => out.extend(bytes),
            _ => out.extend(arg.as_bytes()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_text() {
        assert_eq!(key_bytes("Enter").unwrap(), b"\r");
        assert_eq!(key_bytes("C-c").unwrap(), [3]);
        assert_eq!(key_bytes("M-x").unwrap(), b"\x1bx");
        assert_eq!(key_bytes("hello"), None);
        let args = vec!["echo hi".to_string(), "Enter".to_string()];
        assert_eq!(send_keys_bytes(&args, false), b"echo hi\r");
        assert_eq!(send_keys_bytes(&args, true), b"echo hiEnter");
    }
}
