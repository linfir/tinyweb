use std::{
    io::{self, BufWriter, Write},
    net::TcpStream,
};

use crate::date::Date;

/// A Server-Sent Events response.
pub struct SseResponse(pub(crate) Box<dyn FnOnce(&mut SseWriter) + Send + 'static>);

impl SseResponse {
    /// `handler` is called synchronously on the connection thread; the connection closes when it returns.
    pub fn new<F>(handler: F) -> Self
    where
        F: FnOnce(&mut SseWriter) + Send + 'static,
    {
        SseResponse(Box::new(handler))
    }
}

pub(crate) fn send_sse_headers(stream: &mut TcpStream, date: &Date) -> io::Result<()> {
    let mut w = io::BufWriter::new(stream);
    write!(w, "HTTP/1.1 200 OK\r\n")?;
    write!(w, "Date: {}\r\n", date.http())?;
    write!(w, "Content-Type: text/event-stream\r\n")?;
    write!(w, "Cache-Control: no-cache\r\n")?;
    write!(w, "Connection: close\r\n")?;
    write!(w, "\r\n")?;
    w.flush()
}

/// Writes Server-Sent Events to an open connection.
pub struct SseWriter {
    inner: BufWriter<TcpStream>,
}

impl SseWriter {
    pub(crate) fn new(stream: TcpStream) -> Self {
        SseWriter {
            inner: BufWriter::new(stream),
        }
    }

    /// Sends an unnamed event.
    pub fn send(&mut self, data: &str) -> io::Result<()> {
        for line in lines_lf(data) {
            writeln!(self.inner, "data:{}", line)?;
        }
        writeln!(self.inner)?;
        self.inner.flush()
    }

    /// Sends a named event.
    pub fn send_event(&mut self, event: &str, data: &str) -> io::Result<()> {
        if event.contains(['\n', '\r']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSE event name must not contain CR or LF",
            ));
        }
        writeln!(self.inner, "event:{}", event)?;
        for line in lines_lf(data) {
            writeln!(self.inner, "data:{}", line)?;
        }
        writeln!(self.inner)?;
        self.inner.flush()
    }

    /// Sends a comment line.
    pub fn keepalive(&mut self) -> io::Result<()> {
        write!(self.inner, ":\n\n")?;
        self.inner.flush()
    }
}

/// Split `s` into lines on `\n`, stripping trailing `\r` from each segment.
///
/// Unlike [`str::lines`], this always yields at least one segment (empty string
/// gives `[""]`), and a trailing `\n` produces a final empty segment -- so the
/// round-trip through SSE `data:` fields is lossless.
fn lines_lf(s: &str) -> impl Iterator<Item = &str> {
    s.split('\n').map(|l| l.trim_end_matches('\r'))
}

#[test]
fn test_lines_lf() {
    let v = |s| lines_lf(s).collect::<Vec<_>>();
    // str::lines() yields nothing; lines_lf yields one empty segment
    assert_eq!(v(""), [""]);
    // no newline: single segment
    assert_eq!(v("hello"), ["hello"]);
    // trailing \n: extra empty segment (unlike str::lines)
    assert_eq!(v("hello\n"), ["hello", ""]);
    // internal newline
    assert_eq!(v("hello\nworld"), ["hello", "world"]);
    // CRLF: \r stripped from each segment
    assert_eq!(v("hello\r\nworld\r\n"), ["hello", "world", ""]);
}
