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

use crate::{ContentType, Method, StatusCode, enc, sse::SseWriter};

/// Configuration for [`serve`].
pub struct Config {
    /// Maximum number of concurrent connections.
    /// Excess connections receive a 503 response.
    /// Setting this to 0 rejects all connections.
    /// Default: 100.
    pub max_connections: usize,
    /// Timeout for reading the request headers.
    /// Default: 5 seconds.
    pub read_timeout: Duration,
    /// Timeout for writing the response.
    /// Default: 5 seconds.
    pub write_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_connections: 100,
            read_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(5),
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
///
/// Passed by reference to the handler closure given to [`serve`].
/// Only the request line and headers are available; the request body is not read.
pub struct Request {
    /// The HTTP method.
    pub method: Method,
    /// The percent-decoded request path (e.g. `/foo/bar`).
    pub path: String,
    /// Parsed query-string parameters, percent-decoded.
    pub query: HashMap<String, String>,
    /// Request headers. Keys are lowercased (e.g. `"content-type"`).
    pub headers: HashMap<String, String>,
}

/// An HTTP response returned by the handler closure.
///
/// Construct one using the associated builder methods: [`Response::ok`],
/// [`Response::file`], [`Response::not_found`], [`Response::error`],
/// [`Response::redirect`], or [`Response::sse`].
pub struct Response(ResponseImpl);

enum ResponseImpl {
    Regular {
        status_code: StatusCode,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    Sse {
        handler: Box<dyn FnOnce(&mut SseWriter) + Send + 'static>,
    },
}

/// Binds to `addr` and starts handling incoming connections.
///
/// Each request is dispatched to `handler` on its own thread. The function
/// never returns. Limits are controlled by `config`; use [`Config::default`]
/// for sensible defaults.
pub fn serve<A, F>(addr: A, config: Config, handler: F) -> !
where
    A: ToSocketAddrs,
    F: Fn(&Request) -> Response + Send + Sync + 'static,
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
                thread::spawn(move || {
                    let _guard = ActiveGuard(active);
                    handle_stream(stream, handler, read_timeout, write_timeout);
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
    /// Returns a 404 Not Found response with an empty body.
    pub fn not_found() -> Self {
        Response(ResponseImpl::Regular {
            status_code: StatusCode::NotFound,
            headers: Vec::new(),
            body: Vec::new(),
        })
    }

    /// Returns a response with the given (error) status code and an empty body.
    pub fn error(status_code: StatusCode) -> Self {
        Response(ResponseImpl::Regular {
            status_code,
            headers: Vec::new(),
            body: Vec::new(),
        })
    }

    /// Returns a 200 OK response with the given content type and body.
    pub fn ok(content_type: ContentType, body: impl Into<Vec<u8>>) -> Self {
        Response(ResponseImpl::Regular {
            status_code: StatusCode::Ok,
            headers: vec![("Content-Type".into(), content_type.as_str().into())],
            body: body.into(),
        })
    }

    /// Returns a 200 OK response with a MIME type inferred from `ext`.
    ///
    /// `ext` should be the file extension without a leading dot (e.g. `"html"`).
    /// If the extension is unknown, `application/octet-stream` is used and a
    /// warning is logged.
    pub fn file(ext: Option<&str>, body: impl Into<Vec<u8>>) -> Self {
        let mime = ContentType::from_extension(ext);
        if mime == ContentType::Default {
            log::warn!("Unknown file extension: {:?}", ext);
        }
        Response(ResponseImpl::Regular {
            status_code: StatusCode::Ok,
            headers: vec![("Content-Type".into(), mime.as_str().into())],
            body: body.into(),
        })
    }

    /// Returns a 307 Temporary Redirect to `to`.
    ///
    /// Returns `Err` if `to` contains CR (`\r`) or LF (`\n`),
    /// which would corrupt the response headers.
    pub fn redirect(to: &str) -> Result<Self, &'static str> {
        if to.contains(['\r', '\n']) {
            return Err("redirect target must not contain CR or LF");
        }
        Ok(Response(ResponseImpl::Regular {
            status_code: StatusCode::TemporaryRedirect,
            headers: vec![("Location".into(), to.into())],
            body: Vec::new(),
        }))
    }

    /// Returns a Server-Sent Events response.
    ///
    /// `handler` is called synchronously on the connection thread
    /// with a [`SseWriter`] to send events.
    /// The connection closes when `handler` returns.
    pub fn sse<F>(handler: F) -> Self
    where
        F: FnOnce(&mut SseWriter) + Send + 'static,
    {
        Response(ResponseImpl::Sse {
            handler: Box::new(handler),
        })
    }
}

fn handle_stream<F>(
    mut stream: TcpStream,
    handler: Arc<F>,
    read_timeout: Duration,
    write_timeout: Duration,
) where
    F: Fn(&Request) -> Response + Send + Sync + 'static,
{
    let deadline = Instant::now() + read_timeout;

    let mut buf = [0; 8 * 1024];
    let mut total = 0;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return send_error(stream, StatusCode::BadRequest);
        }
        stream.set_read_timeout(Some(remaining)).unwrap(); // safe
        match stream.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if total == buf.len() {
                    break;
                }
            }
            Err(_) => return send_error(stream, StatusCode::BadRequest),
        }
    }

    let req = match parse_request(&buf[..total]) {
        Ok(x) => x,
        Err(err) => return send_error(stream, err),
    };

    stream.set_write_timeout(Some(write_timeout)).unwrap(); // safe

    match handler(&req).0 {
        ResponseImpl::Regular {
            status_code,
            headers,
            body,
        } => {
            if let Err(e) = send_response(stream, status_code, &headers, &body) {
                log::error!("Failed to send response: {}", e);
            }
        }
        ResponseImpl::Sse { handler } => {
            if let Err(e) = send_sse_headers(&mut stream) {
                log::error!("Failed to send SSE headers: {}", e);
                return;
            }
            let mut writer = SseWriter::new(stream);
            handler(&mut writer);
        }
    }
}

fn parse_request(mut buf: &[u8]) -> Result<Request, StatusCode> {
    let first = next_line(&mut buf).ok_or(StatusCode::BadRequest)?;
    let req_line = RequestLine::parse(first).ok_or(StatusCode::BadRequest)?;

    let mut headers = HashMap::new();
    let mut found_end = false;
    while let Some(line) = next_line(&mut buf) {
        if line.is_empty() {
            found_end = true;
            break;
        }
        let header = Header::parse(line).ok_or(StatusCode::BadRequest)?;
        let entry = headers.entry(header.key).or_insert_with(String::new);
        if !entry.is_empty() {
            entry.push_str(", ");
        }
        entry.push_str(&header.value);
    }

    if !found_end || !headers.contains_key("host") {
        return Err(StatusCode::BadRequest);
    }

    Ok(Request {
        method: req_line.method,
        path: req_line.path,
        query: req_line.query_map,
        headers,
    })
}

struct RequestLine {
    method: Method,
    path: String,
    query_map: HashMap<String, String>,
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

        let mut query_map = HashMap::new();
        if !query.is_empty() {
            for pair in query.split(|&b| b == b'&') {
                let mut parts = pair.splitn(2, |&b| b == b'=');
                let key = parts.next().unwrap_or(b"");
                let value = parts.next().unwrap_or(b"");
                if !key.is_empty() {
                    let key = enc::percent_decode(key)?;
                    let value = enc::percent_decode(value)?;
                    query_map.insert(key, value);
                }
            }
        }

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
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        Some(Header {
            key: key.to_ascii_lowercase(),
            value: value.trim().to_string(),
        })
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

fn send_response(
    stream: TcpStream,
    status_code: StatusCode,
    headers: &[(String, String)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut w = io::BufWriter::new(stream);

    write!(
        w,
        "HTTP/1.1 {} {}\r\n",
        status_code.as_u16(),
        status_code.as_str()
    )?;

    for (name, value) in headers {
        write!(w, "{}: {}\r\n", name, value)?;
    }
    write!(w, "Content-Length: {}\r\n", body.len())?;
    write!(w, "Connection: close\r\n")?;

    write!(w, "\r\n")?;

    w.write_all(body)?;
    w.flush()?;

    Ok(())
}

fn send_error(stream: TcpStream, status_code: StatusCode) {
    let _ = send_response(stream, status_code, &[], b"");
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
    assert_eq!(req_line.query_map.get("q"), Some(&"rust".to_string()));
    assert_eq!(req_line.query_map.get("lang"), Some(&"en".to_string()));
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
