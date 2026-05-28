use std::{
    fmt,
    io::{self, Read, Write},
    net::TcpStream,
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_millis(200);

// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum Error {
    InvalidFrame,
    InvalidUtf8,
    UnsupportedOpcode(u8),
}

impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidFrame => write!(f, "Invalid WebSocket frame"),
            Error::InvalidUtf8 => write!(f, "Invalid UTF-8 in WebSocket message"),
            Error::UnsupportedOpcode(op) => write!(f, "Unsupported opcode: 0x{:x}", op),
        }
    }
}

impl From<Error> for io::Error {
    fn from(err: Error) -> Self {
        io::Error::other(err)
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

impl TryFrom<u8> for Opcode {
    type Error = Error;

    fn try_from(byte: u8) -> Result<Self, Self::Error> {
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

fn read_frame(stream: &mut TcpStream) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 2];
    match stream.read_exact(&mut head) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    }

    let fin = head[0] & 0x80 != 0;
    let opcode = (head[0] & 0x0F).try_into()?;
    let masked = head[1] & 0x80 != 0;
    let mut payload_len = (head[1] & 0x7F) as usize;

    if payload_len > 125 && !matches!(opcode, Opcode::Text | Opcode::Binary) {
        return Err(Error::InvalidFrame.into());
    }

    if payload_len == 126 {
        let mut buf = [0u8; 2];
        stream.read_exact(&mut buf)?;
        payload_len = u16::from_be_bytes(buf) as usize;
    } else if payload_len == 127 {
        let mut buf = [0u8; 8];
        stream.read_exact(&mut buf)?;
        payload_len = u64::from_be_bytes(buf) as usize;
    }

    let masking_key = if masked {
        let mut key = [0u8; 4];
        stream.read_exact(&mut key)?;
        Some(key)
    } else {
        None
    };

    let mut data = vec![0u8; payload_len];
    stream.read_exact(&mut data)?;

    if let Some(key) = masking_key {
        for i in 0..payload_len {
            data[i] ^= key[i % 4];
        }
    }

    Ok(Some(Frame { fin, opcode, data }))
}

fn send_frame(stream: &mut TcpStream, data: &[u8], opcode: Opcode, fin: bool) -> io::Result<()> {
    let mut header = Vec::with_capacity(10);

    let byte1 = (if fin { 0x80 } else { 0x00 }) | (opcode as u8 & 0x0F);
    header.push(byte1);

    if data.len() < 126 {
        header.push(data.len() as u8);
    } else if data.len() <= 65535 {
        header.push(126);
        header.extend_from_slice(&(data.len() as u16).to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(&(data.len() as u64).to_be_bytes());
    }

    stream.write_all(&header)?;
    stream.write_all(data)?;
    stream.flush()?;

    Ok(())
}

// ----------------------------------------------------------------------------

#[derive(Debug)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Close,
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    None,
}

fn read_message(stream: &mut TcpStream) -> io::Result<Message> {
    let frame = read_frame(stream)?;
    let Some(frame) = frame else {
        return Ok(Message::None);
    };

    match frame.opcode {
        Opcode::Close => return Ok(Message::Close),
        Opcode::Ping => return Ok(Message::Ping(frame.data)),
        Opcode::Pong => return Ok(Message::Pong(frame.data)),
        Opcode::Continuation => return Err(Error::InvalidFrame.into()),
        Opcode::Text | Opcode::Binary => {}
    }

    let opcode = frame.opcode;
    let mut frame = frame;
    let mut data = std::mem::take(&mut frame.data);

    while !frame.fin {
        match read_frame(stream)? {
            Some(new_frame) if new_frame.opcode == Opcode::Continuation => frame = new_frame,
            _ => return Err(Error::InvalidFrame.into()),
        }
        data.extend_from_slice(&frame.data);
    }

    if opcode == Opcode::Text {
        let text = String::from_utf8(data).map_err(|_| Error::InvalidUtf8)?;
        Ok(Message::Text(text))
    } else {
        Ok(Message::Binary(data))
    }
}

fn send_message(stream: &mut TcpStream, msg: Message) -> io::Result<()> {
    match msg {
        Message::Text(text) => send_frame(stream, text.as_bytes(), Opcode::Text, true),
        Message::Binary(data) => send_frame(stream, &data, Opcode::Binary, true),
        Message::Close => send_frame(stream, &[], Opcode::Close, true),
        Message::Ping(data) => send_frame(stream, &data, Opcode::Ping, true),
        Message::Pong(data) => send_frame(stream, &data, Opcode::Pong, true),
        Message::None => Ok(()),
    }
}

// ----------------------------------------------------------------------------

pub struct WebSocket {
    inner: TcpStream,
}

impl WebSocket {
    pub(crate) fn new(mut stream: TcpStream, key: &str) -> io::Result<Self> {
        stream.set_read_timeout(Some(TIMEOUT))?;
        accept_connection(&mut stream, key)?;
        Ok(WebSocket { inner: stream })
    }

    pub fn send(&mut self, msg: Message) -> io::Result<()> {
        send_message(&mut self.inner, msg)
    }

    pub fn recv(&mut self) -> io::Result<Message> {
        read_message(&mut self.inner)
    }
}

pub(crate) fn compute_accept(sec_websocket_key: &str) -> String {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

    let mut key = String::with_capacity(sec_websocket_key.len() + GUID.len());
    key.push_str(sec_websocket_key);
    key.push_str(GUID);

    let hash = crate::sha1::sha1(key.as_bytes());
    crate::base64::encode(&hash)
}

#[test]
fn test_compute_accept() {
    // Example from RFC 6455, Section 1.3
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let expected = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
    assert_eq!(compute_accept(key), expected);
}

fn accept_connection(stream: &mut TcpStream, key: &str) -> io::Result<()> {
    let accept = compute_accept(key);

    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept
    );

    stream.write_all(response.as_bytes())?;

    Ok(())
}
