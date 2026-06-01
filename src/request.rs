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

impl Request {
    pub(crate) fn read(
        stream: &TcpStream,
        cfg: &Config,
        peer_addr: SocketAddr,
    ) -> Result<Self, StatusCode> {
        read_request(stream, cfg, peer_addr)
    }
}

fn read_request(
    mut stream: &TcpStream,
    cfg: &Config,
    peer_addr: SocketAddr,
) -> Result<Request, StatusCode> {
    let deadline = cfg
        .read_timeout
        .filter(|d| !d.is_zero())
        .map(|d| Instant::now() + d);
    let mut buf = vec![0u8; cfg.max_header_size];

    let Some((head, body_start)) = read_request_head(stream, deadline, &mut buf) else {
        return Err(StatusCode::BadRequest);
    };

    let Some(mut req) = parse_request(head, peer_addr) else {
        return Err(StatusCode::BadRequest);
    };

    if req.headers.contains_key("transfer-encoding") {
        return Err(StatusCode::NotImplemented);
    }

    let content_length: usize = match req.headers.get("content-length") {
        None => 0,
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(_) => return Err(StatusCode::BadRequest),
        },
    };

    if content_length > 0 {
        if content_length > cfg.max_body_size {
            return Err(StatusCode::ContentTooLarge);
        }

        if req
            .headers
            .get("expect")
            .map(|v| v.eq_ignore_ascii_case("100-continue"))
            .unwrap_or(false)
            && stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").is_err()
        {
            return Err(StatusCode::BadRequest);
        }

        match read_request_body(stream, deadline, body_start, content_length) {
            Some(body) => req.body = body,
            None => return Err(StatusCode::BadRequest),
        };

        let is_form = req
            .headers
            .get("content-type")
            .map(|v| v.starts_with("application/x-www-form-urlencoded"))
            .unwrap_or(false);

        if is_form {
            match parse_urlencoded(&req.body, true) {
                Some(map) => req.form = map,
                None => return Err(StatusCode::BadRequest),
            }
        }
    }
    Ok(req)
}

fn read_request_head<'a>(
    mut stream: &TcpStream,
    deadline: Option<Instant>,
    buf: &'a mut [u8],
) -> Option<(&'a [u8], &'a [u8])> {
    let sep = b"\r\n\r\n";
    let mut end = 0;

    loop {
        let timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        if timeout.map(|t| t.is_zero()).unwrap_or(false) {
            break;
        }
        stream.set_read_timeout(timeout).unwrap();
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
    deadline: Option<Instant>,
    body_start: &[u8],
    content_length: usize,
) -> Option<Vec<u8>> {
    let mut body = vec![0u8; content_length];
    let mut pos = body_start.len().min(content_length);
    body[..pos].copy_from_slice(&body_start[..pos]);
    while pos < content_length {
        let timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        if timeout.map(|t| t.is_zero()).unwrap_or(false) {
            return None;
        }
        stream.set_read_timeout(timeout).unwrap();
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
fn parse_request(mut buf: &[u8], peer_addr: SocketAddr) -> Option<Request> {
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
        peer_addr,
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
    fn parse(line: &[u8]) -> Option<Self> {
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
    let peer_addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
    let req = parse_request(raw, peer_addr).unwrap();
    assert_eq!(req.method, Method::POST);
    assert!(req.body.is_empty());
    assert!(req.form.is_empty());
}
