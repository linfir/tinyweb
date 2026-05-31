use std::{
    io::{self, BufWriter, Write},
    net::TcpStream,
};

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

pub(crate) fn send_sse_headers(stream: &mut TcpStream) -> io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\n\
          Content-Type: text/event-stream\r\n\
          Cache-Control: no-cache\r\n\
          Connection: close\r\n\
          \r\n",
    )
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
        if data.is_empty() {
            writeln!(self.inner, "data:")?;
        } else {
            for line in data.lines() {
                writeln!(self.inner, "data:{}", line)?;
            }
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
        for line in data.lines() {
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
