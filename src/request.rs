use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Instant,
};

use crate::{
    enc,
    generated::{Method, StatusCode},
    server::Config,
};

/// An incoming HTTP request.
#[non_exhaustive]
pub struct Request {
    /// Unique request id (process-wide counter).
    /// Access log lines end with `#id`; include it in handler logs to correlate.
    pub id: u64,
    /// The HTTP method.
    pub method: Method,
    /// The percent-decoded request path (e.g. `/foo/bar`).
    pub path: String,
    /// Parsed query-string parameters, percent-decoded; `+` is treated as a space.
    /// If a key appears more than once, all values are collected in order.
    pub query: HashMap<String, Vec<String>>,
    /// Request headers.
    /// Keys are lowercased (e.g. `"content-type"`).
    pub headers: HashMap<String, String>,
    /// Raw request body bytes.
    /// Requests exceeding [`Config::max_body_size`] are rejected with [`StatusCode::ContentTooLarge`].
    pub body: Vec<u8>,
    /// Parsed `application/x-www-form-urlencoded` form fields.
    /// Keys and values are percent-decoded; `+` is treated as a space.
    /// Only populated when `Content-Type: application/x-www-form-urlencoded`.
    pub form: HashMap<String, Vec<String>>,
    /// The TCP peer address (i.e. the reverse proxy's address, not the client's).
    /// For the real client IP, read the `x-forwarded-for` or `x-real-ip` header.
    pub peer_addr: SocketAddr,
}

mod buf {
    pub struct Buf {
        data: Box<[u8]>,
        pos: usize,
    }

    impl Buf {
        pub fn new(size: usize) -> Self {
            Buf {
                data: vec![0; size].into_boxed_slice(),
                pos: 0,
            }
        }

        pub fn pos(&self) -> usize {
            self.pos
        }

        pub fn data(&self) -> &[u8] {
            &self.data[..self.pos]
        }

        pub fn data_up_to(&self, max: usize) -> &[u8] {
            &self.data[..self.pos.min(max)]
        }

        pub fn consume(&mut self, n: usize) {
            assert!(n <= self.pos);
            self.data.copy_within(n..self.pos, 0);
            self.pos -= n;
        }

        pub fn rest_mut(&mut self) -> &mut [u8] {
            &mut self.data[self.pos..]
        }

        pub fn advance(&mut self, n: usize) {
            assert!(self.pos + n <= self.data.len());
            self.pos += n;
        }
    }
}

use buf::Buf;

pub(crate) enum Error {
    Closed,
    Protocol(StatusCode),
}

type Result<T> = std::result::Result<T, Error>;

pub(crate) struct Reader<'a> {
    config: &'a Config,
    first: bool,
    peer_addr: SocketAddr,
    buf: Buf,
}

impl<'a> Reader<'a> {
    pub fn new(config: &'a Config, peer_addr: SocketAddr) -> Self {
        let buf_size = config.max_header_size.max(config.max_body_size);
        // FOR LATER: maybe buf_size should be bigger (and passed as a separate config)
        // to allow reading more data while processing the request

        Reader {
            config,
            first: true,
            peer_addr,
            buf: Buf::new(buf_size),
        }
    }

    pub(crate) fn read(&mut self, stream: &mut TcpStream, id: u64) -> Result<Request> {
        let (idx, deadline) = self.read_head(stream)?;

        let head = parse_head(&self.buf.data()[..idx + 2])
            .ok_or(Error::Protocol(StatusCode::BadRequest))?;

        if head.headers.contains_key("transfer-encoding") {
            return Err(Error::Protocol(StatusCode::NotImplemented));
        }

        let content_length: usize = match head.headers.get("content-length") {
            None => 0,
            // Strict 1*DIGIT: parse() alone would accept a leading '+',
            // which proxies may read differently (request smuggling).
            Some(v) if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) => {
                return Err(Error::Protocol(StatusCode::BadRequest));
            }
            Some(v) => match v.parse() {
                Ok(n) => n,
                Err(_) => return Err(Error::Protocol(StatusCode::BadRequest)),
            },
        };

        self.buf.consume(idx + 4);

        let mut req = Request {
            id,
            method: head.method,
            path: head.path,
            query: head.query,
            headers: head.headers,
            body: Vec::new(),
            form: HashMap::new(),
            peer_addr: self.peer_addr,
        };

        if content_length > 0 {
            if content_length > self.config.max_body_size {
                return Err(Error::Protocol(StatusCode::ContentTooLarge));
            }

            if req
                .headers
                .get("expect")
                .map(|v| v.eq_ignore_ascii_case("100-continue"))
                .unwrap_or(false)
            {
                stream
                    .set_write_timeout(Some(self.config.write_timeout))
                    .map_err(|_| Error::Closed)?;
                if stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").is_err() {
                    return Err(Error::Closed);
                }
            }

            self.read_body(stream, content_length, deadline)?;
            let body = &self.buf.data()[..content_length];

            let is_form = req
                .headers
                .get("content-type")
                .map(|v| v.starts_with("application/x-www-form-urlencoded"))
                .unwrap_or(false);

            if is_form {
                match parse_urlencoded(body) {
                    Some(map) => req.form = map,
                    None => return Err(Error::Protocol(StatusCode::BadRequest)),
                }
            }

            req.body = body.to_vec();

            self.buf.consume(content_length);
        }
        Ok(req)
    }

    // Returns the index of the end of the headers (the start of the body).
    // The body will start at this index + 4 (after the \r\n\r\n).
    // Also returns the read_timeout deadline to be shared with read_body.
    fn read_head(&mut self, stream: &mut TcpStream) -> Result<(usize, Instant)> {
        let max_size = self.config.max_header_size;
        let sep = b"\r\n\r\n";

        if self.buf.pos() >= max_size {
            return find_from(self.buf.data_up_to(max_size), sep, 0)
                .map(|idx| (idx, Instant::now() + self.config.read_timeout))
                .ok_or(Error::Protocol(StatusCode::RequestHeaderFieldsTooLarge));
        } else if let Some(idx) = find_from(self.buf.data(), sep, 0) {
            return Ok((idx, Instant::now() + self.config.read_timeout));
        }

        let mut idle = !self.first && self.buf.pos() == 0;
        self.first = false;
        let mut deadline = Instant::now()
            + if idle {
                self.config.idle_timeout
            } else {
                self.config.read_timeout
            };

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Protocol(StatusCode::RequestTimeout));
            }
            stream
                .set_read_timeout(Some(remaining))
                .map_err(|_| Error::Closed)?;
            match stream.read(self.buf.rest_mut()) {
                Ok(0) => return Err(Error::Closed),
                Ok(n) => {
                    if idle {
                        deadline = Instant::now() + self.config.read_timeout;
                        idle = false;
                    }
                    let search_from = self.buf.pos();
                    self.buf.advance(n);
                    if let Some(idx) = find_from(self.buf.data_up_to(max_size), sep, search_from) {
                        return Ok((idx, deadline));
                    }
                    if self.buf.pos() >= max_size {
                        return Err(Error::Protocol(StatusCode::RequestHeaderFieldsTooLarge));
                    }
                }
                Err(e) => {
                    use std::io::ErrorKind::{TimedOut, WouldBlock};
                    return Err(if matches!(e.kind(), TimedOut | WouldBlock) {
                        Error::Protocol(StatusCode::RequestTimeout)
                    } else {
                        Error::Closed
                    });
                }
            }
        }
    }

    fn read_body(
        &mut self,
        stream: &mut TcpStream,
        content_length: usize,
        deadline: Instant,
    ) -> Result<()> {
        let max_size = self.config.max_body_size;
        assert!(content_length <= max_size); // should be checked by caller

        while self.buf.pos() < content_length {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Protocol(StatusCode::RequestTimeout));
            }
            stream
                .set_read_timeout(Some(remaining))
                .map_err(|_| Error::Closed)?;
            match stream.read(self.buf.rest_mut()) {
                Ok(0) => return Err(Error::Closed),
                Ok(n) => self.buf.advance(n),
                Err(e) => {
                    use std::io::ErrorKind::{TimedOut, WouldBlock};
                    return Err(if matches!(e.kind(), TimedOut | WouldBlock) {
                        Error::Protocol(StatusCode::RequestTimeout)
                    } else {
                        Error::Closed
                    });
                }
            }
        }
        Ok(())
    }
}

// Search for `sep` in `data` starting from `search_from`.
// Returns the index of the start of `sep` if found.
// Assumes that `sep` is not present in `data[..search_from]`.
fn find_from(data: &[u8], sep: &[u8], search_from: usize) -> Option<usize> {
    // Sep could overlap the boundary, so go back sep.len()-1 bytes.
    let start = search_from.saturating_sub(sep.len().saturating_sub(1));
    data[start..]
        .windows(sep.len())
        .position(|w| w == sep)
        .map(|i| start + i)
}

#[test]
fn test_find_from() {
    let sep = b"\r\n\r\n";
    assert_eq!(find_from(b"hello\r\n\r\nworld", sep, 0), Some(5));
    assert_eq!(find_from(b"\r\n\r\nhello", sep, 4), None);
    assert_eq!(find_from(b"abc\r\n\r\n", sep, 5), Some(3)); // overlap
    assert_eq!(find_from(b"hello world", sep, 0), None);
    assert_eq!(find_from(b"", sep, 0), None);
}

// ----------------------------------------------------------------------------
// ----------------------------------------------------------------------------
// ----------------------------------------------------------------------------

// Parse the request line and headers from `buf`.
// The body is not read and `Request::body` is empty.
fn parse_head(mut buf: &[u8]) -> Option<Head> {
    let first = next_line(&mut buf)?;
    let head = Head::parse(first)?;

    let mut headers = HashMap::new();
    while let Some(line) = next_line(&mut buf) {
        let header = Header::parse(line)?;
        // Duplicate Host headers are a smuggling/poisoning vector.
        if header.key == "host" && headers.contains_key("host") {
            return None;
        }
        let entry = headers.entry(header.key).or_insert_with(String::new);
        if !entry.is_empty() {
            entry.push_str(", ");
        }
        entry.push_str(&header.value);
    }

    if !headers.contains_key("host") {
        return None;
    }

    Some(Head { headers, ..head })
}

struct Head {
    method: Method,
    path: String,
    query: HashMap<String, Vec<String>>,
    headers: HashMap<String, String>,
}

impl Head {
    fn parse(line: &[u8]) -> Option<Self> {
        let mut parts = line.splitn(3, |&b| b == b' ');

        let method = Method::from_bytes(parts.next()?)?;
        let path_and_query = parts.next()?;
        let http_version = parts.next()?;
        if http_version != b"HTTP/1.1" {
            return None;
        }

        let (path, query) = match path_and_query.iter().position(|&b| b == b'?') {
            Some(idx) => (&path_and_query[..idx], &path_and_query[idx + 1..]),
            None => (path_and_query, &path_and_query[0..0]),
        };

        if !path.starts_with(b"/") {
            return None;
        }

        // Reject encoded slashes (only in the path)
        if path
            .windows(3)
            .any(|w| w[0] == b'%' && w[1] == b'2' && (w[2] == b'F' || w[2] == b'f'))
        {
            return None;
        }

        let path = enc::percent_decode(path)?;

        if path.contains('\0') || path.contains('\\') || path.split('/').any(|seg| seg == "..") {
            return None;
        }

        let query = parse_urlencoded(query)?;

        Some(Head {
            method,
            path,
            query,
            headers: HashMap::new(),
        })
    }
}

struct Header {
    key: String,
    value: String,
}

impl Header {
    fn parse(line: &[u8]) -> Option<Self> {
        let line = std::str::from_utf8(line).ok()?;
        let (key, value) = line.split_once(':')?;
        // RFC 7230 s3.2.4: whitespace between field name and colon MUST be rejected
        if key != key.trim() {
            return None;
        }
        if key.is_empty() {
            return None;
        }
        let value = value.trim();
        // RFC 7230 field-content: no CTLs except HT.
        // Control bytes in logged values would allow terminal escape injection.
        if value.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7f) {
            return None;
        }
        Some(Header {
            key: key.to_ascii_lowercase(),
            value: value.to_string(),
        })
    }
}

// application/x-www-form-urlencoded; `+` decodes to a space (WHATWG URL).
fn parse_urlencoded(input: &[u8]) -> Option<HashMap<String, Vec<String>>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for pair in input.split(|&b| b == b'&') {
        let mut parts = pair.splitn(2, |&b| b == b'=');
        let key = parts.next().unwrap_or(b"");
        let value = parts.next().unwrap_or(b"");
        if key.is_empty() {
            continue;
        }
        let key = decode_field(key)?;
        let value = decode_field(value)?;
        map.entry(key).or_default().push(value);
    }
    Some(map)
}

fn decode_field(input: &[u8]) -> Option<String> {
    if input.contains(&b'+') {
        let replaced: Vec<u8> = input
            .iter()
            .map(|&b| if b == b'+' { b' ' } else { b })
            .collect();
        enc::percent_decode(&replaced)
    } else {
        enc::percent_decode(input)
    }
}

fn next_line<'a>(buf: &mut &'a [u8]) -> Option<&'a [u8]> {
    let n = buf.len();
    for i in 0..n {
        if buf[i] == b'\n' {
            return None;
        } else if buf[i] == b'\r' {
            if i + 1 < n && buf[i + 1] == b'\n' {
                let line = &buf[..i];
                *buf = &buf[i + 2..];
                return Some(line);
            } else {
                return None;
            }
        }
    }
    None
}

#[test]
fn test_parse_get_no_query() {
    let line = b"GET /hello HTTP/1.1";
    let req_line = Head::parse(line).unwrap();
    assert_eq!(req_line.method, Method::GET);
    assert_eq!(req_line.path, "/hello");
    assert!(req_line.query.is_empty());
}

#[test]
fn test_parse_get_with_query() {
    let line = b"GET /search?q=rust&lang=en HTTP/1.1";
    let req_line = Head::parse(line).unwrap();
    assert_eq!(req_line.method, Method::GET);
    assert_eq!(req_line.path, "/search");
    assert_eq!(req_line.query["q"], ["rust"]);
    assert_eq!(req_line.query["lang"], ["en"]);
}

#[test]
fn test_parse_query_plus_as_space() {
    let req_line = Head::parse(b"GET /search?q=hello+world HTTP/1.1").unwrap();
    assert_eq!(req_line.query["q"], ["hello world"]);
    // '+' in the path stays literal
    let req_line = Head::parse(b"GET /a+b HTTP/1.1").unwrap();
    assert_eq!(req_line.path, "/a+b");
}

#[test]
fn test_parse_repeated_query_key() {
    let line = b"GET /items?tag=a&tag=b HTTP/1.1";
    let req_line = Head::parse(line).unwrap();
    assert_eq!(req_line.query["tag"], ["a", "b"]);
}

#[test]
fn test_parse_post() {
    let line = b"POST /submit HTTP/1.1";
    let req_line = Head::parse(line).unwrap();
    assert_eq!(req_line.method, Method::POST);
    assert_eq!(req_line.path, "/submit");
    assert!(req_line.query.is_empty());
}

#[test]
fn test_parse_invalid_method() {
    let line = b"FOO /bar HTTP/1.1";
    assert!(Head::parse(line).is_none());
}

#[test]
fn test_parse_invalid_version() {
    let line = b"GET / HTTP/1.0";
    assert!(Head::parse(line).is_none());
}

#[test]
fn test_parse_empty_query_key() {
    let line = b"GET /?=value HTTP/1.1";
    let req_line = Head::parse(line).unwrap();
    assert!(req_line.query.is_empty());
}

#[test]
fn test_parse_rejects_encoded_slash() {
    assert!(Head::parse(b"GET /foo%2Fbar HTTP/1.1").is_none());
    assert!(Head::parse(b"GET /foo%2fbar HTTP/1.1").is_none());
}

#[test]
fn test_parse_rejects_encoded_backslash() {
    assert!(Head::parse(b"GET /foo%5Cbar HTTP/1.1").is_none());
    assert!(Head::parse(b"GET /foo%5cbar HTTP/1.1").is_none());
}

#[test]
fn test_parse_rejects_encoded_null() {
    assert!(Head::parse(b"GET /foo%00bar HTTP/1.1").is_none());
}

#[test]
fn test_parse_rejects_dot_dot_segments() {
    assert!(Head::parse(b"GET /foo/../bar HTTP/1.1").is_none());
    assert!(Head::parse(b"GET /foo/.. HTTP/1.1").is_none());
    assert!(Head::parse(b"GET /../etc/passwd HTTP/1.1").is_none());
    assert!(Head::parse(b"GET /foo/%2e%2e/bar HTTP/1.1").is_none());
}

#[test]
fn test_parse_allows_triple_dot() {
    let req_line = Head::parse(b"GET /foo/.../bar HTTP/1.1").unwrap();
    assert_eq!(req_line.path, "/foo/.../bar");
}

#[test]
fn test_header_parse_basic() {
    let h = Header::parse(b"Content-Type: text/html").unwrap();
    assert_eq!(h.key, "content-type");
    assert_eq!(h.value, "text/html");
}

#[test]
fn test_header_rejects_control_chars_in_value() {
    assert!(Header::parse(b"User-Agent: evil\x1b[2Jclear").is_none());
    assert!(Header::parse(b"User-Agent: evil\x08bs").is_none());
    assert!(Header::parse(b"User-Agent: evil\x7fdel").is_none());
    // HT inside the value is allowed
    assert!(Header::parse(b"User-Agent: a\tb").is_some());
}

#[test]
fn test_parse_head_rejects_duplicate_host() {
    let buf = b"GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n";
    assert!(parse_head(buf).is_none());
}

#[test]
fn test_parse_urlencoded_basic() {
    let map = parse_urlencoded(b"name=Alice&age=30").unwrap();
    assert_eq!(map["name"], ["Alice"]);
    assert_eq!(map["age"], ["30"]);
}

#[test]
fn test_parse_urlencoded_plus_as_space() {
    let map = parse_urlencoded(b"greeting=hello+world").unwrap();
    assert_eq!(map["greeting"], ["hello world"]);
}

#[test]
fn test_parse_urlencoded_percent_encoded() {
    let map = parse_urlencoded(b"city=San%20Francisco").unwrap();
    assert_eq!(map["city"], ["San Francisco"]);
}

#[test]
fn test_parse_urlencoded_repeated_key() {
    let map = parse_urlencoded(b"tag=a&tag=b&tag=c").unwrap();
    assert_eq!(map["tag"], ["a", "b", "c"]);
}

#[test]
fn test_parse_urlencoded_empty_value() {
    let map = parse_urlencoded(b"key=").unwrap();
    assert_eq!(map["key"], [""]);
}

#[test]
fn test_parse_urlencoded_empty_key_skipped() {
    let map = parse_urlencoded(b"=value&real=yes").unwrap();
    assert!(!map.contains_key(""));
    assert_eq!(map["real"], ["yes"]);
}

#[test]
fn test_parse_urlencoded_invalid_percent() {
    assert!(parse_urlencoded(b"key=hello%ZZworld").is_none());
}

#[test]
fn test_parse_urlencoded_empty_input() {
    let map = parse_urlencoded(b"").unwrap();
    assert!(map.is_empty());
}
