//! tmux format strings, the small part of them the app uses: `#{name}` is
//! replaced by the variable's value (empty when unknown) and `##` is a `#`.

/// Expand `fmt`, asking `lookup` for each `#{name}`.
pub fn expand(fmt: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(fmt.len());
    let mut rest = fmt;
    while let Some(at) = rest.find('#') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(tail) = after.strip_prefix('#') {
            out.push('#');
            rest = tail;
        } else if let Some(body) = after.strip_prefix('{') {
            match body.find('}') {
                Some(end) => {
                    out.push_str(&lookup(&body[..end]).unwrap_or_default());
                    rest = &body[end + 1..];
                }
                None => {
                    out.push_str(&rest[at..]);
                    rest = "";
                }
            }
        } else {
            out.push('#');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Whether an `if-shell -F` condition is true: non-empty and not `0`.
pub fn truthy(value: &str) -> bool {
    !value.is_empty() && value != "0"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(name: &str) -> Option<String> {
        match name {
            "session_name" => Some("harness_k_1".into()),
            "pane_pid" => Some("4321".into()),
            _ => None,
        }
    }

    #[test]
    fn expands_known_and_blanks_unknown() {
        assert_eq!(
            expand("#{session_name}\t#{pane_pid}\t#{@oo_agent}", &vars),
            "harness_k_1\t4321\t"
        );
        assert_eq!(expand("a ## b #x #{unclosed", &vars), "a # b #x #{unclosed");
    }

    #[test]
    fn truthiness() {
        assert!(!truthy("") && !truthy("0") && truthy("1"));
    }
}
