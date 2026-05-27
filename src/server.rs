use std::{
    collections::HashMap,
    io::{self, prelude::*},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::Arc,
    thread,
    time::Duration,
};

use crate::{ContentType, Method, StatusCode, enc};

pub struct Request {
    pub method: Method,
    pub path: String,
    pub query: HashMap<String, String>,
    /// Header keys are lowercase
    pub headers: HashMap<String, String>,
}

pub struct Response(ResponseImpl);

enum ResponseImpl {
    Regular {
        status_code: StatusCode,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
}

pub fn serve<A, F>(addr: A, handler: F) -> !
where
    A: ToSocketAddrs,
    F: Fn(&Request) -> Response + Send + Sync + 'static,
{
    let listener = TcpListener::bind(addr).expect("Cannot start server");
    let handler = Arc::new(handler);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let handler = handler.clone();
                thread::spawn(move || {
                    handle_stream(stream, handler);
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
    pub fn not_found() -> Self {
        Response(ResponseImpl::Regular {
            status_code: StatusCode::NotFound,
            headers: Vec::new(),
            body: Vec::new(),
        })
    }

    pub fn error(status_code: StatusCode) -> Self {
        Response(ResponseImpl::Regular {
            status_code,
            headers: vec![("Connection".into(), "close".into())],
            body: Vec::new(),
        })
    }

    pub fn ok(content_type: ContentType, body: impl Into<Vec<u8>>) -> Self {
        Response(ResponseImpl::Regular {
            status_code: StatusCode::Ok,
            headers: vec![("Content-Type".into(), content_type.as_str().into())],
            body: body.into(),
        })
    }

    pub fn file(ext: Option<&str>, body: impl Into<Vec<u8>>) -> Self {
        let mime = ContentType::from_extension(ext);
        if mime == ContentType::Default {
            eprintln!("Unknown file extension: {:?}", ext);
        }
        Response(ResponseImpl::Regular {
            status_code: StatusCode::Ok,
            headers: vec![("Content-Type".into(), mime.as_str().into())],
            body: body.into(),
        })
    }

    pub fn redirect(to: &str) -> Self {
        assert!(
            !to.contains(['\r', '\n']),
            "redirect target must not contain CR or LF"
        );
        Response(ResponseImpl::Regular {
            status_code: StatusCode::TemporaryRedirect,
            headers: vec![
                ("Location".into(), to.into()),
                ("Connection".into(), "close".into()),
            ],
            body: Vec::new(),
        })
    }
}

fn handle_stream<F>(mut stream: TcpStream, handler: Arc<F>)
where
    F: Fn(&Request) -> Response + Send + Sync + 'static,
{
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap(); // safe

    let mut buf = [0; 8 * 1024];
    let mut total = 0;
    loop {
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

    let ResponseImpl::Regular {
        status_code,
        headers,
        body,
    } = handler(&req).0;
    if let Err(e) = send_response(stream, status_code, &headers.into_iter().collect(), &body) {
        log::error!("Failed to send response: {}", e);
    }
}

fn parse_request(mut buf: &[u8]) -> Result<Request, StatusCode> {
    let first = next_line(&mut buf).ok_or(StatusCode::BadRequest)?;
    let req_line = RequestLine::parse(first).ok_or(StatusCode::BadRequest)?;

    let mut headers = HashMap::new();
    while let Some(line) = next_line(&mut buf) {
        if line.is_empty() {
            break;
        }
        let header = Header::parse(line).ok_or(StatusCode::BadRequest)?;
        headers.insert(header.key, header.value);
    }

    if !headers.contains_key("host") {
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

        let path = enc::percent_decode(path)?;

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
            path: path.to_owned(),
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
        Some(Header {
            key: key.trim().to_ascii_lowercase(),
            value: value.trim().to_string(),
        })
    }
}

fn send_response(
    stream: TcpStream,
    status_code: StatusCode,
    headers: &HashMap<String, String>,
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

    write!(w, "\r\n")?;

    w.write_all(body)?;
    w.flush()?;

    Ok(())
}

fn send_error(stream: TcpStream, status_code: StatusCode) {
    let _ = send_response(stream, status_code, &HashMap::new(), b"");
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
