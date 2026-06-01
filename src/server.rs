use std::{
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    request::Request,
    response::Response,
    sse::{SseResponse, SseWriter, send_sse_headers},
    threadpool::ThreadPool,
    types::StatusCode,
};

/// Configuration for [`serve`].
#[non_exhaustive]
pub struct Config {
    /// Number of threads in the pool.
    /// Default: `(cpus * 4).clamp(8, 16)`.
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
    /// `None` means no timeout.
    /// Default: 5 seconds.
    pub read_timeout: Option<Duration>,
    /// Timeout for writing the response.
    /// `None` means no timeout.
    /// Default: 5 seconds.
    pub write_timeout: Option<Duration>,
    /// Emit a `log::info!` line for every completed request (peer IP, method, path, status, latency).
    /// Default: `true`.
    pub access_log: bool,
}

impl Default for Config {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let pool_size = (2 * cpus).clamp(8, 16);

        Config {
            pool_size,
            read_timeout: Some(Duration::from_secs(5)),
            write_timeout: Some(Duration::from_secs(5)),
            max_body_size: 64 * 1024,
            max_header_size: 8 * 1024,
            access_log: true,
        }
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
/// Requests are dispatched to a thread pool.
/// Pool size and timeouts are controlled by `config`; use [`Config::default`] for sensible defaults.
pub fn serve<A, F, R>(addr: A, config: Config, handler: F) -> !
where
    A: ToSocketAddrs,
    F: Fn(&Request) -> R + Send + Sync + 'static,
    R: Into<AnyResponse>,
{
    assert!(config.pool_size > 0);
    assert!(config.max_header_size > 0);
    assert!(config.max_body_size > 0);
    assert!(config.read_timeout.map(|d| !d.is_zero()).unwrap_or(true));
    assert!(config.write_timeout.map(|d| !d.is_zero()).unwrap_or(true));

    let listener = TcpListener::bind(addr).expect("Cannot start server");
    let handler = Arc::new(handler);
    let config = Arc::new(config);
    let pool = ThreadPool::new(config.pool_size);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let handler = handler.clone();
                let config = config.clone();
                pool.execute(move || {
                    handle_stream(stream, handler, &config);
                });
            }
            Err(e) => {
                log::error!("Cannot establish connection: {}", e);
            }
        }
    }
    unreachable!();
}

fn handle_stream<F, R>(mut stream: TcpStream, req_handler: Arc<F>, cfg: &Config)
where
    F: Fn(&Request) -> R + Send + Sync + 'static,
    R: Into<AnyResponse>,
{
    let Ok(peer_addr) = stream.peer_addr() else {
        return;
    };

    let mut first = true;
    loop {
        let start = Instant::now();
        let req = match Request::read(&stream, cfg, peer_addr) {
            Ok(r) => r,
            Err(status) => {
                if first {
                    if cfg.access_log {
                        log::info!(
                            "{} - - {} {}ms",
                            peer_addr,
                            status.as_u16(),
                            start.elapsed().as_millis()
                        );
                    }
                    send_error(&mut stream, status);
                }
                return;
            }
        };
        first = false;

        let keep_alive = !req
            .headers
            .get("connection")
            .map(|v| v.eq_ignore_ascii_case("close"))
            .unwrap_or(false);

        // set_write_timeout rejects Some(0) on most platforms; treat it as no timeout.
        stream
            .set_write_timeout(cfg.write_timeout.filter(|d| !d.is_zero()))
            .unwrap();

        let response =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| req_handler(&req).into()));
        let any_response = match response {
            Ok(r) => r,
            Err(_) => {
                let status = StatusCode::InternalServerError;
                log::error!("handler panicked");
                if cfg.access_log {
                    log::info!(
                        "{} {} {} {} {}ms",
                        peer_addr,
                        req.method.as_str(),
                        req.path,
                        status.as_u16(),
                        start.elapsed().as_millis()
                    );
                }
                send_error(&mut stream, status);
                return;
            }
        };

        match any_response.0 {
            AnyResponseImpl::Regular(resp) => {
                let status = resp.status_code();
                if let Err(e) = resp.send(&mut stream, keep_alive) {
                    log::error!("Failed to send response: {}", e);
                    return;
                }
                if cfg.access_log {
                    log::info!(
                        "{} {} {} {} {}ms",
                        peer_addr,
                        req.method.as_str(),
                        req.path,
                        status.as_u16(),
                        start.elapsed().as_millis()
                    );
                }
                if !keep_alive {
                    return;
                }
            }
            AnyResponseImpl::Sse(SseResponse(sse_handler)) => {
                if let Err(e) = send_sse_headers(&mut stream) {
                    log::error!("Failed to send SSE headers: {}", e);
                    return;
                }
                if cfg.access_log {
                    log::info!(
                        "{} {} {} {} SSE open",
                        peer_addr,
                        req.method.as_str(),
                        req.path,
                        StatusCode::Ok.as_u16()
                    );
                }
                match stream.try_clone() {
                    Ok(clone) => {
                        let mut writer = SseWriter::new(clone);
                        sse_handler(&mut writer);
                    }
                    Err(e) => log::error!("Failed to start SSE: {}", e),
                }
                if cfg.access_log {
                    log::info!(
                        "{} {} {} SSE closed {}ms",
                        peer_addr,
                        req.method.as_str(),
                        req.path,
                        start.elapsed().as_millis()
                    );
                }
                return;
            }
        }
    }
}

fn send_error(stream: &mut TcpStream, status_code: StatusCode) {
    let _ = Response::error(status_code).send(stream, false);
}
