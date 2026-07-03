use std::{
    fmt,
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    date::Date,
    request::{self, Request},
    response::Response,
    sse::{SseResponse, SseWriter, send_sse_headers},
    threadpool::ThreadPool,
    types::{Method, StatusCode},
};

/// Configuration for [`serve`].
#[non_exhaustive]
pub struct Config {
    /// Number of threads in the pool.
    /// Default: `(cpus * 2).clamp(8, 32)`.
    pub pool_size: usize,
    /// Maximum size of the request line + headers in bytes.
    /// Requests exceeding this limit receive a [`StatusCode::BadRequest`] response.
    /// Default: 8192 (8 KB).
    pub max_header_size: usize,
    /// Maximum request body size in bytes.
    /// Bodies larger than this limit receive a [`StatusCode::ContentTooLarge`] response.
    /// Default: 65536 (64 KB).
    pub max_body_size: usize,
    /// Timeout for reading the request headers and body.
    /// Default: 5 seconds.
    pub read_timeout: Duration,
    /// Idle timeout between keep-alive requests.
    /// Default: 30 seconds.
    pub idle_timeout: Duration,
    /// Timeout for writing the response.
    /// Default: 5 seconds.
    pub write_timeout: Duration,
    /// Emit a `log::info!` line for every completed request (peer IP, method, path, status, latency).
    /// Default: `true`.
    pub access_log: bool,
}

impl Config {
    fn validate(&self) {
        assert!(self.pool_size > 0, "pool_size must be > 0");
        assert!(self.max_header_size > 0, "max_header_size must be > 0");
        assert!(self.max_body_size > 0, "max_body_size must be > 0");
        assert!(!self.read_timeout.is_zero(), "read_timeout must be > 0");
        assert!(!self.idle_timeout.is_zero(), "idle_timeout must be > 0");
        assert!(!self.write_timeout.is_zero(), "write_timeout must be > 0");
    }
}

impl Default for Config {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let pool_size = (cpus * 2).clamp(8, 32);

        Config {
            pool_size,
            read_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(5),
            max_body_size: 64 * 1024,
            max_header_size: 8 * 1024,
            access_log: true,
        }
    }
}

// Process-wide request id counter; ids end access log lines as #id.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

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

/// Starts handling incoming connections on `listener`.
///
/// Requests are dispatched to a thread pool.
/// Pool size and timeouts are controlled by `config`; use [`Config::default`] for sensible defaults.
///
/// For HEAD requests, the handler is called normally but the response body is not sent.
///
/// Panics if any `config` field is zero.
pub fn serve<F, R>(listener: TcpListener, config: Config, handler: F) -> !
where
    F: Fn(&Request) -> R + Send + Sync + 'static,
    R: Into<AnyResponse>,
{
    config.validate();

    let handler = Arc::new(handler);
    let config = Arc::new(config);
    let pool = ThreadPool::new(config.pool_size);
    let shutdown = Arc::new(AtomicBool::new(false));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let handler = handler.clone();
                let config = config.clone();
                let shutdown = shutdown.clone();
                let write_timeout = config.write_timeout;
                let stream_copy = stream.try_clone();
                if !pool.execute(move || {
                    handle_stream(stream, handler, &config, shutdown);
                }) {
                    log::warn!("thread pool full, rejecting connection");
                    if let Ok(mut s) = stream_copy {
                        let _ = s.set_write_timeout(Some(write_timeout));
                        send_error(&mut s, StatusCode::ServiceUnavailable);
                    }
                }
            }
            Err(e) => {
                log::error!("Cannot establish connection: {}", e);
                // Avoid spinning hot when accept fails repeatedly (e.g. EMFILE).
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    unreachable!();
}

fn handle_stream<F, R>(
    mut stream: TcpStream,
    req_handler: Arc<F>,
    config: &Config,
    shutdown: Arc<AtomicBool>,
) where
    F: Fn(&Request) -> R + Send + Sync + 'static,
    R: Into<AnyResponse>,
{
    let Ok(peer_addr) = stream.peer_addr() else {
        return;
    };

    // Set before anything (including error responses) is written.
    if stream
        .set_write_timeout(Some(config.write_timeout))
        .is_err()
    {
        return;
    }

    let mut rdr = request::Reader::new(config, peer_addr);
    loop {
        let start = Instant::now();
        let recv_date = Date::now();
        let id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let req = match rdr.read(&mut stream, &shutdown, id) {
            Ok(r) => r,
            Err(request::Error::Closed) => return,
            Err(request::Error::Protocol(status)) => {
                if config.access_log {
                    log_clf(
                        peer_addr,
                        &recv_date,
                        "-",
                        status.as_u16(),
                        0,
                        "-",
                        "-",
                        Some(start.elapsed().as_millis()),
                        id,
                    );
                }
                send_error(&mut stream, status);
                return;
            }
        };

        let keep_alive = !req
            .headers
            .get("connection")
            .map(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("close")))
            .unwrap_or(false)
            && !shutdown.load(Ordering::Relaxed);
        let safe_path = sanitize_field(&req.path);

        let response =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| req_handler(&req).into()));
        let any_response = match response {
            Ok(r) => r,
            Err(_) => {
                let status = StatusCode::InternalServerError;
                log::error!("handler panicked");
                if config.access_log {
                    let (referer, ua) = clf_headers(&req);
                    let req_line = clf_request_line(req.method.as_str(), &safe_path);
                    log_clf(
                        peer_addr,
                        &recv_date,
                        &req_line,
                        status.as_u16(),
                        0,
                        &referer,
                        &ua,
                        Some(start.elapsed().as_millis()),
                        req.id,
                    );
                }
                send_error(&mut stream, status);
                return;
            }
        };

        match any_response.0 {
            AnyResponseImpl::Regular(resp) => {
                let status = resp.status_code();
                let bytes = resp.body_len();
                if let Err(e) = resp.send(
                    &mut stream,
                    keep_alive.then_some(config.idle_timeout),
                    req.method != Method::HEAD,
                    &recv_date,
                ) {
                    log::error!("Failed to send response: {}", e);
                    return;
                }
                if config.access_log {
                    let (referer, ua) = clf_headers(&req);
                    let req_line = clf_request_line(req.method.as_str(), &safe_path);
                    log_clf(
                        peer_addr,
                        &recv_date,
                        &req_line,
                        status.as_u16(),
                        bytes,
                        &referer,
                        &ua,
                        Some(start.elapsed().as_millis()),
                        req.id,
                    );
                }
                if !keep_alive {
                    return;
                }
            }
            AnyResponseImpl::Sse(SseResponse(sse_handler)) => {
                if let Err(e) = send_sse_headers(&mut stream, &recv_date) {
                    log::error!("Failed to send SSE headers: {}", e);
                    return;
                }
                if config.access_log {
                    let (referer, ua) = clf_headers(&req);
                    let req_line = clf_request_line(req.method.as_str(), &safe_path);
                    log_clf(
                        peer_addr,
                        &recv_date,
                        &req_line,
                        StatusCode::Ok.as_u16(),
                        "-",
                        &referer,
                        &ua,
                        None,
                        req.id,
                    );
                }
                let mut writer = SseWriter::new(stream, shutdown.clone());
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    sse_handler(&mut writer)
                }))
                .is_err()
                {
                    log::error!("handler panicked");
                }
                if config.access_log {
                    log::info!(
                        "{} {} {} SSE closed {}ms #{}",
                        peer_addr,
                        req.method.as_str(),
                        safe_path,
                        start.elapsed().as_millis(),
                        req.id,
                    );
                }
                return;
            }
        }
    }
}

fn clf_headers(req: &Request) -> (String, String) {
    let get = |key| {
        req.headers
            .get(key)
            .map(|s| sanitize_field(s))
            .unwrap_or_else(|| "-".to_string())
    };
    (get("referer"), get("user-agent"))
}

fn clf_request_line(method: &str, path: &str) -> String {
    format!("{method} {path} HTTP/1.1")
}

#[allow(clippy::too_many_arguments)]
fn log_clf(
    peer: impl fmt::Display,
    date: &Date,
    request: &str,
    status: u16,
    bytes: impl fmt::Display,
    referer: &str,
    ua: &str,
    ms: Option<u128>,
    id: u64,
) {
    match ms {
        Some(ms) => log::info!(
            "{} - - {} \"{}\" {} {} \"{}\" \"{}\" {}ms #{}",
            peer,
            date.clf(),
            request,
            status,
            bytes,
            referer,
            ua,
            ms,
            id
        ),
        None => log::info!(
            "{} - - {} \"{}\" {} {} \"{}\" \"{}\" #{}",
            peer,
            date.clf(),
            request,
            status,
            bytes,
            referer,
            ua,
            id
        ),
    }
}

// Escapes bytes that could forge log fields: controls, non-ASCII,
// and the CLF quoting characters '"' and '\'.
fn sanitize_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii() && !b.is_ascii_control() && b != b'"' && b != b'\\' {
            out.push(b as char);
        } else {
            out.push_str(&format!("\\x{:02X}", b));
        }
    }
    out
}

#[test]
fn test_sanitize_field() {
    assert_eq!(sanitize_field("/foo?a=1"), "/foo?a=1");
    assert_eq!(sanitize_field("a\" 200 \"b"), "a\\x22 200 \\x22b");
    assert_eq!(sanitize_field("a\\x22"), "a\\x5Cx22");
    assert_eq!(sanitize_field("e\u{1b}[2J"), "e\\x1B[2J");
    assert_eq!(sanitize_field("caf\u{e9}"), "caf\\xC3\\xA9");
}

fn send_error(stream: &mut TcpStream, status_code: StatusCode) {
    let _ = Response::error(status_code).send(stream, None, true, &Date::now());
}
