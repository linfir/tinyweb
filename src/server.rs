use std::{
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use crate::{
    log,
    request::Request,
    response::Response,
    sse::{SseResponse, SseWriter, send_sse_headers},
    types::StatusCode,
};

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
    let config = Arc::new(config);
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
                let config = config.clone();
                thread::spawn(move || {
                    let _guard = ActiveGuard(active);
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
    let req = match Request::read(&stream, cfg) {
        Ok(r) => r,
        Err(status) => {
            send_error(stream, status);
            return;
        }
    };

    if !cfg.write_timeout.is_zero() {
        stream.set_write_timeout(Some(cfg.write_timeout)).unwrap();
    }

    match req_handler(&req).into().0 {
        AnyResponseImpl::Regular(resp) => {
            if let Err(e) = resp.send(stream) {
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

fn send_error(stream: TcpStream, status_code: StatusCode) {
    let _ = Response::error(status_code).send(stream);
}
