//! JSONL framing for pi's RPC mode (architecture §4.2).
//!
//! pi's docs are unusually specific here, because getting it wrong corrupts
//! messages rather than failing loudly:
//!
//! > RPC mode uses strict JSONL semantics with LF (`\n`) as the only record
//! > delimiter. Split records on `\n` only. Accept optional `\r\n` input by
//! > stripping a trailing `\r`. Do not use generic line readers that treat
//! > Unicode separators as newlines.
//!
//! The named hazard is Node's `readline`, which also splits on `U+2028` /
//! `U+2029` — both of which are perfectly legal *inside* a JSON string, so a
//! model emitting a line-separator character would silently split one record
//! into two unparseable halves.
//!
//! Rust's `BufRead::read_line` splits on `\n` only, so it is compliant — but
//! that is a property worth pinning with a test rather than assuming, since a
//! future refactor to something "nicer" could reintroduce the bug.

use std::io::BufRead;

/// Read one framed record, or `None` at end of stream.
///
/// Returns the record with its trailing `\n` and any single trailing `\r`
/// removed. Empty lines are skipped rather than returned as empty records.
pub fn read_record<R: BufRead>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut buf = String::new();
    loop {
        buf.clear();
        if reader.read_line(&mut buf)? == 0 {
            return Ok(None);
        }
        let record = strip_terminator(&buf);
        if record.is_empty() {
            continue; // blank keep-alive line
        }
        return Ok(Some(record.to_string()));
    }
}

/// Strip the record terminator: one `\n`, plus one `\r` before it if present.
///
/// Deliberately does *not* use `trim_end`, which would also eat `U+2028`,
/// `U+2029`, and other Unicode whitespace that may be meaningful payload.
pub fn strip_terminator(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// Split a whole buffer into records, LF only.
///
/// Used by the conformance tests to replay a recorded stream.
pub fn split_records(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(strip_terminator)
        .filter(|r| !r.is_empty())
        .collect()
}
