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
    /// Default: 5 seconds.
    pub read_timeout: Duration,
    /// Timeout for writing the response.
    /// Default: 5 seconds.
    pub write_timeout: Duration,
    /// Emit a `log::info!` line for every completed request (peer IP, method, path, status, latency).
    /// Default: `true`.
    pub access_log: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            pool_size: {
                let cpus = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4);
                (2 * cpus).clamp(8, 16)
            },
            read_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(5),
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
    let start = Instant::now();
    let req = match Request::read(&stream, cfg, peer_addr) {
        Ok(r) => r,
        Err(status) => {
            send_error(stream, status);
            if cfg.access_log {
                log::info!(
                    "{} - - {} {}ms",
                    peer_addr,
                    status.as_u16(),
                    start.elapsed().as_millis()
                );
            }
            return;
        }
    };

    if !cfg.write_timeout.is_zero() {
        stream.set_write_timeout(Some(cfg.write_timeout)).unwrap();
    }

    match req_handler(&req).into().0 {
        AnyResponseImpl::Regular(resp) => {
            let status = resp.status_code();
            if let Err(e) = resp.send(stream) {
                log::error!("Failed to send response: {}", e);
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
        }
        AnyResponseImpl::Sse(SseResponse(sse_handler)) => {
            if let Err(e) = send_sse_headers(&mut stream) {
                log::error!("Failed to send SSE headers: {}", e);
                return;
            }
            if cfg.access_log {
                log::info!(
                    "{} {} {} 200 SSE open",
                    peer_addr,
                    req.method.as_str(),
                    req.path
                );
            }
            let mut writer = SseWriter::new(stream);
            sse_handler(&mut writer);
            if cfg.access_log {
                log::info!(
                    "{} {} {} SSE closed {}ms",
                    peer_addr,
                    req.method.as_str(),
                    req.path,
                    start.elapsed().as_millis()
                );
            }
        }
    }
}

fn send_error(stream: TcpStream, status_code: StatusCode) {
    let _ = Response::error(status_code).send(stream);
}
