use std::{
    fmt,
    io::{self, Read, Write},
    net::TcpStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{date::Date, generated::Method, request::Request};

// How often recv() wakes up to check for a graceful shutdown.
const POLL_INTERVAL: Duration = Duration::from_millis(200);
// How long close() waits for the peer's Close echo.
const CLOSE_DRAIN: Duration = Duration::from_secs(1);

impl Request {
    /// Returns `true` if this request asks for a WebSocket upgrade (RFC 6455).
    pub fn upgradable(&self) -> bool {
        self.method == Method::GET
            && self
                .headers
                .get("upgrade")
                .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
            && self.headers.get("connection").is_some_and(|v| {
                v.split(',')
                    .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
            })
            && self
                .headers
                .get("sec-websocket-version")
                .is_some_and(|v| v == "13")
            && self.headers.contains_key("sec-websocket-key")
    }
}

/// A WebSocket upgrade response.
pub struct WsResponse(pub(crate) Box<dyn FnOnce(&mut WebSocket) + Send + 'static>);

impl WsResponse {
    /// `handler` is called synchronously on the connection thread after the
    /// 101 handshake; the connection closes when it returns.
    /// If the request is not [`Request::upgradable`], a
    /// [`crate::StatusCode::BadRequest`] response is sent instead.
    pub fn new<F>(handler: F) -> Self
    where
        F: FnOnce(&mut WebSocket) + Send + 'static,
    {
        WsResponse(Box::new(handler))
    }
}

// ----------------------------------------------------------------------------

#[derive(Debug)]
enum Error {
    Io(io::Error),
    InvalidFrame,
    Unmasked,
    InvalidUtf8,
    UnsupportedOpcode(u8),
    TooLarge,
}

impl Error {
    // RFC 6455 s7.4.1 close status codes.
    fn close_code(&self) -> Option<u16> {
        match self {
            Error::Io(_) => None,
            Error::InvalidFrame | Error::Unmasked | Error::UnsupportedOpcode(_) => Some(1002),
            Error::InvalidUtf8 => Some(1007),
            Error::TooLarge => Some(1009),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{}", e),
            Error::InvalidFrame => write!(f, "Invalid WebSocket frame"),
            Error::Unmasked => write!(f, "Unmasked WebSocket client frame"),
            Error::InvalidUtf8 => write!(f, "Invalid UTF-8 in WebSocket message"),
            Error::UnsupportedOpcode(op) => write!(f, "Unsupported opcode: 0x{:x}", op),
            Error::TooLarge => write!(f, "WebSocket message too large"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(e) => e,
            other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
        }
    }
}

// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opcode {
    Continuation = 0x0,
    Text = 0x1,
    Binary = 0x2,
    Close = 0x8,
    Ping = 0x9,
    Pong = 0xA,
}

impl Opcode {
    fn is_control(self) -> bool {
        matches!(self, Opcode::Close | Opcode::Ping | Opcode::Pong)
    }
}

impl TryFrom<u8> for Opcode {
    type Error = Error;

    fn try_from(byte: u8) -> Result<Self, Error> {
        match byte {
            0x0 => Ok(Opcode::Continuation),
            0x1 => Ok(Opcode::Text),
            0x2 => Ok(Opcode::Binary),
            0x8 => Ok(Opcode::Close),
            0x9 => Ok(Opcode::Ping),
            0xA => Ok(Opcode::Pong),
            other => Err(Error::UnsupportedOpcode(other)),
        }
    }
}

// ----------------------------------------------------------------------------

#[derive(Debug)]
struct Frame {
    fin: bool,
    opcode: Opcode,
    data: Vec<u8>,
}

// Reads one client frame; blocking, timeouts are the reader's concern.
#[cfg(test)]
fn read_frame(r: &mut impl Read, max_len: usize) -> Result<Frame, Error> {
    let mut b0 = [0u8; 1];
    r.read_exact(&mut b0)?;
    read_frame_rest(b0[0], r, max_len)
}

// Reads the remainder of a client frame after its first byte.
// Enforces RFC 6455 rules: no RSV bits (no extension is negotiated), client
// frames must be masked, control frames must be short and unfragmented,
// lengths must use the minimal encoding, data frames must fit in `max_len`.
fn read_frame_rest(b0: u8, r: &mut impl Read, max_len: usize) -> Result<Frame, Error> {
    if b0 & 0x70 != 0 {
        return Err(Error::InvalidFrame);
    }
    let fin = b0 & 0x80 != 0;
    let opcode: Opcode = (b0 & 0x0F).try_into()?;

    let mut b1 = [0u8; 1];
    r.read_exact(&mut b1)?;
    // Clients MUST mask their frames (RFC 6455 s5.1).
    if b1[0] & 0x80 == 0 {
        return Err(Error::Unmasked);
    }
    let short_len = (b1[0] & 0x7F) as usize;

    if opcode.is_control() && (short_len > 125 || !fin) {
        return Err(Error::InvalidFrame);
    }

    let payload_len = match short_len {
        126 => {
            let mut buf = [0u8; 2];
            r.read_exact(&mut buf)?;
            let n = u16::from_be_bytes(buf) as usize;
            if n < 126 {
                return Err(Error::InvalidFrame);
            }
            n
        }
        127 => {
            let mut buf = [0u8; 8];
            r.read_exact(&mut buf)?;
            let n = u64::from_be_bytes(buf);
            if n & (1 << 63) != 0 || n <= u16::MAX as u64 {
                return Err(Error::InvalidFrame);
            }
            usize::try_from(n).map_err(|_| Error::TooLarge)?
        }
        n => n,
    };

    if !opcode.is_control() && payload_len > max_len {
        return Err(Error::TooLarge);
    }

    let mut key = [0u8; 4];
    r.read_exact(&mut key)?;

    let mut data = vec![0u8; payload_len];
    r.read_exact(&mut data)?;
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= key[i % 4];
    }

    Ok(Frame { fin, opcode, data })
}

// Sends one (unmasked) server frame.
fn send_frame(w: &mut impl Write, opcode: Opcode, data: &[u8], fin: bool) -> io::Result<()> {
    let mut header = Vec::with_capacity(10);

    let byte1 = (if fin { 0x80 } else { 0x00 }) | (opcode as u8 & 0x0F);
    header.push(byte1);

    if data.len() < 126 {
        header.push(data.len() as u8);
    } else if data.len() <= u16::MAX as usize {
        header.push(126);
        header.extend_from_slice(&(data.len() as u16).to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(&(data.len() as u64).to_be_bytes());
    }

    w.write_all(&header)?;
    w.write_all(data)?;
    w.flush()?;

    Ok(())
}

// Retries reads that time out until `deadline`, so a frame arriving in
// pieces is not mistaken for a dead connection.
struct DeadlineReader<'a> {
    stream: &'a TcpStream,
    deadline: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "WebSocket frame read timed out",
                ));
            }
            self.stream.set_read_timeout(Some(remaining))?;
            let mut s = self.stream;
            match s.read(buf) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                r => return r,
            }
        }
    }
}

// ----------------------------------------------------------------------------

/// A WebSocket data message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
}

/// The result of [`WebSocket::recv_timeout`].
#[derive(Debug)]
pub enum Recv {
    /// A data message arrived.
    Message(Message),
    /// The connection is closed.
    Closed,
    /// No message arrived within the timeout.
    Timeout,
}

/// Sends and receives messages on an upgraded connection.
///
/// Ping frames are answered automatically; fragmented messages are
/// reassembled.
/// A message larger than [`crate::Config::max_ws_message_size`] closes the
/// connection with status 1009.
pub struct WebSocket {
    stream: Arc<TcpStream>,
    shutdown: Arc<AtomicBool>,
    frame_timeout: Duration,
    max_message_size: usize,
    closed: bool,
}

impl WebSocket {
    pub(crate) fn new(
        stream: Arc<TcpStream>,
        shutdown: Arc<AtomicBool>,
        frame_timeout: Duration,
        max_message_size: usize,
    ) -> Self {
        WebSocket {
            stream,
            shutdown,
            frame_timeout,
            max_message_size,
            closed: false,
        }
    }

    /// Returns `true` once the server has begun a graceful shutdown.
    /// [`WebSocket::recv`] already checks this; poll it from loops built on
    /// [`WebSocket::recv_timeout`].
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    /// Sends a data message.
    pub fn send(&mut self, msg: Message) -> io::Result<()> {
        if self.closed {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "WebSocket is closed",
            ));
        }
        let mut s = &*self.stream;
        match msg {
            Message::Text(t) => send_frame(&mut s, Opcode::Text, t.as_bytes(), true),
            Message::Binary(d) => send_frame(&mut s, Opcode::Binary, &d, true),
        }
    }

    /// Waits for the next data message.
    /// Returns `None` when the connection is closed -- by the peer, or by
    /// this server starting a graceful shutdown (a Close with status 1001 is
    /// sent first).
    pub fn recv(&mut self) -> io::Result<Option<Message>> {
        loop {
            if self.is_shutdown() && !self.closed {
                let _ = self.close_with(1001);
                return Ok(None);
            }
            match self.recv_timeout(POLL_INTERVAL)? {
                Recv::Message(m) => return Ok(Some(m)),
                Recv::Closed => return Ok(None),
                Recv::Timeout => {}
            }
        }
    }

    /// Waits up to `timeout` for the next data message.
    pub fn recv_timeout(&mut self, timeout: Duration) -> io::Result<Recv> {
        if self.closed {
            return Ok(Recv::Closed);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(Recv::Timeout);
            }
            let frame = match self.next_frame(remaining) {
                Ok(None) => return Ok(Recv::Timeout),
                Ok(Some(f)) => f,
                Err(e) => return Err(self.fail(e)),
            };
            match frame.opcode {
                Opcode::Ping => {
                    if let Err(e) = self.send_pong(&frame.data) {
                        return Err(self.fail(e));
                    }
                }
                Opcode::Pong => {}
                Opcode::Close => {
                    if let Err(e) = self.finish_close(&frame.data) {
                        return Err(self.fail(e));
                    }
                    return Ok(Recv::Closed);
                }
                Opcode::Continuation => return Err(self.fail(Error::InvalidFrame)),
                Opcode::Text | Opcode::Binary => match self.assemble(frame) {
                    Ok(Some(msg)) => return Ok(Recv::Message(msg)),
                    Ok(None) => return Ok(Recv::Closed),
                    Err(e) => return Err(self.fail(e)),
                },
            }
        }
    }

    /// Closes the connection: sends a Close frame (status 1000) and waits
    /// briefly for the peer's echo.
    /// Called automatically when the handler returns.
    pub fn close(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.close_with(1000)
    }

    // Reassembles a fragmented message starting at `first`.
    // Control frames interleaved between fragments are handled here; `None`
    // means the peer closed mid-message.
    fn assemble(&mut self, first: Frame) -> Result<Option<Message>, Error> {
        let text = first.opcode == Opcode::Text;
        let mut fin = first.fin;
        let mut data = first.data;
        while !fin {
            let Some(frame) = self.next_frame(self.frame_timeout)? else {
                return Err(Error::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "WebSocket fragment timed out",
                )));
            };
            match frame.opcode {
                Opcode::Ping => self.send_pong(&frame.data)?,
                Opcode::Pong => {}
                Opcode::Close => {
                    self.finish_close(&frame.data)?;
                    return Ok(None);
                }
                Opcode::Continuation => {
                    if data.len() + frame.data.len() > self.max_message_size {
                        return Err(Error::TooLarge);
                    }
                    data.extend_from_slice(&frame.data);
                    fin = frame.fin;
                }
                Opcode::Text | Opcode::Binary => return Err(Error::InvalidFrame),
            }
        }
        if text {
            match String::from_utf8(data) {
                Ok(t) => Ok(Some(Message::Text(t))),
                Err(_) => Err(Error::InvalidUtf8),
            }
        } else {
            Ok(Some(Message::Binary(data)))
        }
    }

    // Waits up to `wait` for the start of a frame, then reads the rest of it
    // within `frame_timeout`.
    fn next_frame(&mut self, wait: Duration) -> Result<Option<Frame>, Error> {
        let stream = &*self.stream;
        stream.set_read_timeout(Some(wait))?;
        let mut b0 = [0u8; 1];
        let mut s = stream;
        match s.read_exact(&mut b0) {
            Ok(()) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(Error::Io(e)),
        }
        let mut r = DeadlineReader {
            stream,
            deadline: Instant::now() + self.frame_timeout,
        };
        read_frame_rest(b0[0], &mut r, self.max_message_size).map(Some)
    }

    // Best-effort Close with the code matching the error, then poison.
    fn fail(&mut self, e: Error) -> io::Error {
        if let Some(code) = e.close_code()
            && !self.closed
        {
            let _ = self.send_close(code);
            self.discard_incoming();
        }
        self.closed = true;
        e.into()
    }

    // Reads and discards whatever the client still has in flight, so the
    // socket closes with FIN (not RST) and the Close frame reaches the
    // client.  The stream may be mid-frame here, so bytes, not frames.
    fn discard_incoming(&mut self) {
        let deadline = Instant::now() + CLOSE_DRAIN;
        let mut buf = [0u8; 4096];
        let stream = &*self.stream;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
                return;
            }
            let mut s = stream;
            match s.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    }

    fn send_close(&mut self, code: u16) -> io::Result<()> {
        let mut s = &*self.stream;
        send_frame(&mut s, Opcode::Close, &code.to_be_bytes(), true)
    }

    fn send_pong(&mut self, data: &[u8]) -> Result<(), Error> {
        let mut s = &*self.stream;
        send_frame(&mut s, Opcode::Pong, data, true).map_err(Error::Io)
    }

    // The peer sent Close first: echo its status code back.
    fn finish_close(&mut self, payload: &[u8]) -> Result<(), Error> {
        let code = match payload.len() {
            0 => 1000,
            1 => return Err(Error::InvalidFrame),
            _ => u16::from_be_bytes([payload[0], payload[1]]),
        };
        if !self.closed {
            self.send_close(code)?;
        }
        self.closed = true;
        Ok(())
    }

    // This side closes first: send Close, then drain until the peer echoes.
    fn close_with(&mut self, code: u16) -> io::Result<()> {
        self.send_close(code)?;
        self.closed = true;
        let deadline = Instant::now() + CLOSE_DRAIN;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.next_frame(remaining) {
                Ok(Some(f)) if f.opcode == Opcode::Close => break,
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => break,
            }
        }
        Ok(())
    }
}

// ----------------------------------------------------------------------------

pub(crate) fn compute_accept(sec_websocket_key: &str) -> String {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

    let mut key = String::with_capacity(sec_websocket_key.len() + GUID.len());
    key.push_str(sec_websocket_key);
    key.push_str(GUID);

    let hash = crate::sha1::sha1(key.as_bytes());
    crate::base64::encode(&hash)
}

pub(crate) fn send_upgrade_headers(stream: &TcpStream, key: &str, date: &Date) -> io::Result<()> {
    let mut w = io::BufWriter::new(stream);
    write!(w, "HTTP/1.1 101 Switching Protocols\r\n")?;
    write!(w, "Date: {}\r\n", date.http())?;
    write!(w, "Upgrade: websocket\r\n")?;
    write!(w, "Connection: Upgrade\r\n")?;
    write!(w, "Sec-WebSocket-Accept: {}\r\n", compute_accept(key))?;
    write!(w, "\r\n")?;
    w.flush()
}

// ----------------------------------------------------------------------------

#[test]
fn test_compute_accept() {
    // Example from RFC 6455, Section 1.3
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let expected = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
    assert_eq!(compute_accept(key), expected);
}

// Builds a masked client frame for tests.
#[cfg(test)]
fn client_frame(b0: u8, payload: &[u8]) -> Vec<u8> {
    let key = [7u8, 42, 13, 200];
    let mut f = vec![b0];
    if payload.len() < 126 {
        f.push(0x80 | payload.len() as u8);
    } else if payload.len() <= u16::MAX as usize {
        f.push(0x80 | 126);
        f.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        f.push(0x80 | 127);
        f.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    f.extend_from_slice(&key);
    f.extend(payload.iter().enumerate().map(|(i, &b)| b ^ key[i % 4]));
    f
}

#[cfg(test)]
const TEST_MAX: usize = 1 << 20;

#[test]
fn test_read_frame_roundtrip() {
    for len in [0, 5, 125, 126, 1000, 65535, 65536, 100_000] {
        let payload = vec![0xABu8; len];
        let bytes = client_frame(0x82, &payload);
        let frame = read_frame(&mut &bytes[..], TEST_MAX).unwrap();
        assert!(frame.fin);
        assert_eq!(frame.opcode, Opcode::Binary);
        assert_eq!(frame.data, payload, "len {}", len);
    }
}

#[test]
fn test_read_frame_unmasks() {
    let bytes = client_frame(0x81, b"hello");
    let frame = read_frame(&mut &bytes[..], TEST_MAX).unwrap();
    assert_eq!(frame.data, b"hello");
    // The wire bytes must not contain the cleartext.
    assert!(!bytes.windows(5).any(|w| w == b"hello"));
}

#[test]
fn test_rejects_unmasked_frame() {
    let bytes = [0x81, 0x05, b'h', b'e', b'l', b'l', b'o'];
    assert!(matches!(
        read_frame(&mut &bytes[..], TEST_MAX),
        Err(Error::Unmasked)
    ));
}

#[test]
fn test_rejects_rsv_bits() {
    for rsv in [0x10, 0x20, 0x40] {
        let bytes = client_frame(0x81 | rsv, b"x");
        assert!(matches!(
            read_frame(&mut &bytes[..], TEST_MAX),
            Err(Error::InvalidFrame)
        ));
    }
}

#[test]
fn test_rejects_unsupported_opcode() {
    let bytes = client_frame(0x83, b"x");
    assert!(matches!(
        read_frame(&mut &bytes[..], TEST_MAX),
        Err(Error::UnsupportedOpcode(0x3))
    ));
}

#[test]
fn test_rejects_oversized_control_frame() {
    let bytes = client_frame(0x89, &[0u8; 126]); // ping, 126 bytes
    assert!(matches!(
        read_frame(&mut &bytes[..], TEST_MAX),
        Err(Error::InvalidFrame)
    ));
}

#[test]
fn test_rejects_fragmented_control_frame() {
    let bytes = client_frame(0x08, b"x"); // close without fin
    assert!(matches!(
        read_frame(&mut &bytes[..], TEST_MAX),
        Err(Error::InvalidFrame)
    ));
}

#[test]
fn test_allows_large_continuation_frame() {
    let bytes = client_frame(0x80, &[0u8; 1000]); // continuation, fin, 1000 bytes
    let frame = read_frame(&mut &bytes[..], TEST_MAX).unwrap();
    assert_eq!(frame.opcode, Opcode::Continuation);
    assert_eq!(frame.data.len(), 1000);
}

#[test]
fn test_rejects_nonminimal_length() {
    // 5-byte payload wrongly sent with the 2-byte length form.
    let mut bytes = vec![0x81, 0x80 | 126, 0, 5];
    bytes.extend_from_slice(&[7, 42, 13, 200]);
    bytes.extend(
        b"hello"
            .iter()
            .enumerate()
            .map(|(i, &b)| b ^ [7u8, 42, 13, 200][i % 4]),
    );
    assert!(matches!(
        read_frame(&mut &bytes[..], TEST_MAX),
        Err(Error::InvalidFrame)
    ));

    // 300-byte payload wrongly sent with the 8-byte length form.
    let mut bytes = vec![0x81, 0x80 | 127];
    bytes.extend_from_slice(&300u64.to_be_bytes());
    assert!(matches!(
        read_frame(&mut &bytes[..], TEST_MAX),
        Err(Error::InvalidFrame)
    ));
}

#[test]
fn test_rejects_length_msb() {
    let mut bytes = vec![0x81, 0x80 | 127];
    bytes.extend_from_slice(&(1u64 << 63 | 70000).to_be_bytes());
    assert!(matches!(
        read_frame(&mut &bytes[..], TEST_MAX),
        Err(Error::InvalidFrame)
    ));
}

#[test]
fn test_rejects_too_large_frame() {
    let bytes = client_frame(0x81, &[0u8; 100]);
    assert!(matches!(
        read_frame(&mut &bytes[..], 64),
        Err(Error::TooLarge)
    ));
}

#[test]
fn test_send_frame_lengths() {
    for (len, header_len) in [(5usize, 2usize), (126, 4), (65535, 4), (65536, 10)] {
        let mut out = Vec::new();
        send_frame(&mut out, Opcode::Binary, &vec![0u8; len], true).unwrap();
        assert_eq!(out.len(), header_len + len, "payload len {}", len);
        assert_eq!(out[0], 0x82);
        assert_eq!(out[1] & 0x80, 0, "server frames are unmasked");
    }
}
