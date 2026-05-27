use std::{
    io::{self, BufWriter, Write},
    net::TcpStream,
};

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
        for line in data.split('\n') {
            writeln!(self.inner, "data:{}", line)?;
        }
        writeln!(self.inner)?;
        self.inner.flush()
    }

    /// Sends a named event.
    pub fn send_event(&mut self, event: &str, data: &str) -> io::Result<()> {
        writeln!(self.inner, "event:{}", event)?;
        for line in data.split('\n') {
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
