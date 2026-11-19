//! A minimal server-sent-events client over `std::net::TcpStream`.
//!
//! Deliberately hand-rolled rather than pulling `tokio`/`hyper`/`reqwest` into
//! a headless crate: the only thing we ever talk to is an agent's **local**
//! HTTP server on loopback (OpenCode's `GET /event`), which is a far smaller
//! problem than the general case. This matches the `forge` crate's discipline
//! of not taking a dependency for something small and well-understood.
//!
//! Handles what a real server actually does: HTTP/1.1 response headers,
//! `Transfer-Encoding: chunked` (which is how a streaming endpoint normally
//! responds), multi-line `data:` payloads, and `event:` names.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// One decoded SSE frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    /// The `event:` field, when the server names the event type.
    pub event: Option<String>,
    /// The `data:` payload, with multiple `data:` lines joined by `\n`.
    pub data: String,
}

/// What the body is read through once the headers are consumed: whatever the
/// header reader had already buffered, followed by the rest of the socket.
type Body = io::Chain<io::Cursor<Vec<u8>>, TcpStream>;

/// A connected SSE stream. Iterate it to receive frames; iteration ends when
/// the server closes the connection.
pub struct SseStream {
    reader: BufReader<ChunkedReader<Body>>,
}

/// Longest status, header, or chunk-size line accepted.
const MAX_HEADER_LINE: usize = 8 * 1024;
/// Most header lines accepted before the body.
const MAX_HEADERS: usize = 100;
/// Largest single event (all its lines together) accepted.
const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// One line of at most `max` bytes, including its `\n`. `Ok(Some(""))` at end
/// of stream, `Ok(None)` when the line is longer than `max`.
fn read_line_capped<R: BufRead>(reader: &mut R, max: usize) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    reader
        .by_ref()
        .take(max as u64 + 1)
        .read_until(b'\n', &mut buf)?;
    if buf.len() > max {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

impl SseStream {
    /// Open `GET <path>` on `host:port` and start streaming events.
    pub fn connect(host: &str, port: u16, path: &str) -> io::Result<SseStream> {
        let mut stream = TcpStream::connect((host, port))?;
        // Long read timeout, not none: a wedged server must not pin a thread
        // forever, but ordinary quiet periods between events are expected.
        stream.set_read_timeout(Some(Duration::from_secs(300)))?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: text/event-stream\r\n\
             Cache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
        )?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut chunked = false;
        let status_line = read_line_capped(&mut reader, MAX_HEADER_LINE)?
            .ok_or_else(|| io::Error::other("SSE status line too long"))?;
        if !status_line.contains(" 200") {
            return Err(io::Error::other(format!(
                "SSE endpoint returned {:?}",
                status_line.trim()
            )));
        }
        let mut header_lines = 0usize;
        loop {
            header_lines += 1;
            if header_lines > MAX_HEADERS {
                return Err(io::Error::other("too many SSE headers"));
            }
            let line = read_line_capped(&mut reader, MAX_HEADER_LINE)?
                .ok_or_else(|| io::Error::other("SSE header too long"))?;
            if line.is_empty() {
                return Err(io::Error::other("connection closed during headers"));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break; // end of headers
            }
            let lower = trimmed.to_ascii_lowercase();
            if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
                chunked = true;
            }
        }

        // `into_inner()` alone would DISCARD whatever the header reader had
        // already buffered — which for a streaming endpoint routinely includes
        // the first event, since the server may flush headers and data in one
        // packet. Carry the buffer forward and chain it ahead of the socket.
        let leftover = reader.buffer().to_vec();
        let socket = reader.into_inner();
        let body = io::Cursor::new(leftover).chain(socket);
        Ok(SseStream {
            reader: BufReader::new(ChunkedReader::new(BufReader::new(body), chunked)),
        })
    }
}

impl Iterator for SseStream {
    type Item = SseEvent;

    fn next(&mut self) -> Option<SseEvent> {
        let mut event = SseEvent::default();
        let mut data_lines: Vec<String> = Vec::new();
        let mut data_bytes = 0usize;
        loop {
            // Bounded per line and per event: the stream comes from a local
            // server, but an unbounded read is an app-wide crash waiting for
            // one misbehaving peer. An over-limit stream is ended, the same as
            // a closed one.
            let line = match read_line_capped(&mut self.reader, MAX_EVENT_BYTES) {
                Ok(Some(l)) if !l.is_empty() => l,
                _ => return None,
            };
            data_bytes += line.len();
            if data_bytes > MAX_EVENT_BYTES {
                return None;
            }
            let line = line.trim_end_matches(['\r', '\n']);

            if line.is_empty() {
                // Blank line dispatches the frame — but ignore keep-alive
                // blanks that carry no payload.
                if data_lines.is_empty() && event.event.is_none() {
                    continue;
                }
                event.data = data_lines.join("\n");
                return Some(event);
            }
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            } else if let Some(rest) = line.strip_prefix("event:") {
                event.event = Some(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // `id:`, `retry:`, and `:` comments are intentionally ignored.
        }
    }
}

/// Transparently un-chunks an HTTP/1.1 body when `Transfer-Encoding: chunked`.
struct ChunkedReader<R: Read> {
    inner: BufReader<R>,
    chunked: bool,
    /// Bytes left in the current chunk.
    remaining: usize,
    done: bool,
}

impl<R: Read> ChunkedReader<R> {
    fn new(inner: BufReader<R>, chunked: bool) -> Self {
        ChunkedReader {
            inner,
            chunked,
            remaining: 0,
            done: false,
        }
    }

    /// Read the `<hex>\r\n` size line that starts each chunk.
    fn start_chunk(&mut self) -> io::Result<()> {
        loop {
            let line = read_line_capped(&mut self.inner, MAX_HEADER_LINE)?
                .ok_or_else(|| io::Error::other("chunk size line too long"))?;
            if line.is_empty() {
                self.done = true;
                return Ok(());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue; // the CRLF terminating the previous chunk
            }
            // Chunk extensions after ';' are legal and ignorable.
            let size_hex = trimmed.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_hex, 16)
                .map_err(|_| io::Error::other(format!("bad chunk size {size_hex:?}")))?;
            if size == 0 {
                self.done = true;
            }
            self.remaining = size;
            return Ok(());
        }
    }
}

impl<R: Read> Read for ChunkedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.chunked {
            return self.inner.read(buf);
        }
        if self.done {
            return Ok(0);
        }
        if self.remaining == 0 {
            self.start_chunk()?;
            if self.done {
                return Ok(0);
            }
        }
        let want = buf.len().min(self.remaining);
        let read = self.inner.read(&mut buf[..want])?;
        self.remaining -= read;
        Ok(read)
    }
}
