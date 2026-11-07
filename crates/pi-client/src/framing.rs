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

use std::io::{BufRead, Read};

/// Largest record accepted, in bytes. Generous — a record can carry a whole
/// tool result — but finite: `read_line` alone grows its buffer for as long as
/// the other side keeps sending without a newline, and running out of memory
/// aborts the whole app rather than just this session.
pub const MAX_RECORD: usize = 32 * 1024 * 1024;

/// Read one framed record, or `None` at end of stream.
///
/// Returns the record with its trailing `\n` and any single trailing `\r`
/// removed. Empty lines are skipped rather than returned as empty records, and
/// so are records over [`MAX_RECORD`]: like unparseable JSON, an oversized
/// record is a protocol violation, not a reason to end the session.
pub fn read_record<R: BufRead>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader
            .by_ref()
            .take(MAX_RECORD as u64 + 1)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(None);
        }
        if buf.len() > MAX_RECORD && buf.last() != Some(&b'\n') {
            skip_past_newline(reader)?;
            continue;
        }
        let text = String::from_utf8(std::mem::take(&mut buf))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let record = strip_terminator(&text);
        if record.is_empty() {
            continue; // blank keep-alive line
        }
        return Ok(Some(record.to_string()));
    }
}

/// Discard input up to and including the next `\n`, without buffering it.
fn skip_past_newline<R: BufRead>(reader: &mut R) -> std::io::Result<()> {
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(());
        }
        if let Some(i) = chunk.iter().position(|&b| b == b'\n') {
            reader.consume(i + 1);
            return Ok(());
        }
        let len = chunk.len();
        reader.consume(len);
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

#[cfg(test)]
mod size_tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn an_oversized_record_is_skipped_and_the_next_one_still_arrives() {
        let mut input = vec![b'x'; MAX_RECORD + 10];
        input.extend_from_slice(b"\n{\"ok\":true}\n");
        let mut reader = BufReader::new(Cursor::new(input));
        assert_eq!(read_record(&mut reader).unwrap().as_deref(), Some(r#"{"ok":true}"#));
        assert!(read_record(&mut reader).unwrap().is_none());
    }
}
