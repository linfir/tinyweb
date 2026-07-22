use std::{
    collections::HashMap,
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use crate::{
    date::Date,
    request::{self, Request},
    response::Response,
    sse::{SseResponse, SseWriter, send_sse_headers},
    threadpool::ThreadPool,
    types::{HeaderName, HeaderValue, Method, StatusCode},
    ws::{WebSocket, WsResponse, send_upgrade_headers},
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
    /// Timeout for each write while sending the response, not for the
    /// response as a whole.
    /// A client that keeps reading slowly can extend the total send time
    /// well beyond this; see the crate-level Limitations section.
    /// Default: 5 seconds.
    pub write_timeout: Duration,
    /// How long [`serve_graceful`] waits for in-flight requests to finish after the stop signal.
    /// `None` waits indefinitely.
    /// When the timeout elapses, the sockets of the remaining connections are shut down;
    /// handlers that still do not finish within a short grace period keep running on
    /// detached threads and are killed at process exit.
    /// Default: `Some(2 seconds)`.
    pub shutdown_timeout: Option<Duration>,
    /// Maximum WebSocket message size in bytes.
    /// A larger message closes the connection with status 1009.
    /// Default: 1048576 (1 MB).
    pub max_ws_message_size: usize,
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
        assert!(
            self.shutdown_timeout.is_none_or(|t| !t.is_zero()),
            "shutdown_timeout must be > 0; use None to wait indefinitely"
        );
        assert!(
            self.max_ws_message_size > 0,
            "max_ws_message_size must be > 0"
        );
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
            shutdown_timeout: Some(Duration::from_secs(2)),
            max_body_size: 64 * 1024,
            max_header_size: 8 * 1024,
            max_ws_message_size: 1024 * 1024,
            access_log: true,
        }
    }
}

// How long to keep waiting after cutting the sockets of connections that
// outlived shutdown_timeout, before detaching their worker threads.
const SHUTDOWN_ABORT_GRACE: Duration = Duration::from_secs(1);

// Process-wide request id counter; ids end access log lines as #id.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

// Tracks in-flight connections so shutdown can wait for them to drain,
// and cut their sockets if they outlive the drain timeout.
// Sockets are shared with the workers via Arc, not try_clone'd fds.
struct Drain {
    conns: Mutex<HashMap<u64, Arc<TcpStream>>>,
    cv: Condvar,
    next_id: AtomicU64,
}

impl Drain {
    fn new() -> Arc<Self> {
        Arc::new(Drain {
            conns: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
            next_id: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Arc<TcpStream>>> {
        self.conns.lock().unwrap_or_else(|e| e.into_inner())
    }

    // Registers a connection; it stays in flight until the guard is dropped.
    fn register(self: &Arc<Self>, socket: Arc<TcpStream>) -> ConnGuard {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock().insert(id, socket);
        ConnGuard {
            drain: self.clone(),
            id,
        }
    }

    // Waits until all connections are done or `timeout` elapses.
    // Returns the number of connections still in flight.
    fn wait(&self, timeout: Option<Duration>) -> usize {
        let guard = self.lock();
        match timeout {
            None => self
                .cv
                .wait_while(guard, |m| !m.is_empty())
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            Some(t) => self
                .cv
                .wait_timeout_while(guard, t, |m| !m.is_empty())
                .unwrap_or_else(|e| e.into_inner())
                .0
                .len(),
        }
    }

    // Shuts down the sockets of all connections still in flight, so their
    // blocked reads and writes fail instead of holding up the drain.
    fn cut_sockets(&self) {
        for s in self.lock().values() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

struct ConnGuard {
    drain: Arc<Drain>,
    id: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let mut m = self.drain.lock();
        m.remove(&self.id);
        if m.is_empty() {
            self.drain.cv.notify_all();
        }
    }
}

enum AnyResponseImpl {
    Regular(Response),
    Sse(SseResponse),
    Ws(WsResponse),
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

impl From<WsResponse> for AnyResponse {
    fn from(r: WsResponse) -> Self {
        AnyResponse(AnyResponseImpl::Ws(r))
    }
}

/// Starts handling incoming connections on `listener`.
///
/// Pool size and timeouts are controlled by `config`; use [`Config::default`] for sensible defaults.
///
/// For HEAD requests, the handler is called normally but the response body is not sent.
/// If the handler returns an [`SseResponse`], only the SSE headers are sent
/// and the stream handler is not invoked.
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
                let stream = Arc::new(stream);
                let handler = handler.clone();
                let config = config.clone();
                let shutdown = shutdown.clone();
                let write_timeout = config.write_timeout;
                let job_stream = stream.clone();
                if !pool.execute(move || {
                    handle_stream(job_stream, handler, &config, shutdown);
                }) {
                    log::warn!("thread pool full, rejecting connection");
                    let _ = stream.set_write_timeout(Some(write_timeout));
                    send_error(&stream, StatusCode::ServiceUnavailable);
                }
            }
            Err(e) => {
                log::error!("Cannot establish connection: {}", e);
                // Avoid spinning hot when accept fails repeatedly (e.g. EMFILE).
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    unreachable!()
}

/// Starts handling incoming connections on `listener`.
/// Blocks the calling thread until `stop` receives a message (or all senders are dropped),
/// then shuts down gracefully: pending and new connections are rejected with a
/// [`StatusCode::ServiceUnavailable`] response, and the function waits for all in-flight
/// requests to complete before returning.
///
/// Pool size and timeouts are controlled by `config`; use [`Config::default`] for sensible defaults.
///
/// The stop signal wakes the accept loop by connecting to the listener's own
/// address. If the listen address is not self-connectable (e.g. a firewall
/// blocks it), shutdown is delayed until the next incoming connection.
pub fn serve_graceful<F, R>(
    listener: TcpListener,
    config: Config,
    handler: F,
    stop: mpsc::Receiver<()>,
) where
    F: Fn(&Request) -> R + Send + Sync + 'static,
    R: Into<AnyResponse>,
{
    config.validate();
    if let Err(e) = listener.set_nonblocking(false) {
        log::warn!("Cannot set listener to blocking mode: {}", e);
    }
    let handler = Arc::new(handler);
    let config = Arc::new(config);
    let pool = ThreadPool::new(config.pool_size);
    let drain = Drain::new();
    let shutdown = Arc::new(AtomicBool::new(false));

    let wake_addr = listener
        .local_addr()
        .inspect_err(|e| log::warn!("Cannot get listener address to wake the accept loop: {}", e))
        .ok();
    spawn_stop_watcher(stop, shutdown.clone(), wake_addr);

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if shutdown.load(Ordering::SeqCst) {
                    // Likely the watcher's wake connection; a real client gets a 503.
                    let _ = stream.set_write_timeout(Some(config.write_timeout));
                    send_error(&stream, StatusCode::ServiceUnavailable);
                    break;
                }
                let stream = Arc::new(stream);
                let handler = handler.clone();
                let config = config.clone();
                let shutdown = shutdown.clone();
                let write_timeout = config.write_timeout;
                let guard = drain.register(stream.clone());
                let job_stream = stream.clone();
                if !pool.execute(move || {
                    // On failure, execute drops the closure, and with it the guard.
                    let _guard = guard;
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handle_stream(job_stream, handler, &config, shutdown);
                    }))
                    .is_err()
                    {
                        log::error!("internal error: handle_stream panicked");
                    }
                }) {
                    log::warn!("thread pool full, rejecting connection");
                    let _ = stream.set_write_timeout(Some(write_timeout));
                    send_error(&stream, StatusCode::ServiceUnavailable);
                }
            }
            Err(e) => {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                // WouldBlock means the caller left the listener nonblocking.
                if e.kind() != std::io::ErrorKind::WouldBlock {
                    log::error!("Cannot establish connection: {}", e);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    reject_backlog(&listener, config.write_timeout);
    drop(listener);

    let mut remaining = drain.wait(config.shutdown_timeout);
    if remaining > 0 {
        log::warn!(
            "Shutdown timed out with {} connection(s) still active, closing their sockets",
            remaining
        );
        drain.cut_sockets();
        remaining = drain.wait(Some(SHUTDOWN_ABORT_GRACE));
        if remaining > 0 {
            log::warn!(
                "{} connection(s) did not finish, detaching their worker threads",
                remaining
            );
            return;
        }
    }
    pool.join();
}

// Waits for the stop signal, then sets the shutdown flag (telling in-flight
// connections to stop keep-alive and SSE handlers to wind down) and wakes the
// blocking accept loop with a dummy connection.
fn spawn_stop_watcher(
    stop: mpsc::Receiver<()>,
    shutdown: Arc<AtomicBool>,
    addr: Option<SocketAddr>,
) {
    std::thread::spawn(move || {
        let _ = stop.recv();
        shutdown.store(true, Ordering::SeqCst);
        let Some(mut addr) = addr else { return };
        if addr.ip().is_unspecified() {
            addr.set_ip(if addr.is_ipv4() {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            } else {
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            });
        }
        if let Err(e) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
            log::error!("Cannot wake accept loop: {}", e);
        }
    });
}

// Replies 503 to connections still queued in the kernel accept backlog,
// so they get an orderly answer instead of a reset when the listener drops.
fn reject_backlog(listener: &TcpListener, write_timeout: Duration) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    for _ in 0..128 {
        match listener.accept() {
            Ok((s, _)) => {
                let _ = s.set_write_timeout(Some(write_timeout));
                send_error(&s, StatusCode::ServiceUnavailable);
            }
            Err(_) => break,
        }
    }
}

fn handle_stream<F, R>(
    stream: Arc<TcpStream>,
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
        let req = match rdr.read(&stream, &shutdown, id) {
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
                send_error(&stream, status);
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
                send_error(&stream, status);
                return;
            }
        };

        match any_response.0 {
            AnyResponseImpl::Regular(resp) => {
                let status = resp.status_code();
                let bytes = resp.body_len();
                if let Err(e) = resp.send(
                    &stream,
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
                if let Err(e) = send_sse_headers(&stream, &recv_date) {
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
                // HEAD gets the SSE headers only; the stream handler is not invoked.
                if req.method == Method::HEAD {
                    return;
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
            AnyResponseImpl::Ws(WsResponse(ws_handler)) => {
                let key = req
                    .upgradable()
                    .then(|| req.headers.get("sec-websocket-key"))
                    .flatten();
                let Some(key) = key else {
                    // RFC 6455 s4.2.2: a version mismatch gets 426 plus the
                    // version the server speaks; anything else is a 400.
                    let wants_ws = req
                        .headers
                        .get("upgrade")
                        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
                    let version_ok = req
                        .headers
                        .get("sec-websocket-version")
                        .is_some_and(|v| v == "13");
                    let resp = if wants_ws && !version_ok {
                        Response::error(StatusCode::UpgradeRequired).with_header(
                            HeaderName::SEC_WEBSOCKET_VERSION,
                            HeaderValue::from_static("13").expect("valid header value"),
                        )
                    } else {
                        Response::error(StatusCode::BadRequest)
                    };
                    let status = resp.status_code();
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
                    let _ = resp.send(&stream, None, true, &recv_date);
                    return;
                };
                if let Err(e) = send_upgrade_headers(&stream, key, &recv_date) {
                    log::error!("Failed to send upgrade headers: {}", e);
                    return;
                }
                if config.access_log {
                    let (referer, ua) = clf_headers(&req);
                    let req_line = clf_request_line(req.method.as_str(), &safe_path);
                    log_clf(
                        peer_addr, &recv_date, &req_line, 101, "-", &referer, &ua, None, req.id,
                    );
                }
                let mut ws = WebSocket::new(
                    stream,
                    shutdown.clone(),
                    config.read_timeout,
                    config.max_ws_message_size,
                );
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ws_handler(&mut ws)))
                    .is_err()
                {
                    log::error!("handler panicked");
                }
                let _ = ws.close();
                if config.access_log {
                    log::info!(
                        "{} {} {} WS closed {}ms #{}",
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
#[should_panic(expected = "shutdown_timeout must be > 0")]
fn test_validate_rejects_zero_shutdown_timeout() {
    let config = Config {
        shutdown_timeout: Some(Duration::ZERO),
        ..Config::default()
    };
    config.validate();
}

#[test]
fn test_sanitize_field() {
    assert_eq!(sanitize_field("/foo?a=1"), "/foo?a=1");
    assert_eq!(sanitize_field("a\" 200 \"b"), "a\\x22 200 \\x22b");
    assert_eq!(sanitize_field("a\\x22"), "a\\x5Cx22");
    assert_eq!(sanitize_field("e\u{1b}[2J"), "e\\x1B[2J");
    assert_eq!(sanitize_field("caf\u{e9}"), "caf\\xC3\\xA9");
}

fn send_error(stream: &TcpStream, status_code: StatusCode) {
    let _ = Response::error(status_code).send(stream, None, true, &Date::now());
}
