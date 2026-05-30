use std::{
    collections::HashMap,
    io::{self, prelude::*},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{ContentType, HeaderName, Method, StatusCode, enc, log, sse::SseWriter};

/// Configuration for [`serve`].
pub struct Config {
    /// Maximum number of concurrent connections.
    /// Excess connections receive a 503 response.
    /// Setting this to 0 rejects all connections.
    /// Default: 100.
    pub max_connections: usize,
    /// Timeout for reading the request headers and body.
    /// Default: 5 seconds.
    pub read_timeout: Duration,
    /// Timeout for writing the response.
    /// Default: 5 seconds.
    pub write_timeout: Duration,
    /// Maximum request body size in bytes.
    /// Bodies larger than this limit receive a 413 response.
    /// Default: 65536 (64 KB).
    pub max_body_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_connections: 100,
            read_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(5),
            max_body_size: 64 * 1024,
        }
    }
}

/// Ensures the decrement happens even if the handler panics.
struct ActiveGuard(Arc<AtomicUsize>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// An incoming HTTP request.
#[non_exhaustive]
pub struct Request {
    /// The HTTP method.
    pub method: Method,
    /// The percent-decoded request path (e.g. `/foo/bar`).
    pub path: String,
    /// Parsed query-string parameters, percent-decoded.
    /// If a key appears more than once, all values are collected in order.
    pub query: HashMap<String, Vec<String>>,
    /// Request headers.
    /// Keys are lowercased (e.g. `"content-type"`).
    pub headers: HashMap<String, String>,
    /// Raw request body bytes.
    /// Requests exceeding [`Config::max_body_size`] are rejected with 413.
    pub body: Vec<u8>,
    /// Parsed `application/x-www-form-urlencoded` form fields.
    /// Keys and values are percent-decoded; `+` is treated as a space.
    /// Only populated when `Content-Type: application/x-www-form-urlencoded`.
    pub form: HashMap<String, Vec<String>>,
}

/// An HTTP header value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderValue(String);

impl HeaderValue {
    /// Returns `Err` if `value` contains CR (`\r`), LF (`\n`), or NUL (`\0`).
    pub fn new(value: &str) -> Result<Self, &'static str> {
        if value.contains(['\r', '\n', '\0']) {
            return Err("header value must not contain CR, LF, or NUL");
        }
        Ok(HeaderValue(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A regular HTTP response.
pub struct Response {
    status_code: StatusCode,
    content_type: Option<HeaderValue>,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Vec<u8>,
}

/// A Server-Sent Events response.
pub struct SseResponse(Box<dyn FnOnce(&mut SseWriter) + Send + 'static>);

impl SseResponse {
    /// `handler` is called synchronously on the connection thread; the connection closes when it returns.
    pub fn new<F>(handler: F) -> Self
    where
        F: FnOnce(&mut SseWriter) + Send + 'static,
    {
        SseResponse(Box::new(handler))
    }
}

enum AnyResponseImpl {
    Regular(Response),
    Sse(SseResponse),
}

/// The return type of a request handler.
pub struct AnyResponse(AnyResponseImpl);

impl From<Response> for AnyResponse {
    fn from(r: Response) -> Self {
        AnyResponse(AnyResponseImpl::Regular(r))
    }
}

impl From<SseResponse> for AnyResponse {
    fn from(r: SseResponse) -> Self {
        AnyResponse(AnyResponseImpl::Sse(r))
    }
}

/// Binds to `addr` and starts handling incoming connections.
///
/// Each request is dispatched to `handler` on its own thread.
/// Limits are controlled by `config`; use [`Config::default`] for sensible defaults.
pub fn serve<A, F, R>(addr: A, config: Config, handler: F) -> !
where
    A: ToSocketAddrs,
    F: Fn(&Request) -> R + Send + Sync + 'static,
    R: Into<AnyResponse>,
{
    let listener = TcpListener::bind(addr).expect("Cannot start server");
    let handler = Arc::new(handler);
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if active.fetch_add(1, Ordering::Relaxed) >= config.max_connections {
                    active.fetch_sub(1, Ordering::Relaxed);
                    send_error(stream, StatusCode::ServiceUnavailable);
                    continue;
                }
                let handler = handler.clone();
                let active = active.clone();
                let read_timeout = config.read_timeout;
                let write_timeout = config.write_timeout;
                let max_body_size = config.max_body_size;
                thread::spawn(move || {
                    let _guard = ActiveGuard(active);
                    handle_stream(stream, handler, read_timeout, write_timeout, max_body_size);
                });
            }
            Err(e) => {
                log::error!("Cannot establish connection: {}", e);
            }
        }
    }
    unreachable!();
}

impl Response {
    /// Returns a 200 OK response with no headers and an empty body.
    pub fn new() -> Self {
        Response {
            status_code: StatusCode::Ok,
            content_type: None,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Returns the response with the status code updated.
    pub fn with_status(mut self, status_code: StatusCode) -> Self {
        self.status_code = status_code;
        self
    }

    /// Returns the response with the given body and content type.
    pub fn with_body(mut self, content_type: ContentType, body: impl Into<Vec<u8>>) -> Self {
        self.content_type = Some(HeaderValue(content_type.into_string()));
        self.body = body.into();
        self
    }

    /// Returns the response with an additional HTTP header.
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.add_header(name, value);
        self
    }

    fn add_header(&mut self, name: HeaderName, value: HeaderValue) {
        if name.as_str().eq_ignore_ascii_case("content-length")
            || name.as_str().eq_ignore_ascii_case("connection")
        {
            return;
        }
        if name.as_str().eq_ignore_ascii_case("content-type") {
            self.content_type = Some(value);
            return;
        }
        self.headers.push((name, value));
    }
}

impl Response {
    /// Returns a 404 Not Found response with an empty body.
    pub fn not_found() -> Self {
        Self::error(StatusCode::NotFound)
    }

    /// Returns a response with the given (error) status code and an empty body.
    pub fn error(status_code: StatusCode) -> Self {
        Self::new().with_status(status_code)
    }

    /// Returns a 200 OK response with the given content type and body.
    pub fn ok(content_type: ContentType, body: impl Into<Vec<u8>>) -> Self {
        Self::new().with_body(content_type, body)
    }

    /// Returns a 200 OK response with a MIME type inferred from `ext`.
    ///
    /// `ext` should be the file extension without a leading dot (e.g. `"html"`).
    /// If the extension is unknown, `application/octet-stream` is used
    /// and a warning is logged.
    pub fn file(ext: Option<&str>, body: impl Into<Vec<u8>>) -> Self {
        let mime = ContentType::from_extension(ext).unwrap_or_else(|| {
            log::warn!("Unknown file extension: {:?}", ext);
            ContentType::DEFAULT
        });
        Self::new().with_body(mime, body)
    }

    /// Returns a 307 Temporary Redirect to `to`.
    ///
    /// Use [`HeaderValue::new`] to construct the target URL,
    /// which validates that it contains no CR or LF.
    pub fn redirect(to: HeaderValue) -> Self {
        Self::new()
            .with_status(StatusCode::TemporaryRedirect)
            .with_header(HeaderName::LOCATION, to)
    }
}

impl Default for Response {
    fn default() -> Self {
        Self::new()
    }
}

fn handle_stream<F, R>(
    mut stream: TcpStream,
    req_handler: Arc<F>,
    read_timeout: Duration,
    write_timeout: Duration,
    max_body_size: usize,
) where
    F: Fn(&Request) -> R + Send + Sync + 'static,
    R: Into<AnyResponse>,
{
    let deadline = Instant::now() + read_timeout;
    let mut buf = [0; 8 * 1024];

    let Some((head, body_start)) = read_request_head(&stream, deadline, &mut buf) else {
        return send_error(stream, StatusCode::BadRequest);
    };

    let Some(mut req) = parse_request(head) else {
        return send_error(stream, StatusCode::BadRequest);
    };

    if req.headers.contains_key("transfer-encoding") {
        return send_error(stream, StatusCode::NotImplemented);
    }

    let content_length: usize = match req.headers.get("content-length") {
        None => 0,
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(_) => return send_error(stream, StatusCode::BadRequest),
        },
    };

    if content_length > 0 {
        if content_length > max_body_size {
            return send_error(stream, StatusCode::ContentTooLarge);
        }

        match read_request_body(&stream, deadline, body_start, content_length) {
            Some(body) => req.body = body,
            None => return send_error(stream, StatusCode::BadRequest),
        };

        let is_form = req
            .headers
            .get("content-type")
            .map(|v| v.starts_with("application/x-www-form-urlencoded"))
            .unwrap_or(false);

        if is_form {
            match parse_urlencoded(&req.body, true) {
                Some(map) => req.form = map,
                None => return send_error(stream, StatusCode::BadRequest),
            }
        }
    }

    if !write_timeout.is_zero() {
        stream.set_write_timeout(Some(write_timeout)).unwrap();
    }

    match req_handler(&req).into().0 {
        AnyResponseImpl::Regular(resp) => {
            if let Err(e) = send_response(stream, resp) {
                log::error!("Failed to send response: {}", e);
            }
        }
        AnyResponseImpl::Sse(SseResponse(sse_handler)) => {
            if let Err(e) = send_sse_headers(&mut stream) {
                log::error!("Failed to send SSE headers: {}", e);
                return;
            }
            let mut writer = SseWriter::new(stream);
            sse_handler(&mut writer);
        }
    }
}

fn read_request_head<'a>(
    mut stream: &TcpStream,
    deadline: Instant,
    buf: &'a mut [u8],
) -> Option<(&'a [u8], &'a [u8])> {
    let sep = b"\r\n\r\n";
    let mut end = 0;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        stream.set_read_timeout(Some(remaining)).unwrap();
        match stream.read(&mut buf[end..]) {
            Ok(0) => break,
            Ok(n) => {
                let search_from = end.saturating_sub(sep.len() - 1);
                end += n;
                if let Some(p) = buf[search_from..end]
                    .windows(sep.len())
                    .position(|w| w == sep)
                {
                    let pos = search_from + p;
                    return Some((&buf[..pos + sep.len()], &buf[pos + sep.len()..end]));
                }
                if end == buf.len() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    None
}

fn read_request_body(
    mut stream: &TcpStream,
    deadline: Instant,
    body_start: &[u8],
    content_length: usize,
) -> Option<Vec<u8>> {
    let mut body = vec![0u8; content_length];
    let mut pos = body_start.len().min(content_length);
    body[..pos].copy_from_slice(&body_start[..pos]);
    while pos < content_length {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(remaining)).unwrap();
        match stream.read(&mut body[pos..]) {
            Ok(0) => return None,
            Ok(n) => pos += n,
            Err(_) => return None,
        }
    }
    Some(body)
}

// Parse the request line and headers from `buf`.
// The body is not read and `Request::body` is empty.
fn parse_request(mut buf: &[u8]) -> Option<Request> {
    let first = next_line(&mut buf)?;
    let req_line = RequestLine::parse(first)?;

    let mut headers = HashMap::new();
    let mut found_end = false;
    while let Some(line) = next_line(&mut buf) {
        if line.is_empty() {
            found_end = true;
            break;
        }
        let header = Header::parse(line)?;
        let entry = headers.entry(header.key).or_insert_with(String::new);
        if !entry.is_empty() {
            entry.push_str(", ");
        }
        entry.push_str(&header.value);
    }

    if !found_end || !headers.contains_key("host") {
        return None;
    }

    Some(Request {
        method: req_line.method,
        path: req_line.path,
        query: req_line.query_map,
        headers,
        body: Vec::new(),
        form: HashMap::new(),
    })
}

struct RequestLine {
    method: Method,
    path: String,
    query_map: HashMap<String, Vec<String>>,
}

impl RequestLine {
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

        let query_map = parse_urlencoded(query, false)?;

        Some(RequestLine {
            method,
            path,
            query_map,
        })
    }
}

struct Header {
    key: String,
    value: String,
}

impl Header {
    pub fn parse(line: &[u8]) -> Option<Self> {
        let line = std::str::from_utf8(line).ok()?;
        let (key, value) = line.split_once(':')?;
        // RFC 7230 §3.2.4: whitespace between field name and colon MUST be rejected
        if key != key.trim() {
            return None;
        }
        if key.is_empty() {
            return None;
        }
        Some(Header {
            key: key.to_ascii_lowercase(),
            value: value.trim().to_string(),
        })
    }
}

fn parse_urlencoded(input: &[u8], plus_as_space: bool) -> Option<HashMap<String, Vec<String>>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for pair in input.split(|&b| b == b'&') {
        let mut parts = pair.splitn(2, |&b| b == b'=');
        let key = parts.next().unwrap_or(b"");
        let value = parts.next().unwrap_or(b"");
        if key.is_empty() {
            continue;
        }
        let key = decode_field(key, plus_as_space)?;
        let value = decode_field(value, plus_as_space)?;
        map.entry(key).or_default().push(value);
    }
    Some(map)
}

fn decode_field(input: &[u8], plus_as_space: bool) -> Option<String> {
    if plus_as_space && input.contains(&b'+') {
        let replaced: Vec<u8> = input
            .iter()
            .map(|&b| if b == b'+' { b' ' } else { b })
            .collect();
        enc::percent_decode(&replaced)
    } else {
        enc::percent_decode(input)
    }
}

fn send_sse_headers(stream: &mut TcpStream) -> io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\n\
          Content-Type: text/event-stream\r\n\
          Cache-Control: no-cache\r\n\
          Connection: close\r\n\
          \r\n",
    )
}

fn send_response(stream: TcpStream, resp: Response) -> std::io::Result<()> {
    let mut w = io::BufWriter::new(stream);

    write!(
        w,
        "HTTP/1.1 {} {}\r\n",
        resp.status_code.as_u16(),
        resp.status_code.as_str()
    )?;

    if let Some(ct) = &resp.content_type {
        write!(w, "Content-Type: {}\r\n", ct.as_str())?;
    }
    for (name, value) in &resp.headers {
        write!(w, "{}: {}\r\n", name.as_str(), value.as_str())?;
    }
    write!(w, "Content-Length: {}\r\n", resp.body.len())?;
    write!(w, "Connection: close\r\n")?;
    write!(w, "\r\n")?;

    w.write_all(&resp.body)?;
    w.flush()
}

fn send_error(stream: TcpStream, status_code: StatusCode) {
    let _ = send_response(stream, Response::error(status_code));
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
    let req_line = RequestLine::parse(line).unwrap();
    assert_eq!(req_line.method, Method::GET);
    assert_eq!(req_line.path, "/hello");
    assert!(req_line.query_map.is_empty());
}

#[test]
fn test_parse_get_with_query() {
    let line = b"GET /search?q=rust&lang=en HTTP/1.1";
    let req_line = RequestLine::parse(line).unwrap();
    assert_eq!(req_line.method, Method::GET);
    assert_eq!(req_line.path, "/search");
    assert_eq!(req_line.query_map["q"], ["rust"]);
    assert_eq!(req_line.query_map["lang"], ["en"]);
}

#[test]
fn test_parse_repeated_query_key() {
    let line = b"GET /items?tag=a&tag=b HTTP/1.1";
    let req_line = RequestLine::parse(line).unwrap();
    assert_eq!(req_line.query_map["tag"], ["a", "b"]);
}

#[test]
fn test_parse_post() {
    let line = b"POST /submit HTTP/1.1";
    let req_line = RequestLine::parse(line).unwrap();
    assert_eq!(req_line.method, Method::POST);
    assert_eq!(req_line.path, "/submit");
    assert!(req_line.query_map.is_empty());
}

#[test]
fn test_parse_invalid_method() {
    let line = b"FOO /bar HTTP/1.1";
    assert!(RequestLine::parse(line).is_none());
}

#[test]
fn test_parse_invalid_version() {
    let line = b"GET / HTTP/1.0";
    assert!(RequestLine::parse(line).is_none());
}

#[test]
fn test_parse_empty_query_key() {
    let line = b"GET /?=value HTTP/1.1";
    let req_line = RequestLine::parse(line).unwrap();
    assert!(req_line.query_map.is_empty());
}

#[test]
fn test_parse_rejects_encoded_slash() {
    assert!(RequestLine::parse(b"GET /foo%2Fbar HTTP/1.1").is_none());
    assert!(RequestLine::parse(b"GET /foo%2fbar HTTP/1.1").is_none());
}

#[test]
fn test_parse_rejects_encoded_backslash() {
    assert!(RequestLine::parse(b"GET /foo%5Cbar HTTP/1.1").is_none());
    assert!(RequestLine::parse(b"GET /foo%5cbar HTTP/1.1").is_none());
}

#[test]
fn test_parse_rejects_encoded_null() {
    assert!(RequestLine::parse(b"GET /foo%00bar HTTP/1.1").is_none());
}

#[test]
fn test_parse_rejects_dot_dot_segments() {
    assert!(RequestLine::parse(b"GET /foo/../bar HTTP/1.1").is_none());
    assert!(RequestLine::parse(b"GET /foo/.. HTTP/1.1").is_none());
    assert!(RequestLine::parse(b"GET /../etc/passwd HTTP/1.1").is_none());
    assert!(RequestLine::parse(b"GET /foo/%2e%2e/bar HTTP/1.1").is_none());
}

#[test]
fn test_parse_allows_triple_dot() {
    let req_line = RequestLine::parse(b"GET /foo/.../bar HTTP/1.1").unwrap();
    assert_eq!(req_line.path, "/foo/.../bar");
}

#[test]
fn test_parse_urlencoded_basic() {
    let map = parse_urlencoded(b"name=Alice&age=30", false).unwrap();
    assert_eq!(map["name"], ["Alice"]);
    assert_eq!(map["age"], ["30"]);
}

#[test]
fn test_parse_urlencoded_plus_as_space() {
    let map = parse_urlencoded(b"greeting=hello+world", true).unwrap();
    assert_eq!(map["greeting"], ["hello world"]);
}

#[test]
fn test_parse_urlencoded_plus_literal_without_flag() {
    let map = parse_urlencoded(b"greeting=hello+world", false).unwrap();
    assert_eq!(map["greeting"], ["hello+world"]);
}

#[test]
fn test_parse_urlencoded_percent_encoded() {
    let map = parse_urlencoded(b"city=San%20Francisco", true).unwrap();
    assert_eq!(map["city"], ["San Francisco"]);
}

#[test]
fn test_parse_urlencoded_repeated_key() {
    let map = parse_urlencoded(b"tag=a&tag=b&tag=c", false).unwrap();
    assert_eq!(map["tag"], ["a", "b", "c"]);
}

#[test]
fn test_parse_urlencoded_empty_value() {
    let map = parse_urlencoded(b"key=", false).unwrap();
    assert_eq!(map["key"], [""]);
}

#[test]
fn test_parse_urlencoded_empty_key_skipped() {
    let map = parse_urlencoded(b"=value&real=yes", false).unwrap();
    assert!(!map.contains_key(""));
    assert_eq!(map["real"], ["yes"]);
}

#[test]
fn test_parse_urlencoded_invalid_percent() {
    assert!(parse_urlencoded(b"key=hello%ZZworld", false).is_none());
}

#[test]
fn test_parse_urlencoded_empty_input() {
    let map = parse_urlencoded(b"", false).unwrap();
    assert!(map.is_empty());
}

#[test]
fn test_parse_request_post_no_body() {
    let raw = b"POST /submit HTTP/1.1\r\nHost: localhost\r\n\r\n";
    let req = parse_request(raw).unwrap();
    assert_eq!(req.method, Method::POST);
    assert!(req.body.is_empty());
    assert!(req.form.is_empty());
}
