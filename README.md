# tinyweb

[![CI](https://github.com/linfir/tinyweb/actions/workflows/ci.yml/badge.svg)](https://github.com/linfir/tinyweb/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/tinyweb)](https://crates.io/crates/tinyweb)
[![docs.rs](https://img.shields.io/docsrs/tinyweb)](https://docs.rs/tinyweb)
[![license](https://img.shields.io/crates/l/tinyweb)](LICENSE-MIT)

A minimal, synchronous HTTP/1.1 server library,
intended for local development servers;
probably fine behind a robust reverse proxy in production.

## Features

- Only depends on the [`log`](https://crates.io/crates/log) crate
- Thread pool model, no async runtime needed
- Built-in access logging
- Built-in path traversal and injection protection
- Automatic MIME type detection
- HTTP/1.1 keep-alive
- Server-Sent Events (SSE)
- WebSockets (RFC 6455)

## Limitations

Each idle keep-alive connection holds a worker thread for the duration of `idle_timeout` (default 30 s).
With the default pool size of 8-32 threads, a small number of idle connections can starve the server (slow loris).
Similarly, `write_timeout` bounds each write, not the whole response:
a client that reads slowly holds a worker thread for the duration of a large download.
Each open SSE or WebSocket connection also pins a worker thread for its whole lifetime.
A reverse proxy such as nginx mitigates the first two by buffering requests and responses and using short-lived upstream connections;
**direct internet exposure without a reverse proxy is not recommended**.

For WebSockets behind nginx, upgrade headers must be forwarded explicitly:

```text
location /ws {
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
}
```

## Usage

```rust,no_run
use std::net::TcpListener;
use tinyweb::{Config, ContentType, Method, Request, Response};

fn main() {
    env_logger::init();

    let listener = TcpListener::bind("127.0.0.1:8080").unwrap_or_else(|e| {
        eprintln!("bind: {e}");
        std::process::exit(1);
    });
    tinyweb::serve(listener, Config::default(), |req: &Request| {
        match (req.method, req.path.as_str()) {
            (Method::GET, "/") => Response::ok(ContentType::HTML, "<h1>Hello!</h1>"),
            _ => Response::not_found(),
        }
    });
}
```

## Security headers

tinyweb does not emit security headers by default; add them in your handler:

```rust,no_run
use tinyweb::{HeaderName, HeaderValue, Response};

fn secure(resp: Response) -> Response {
    resp.with_header(HeaderName::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff").unwrap())
        .with_header(HeaderName::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN").unwrap())
        .with_header(HeaderName::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin").unwrap())
}
```

If traffic is TLS-terminated by a reverse proxy, also add:

```text
Strict-Transport-Security: max-age=63072000
```

`Content-Security-Policy` is application-specific; restrict script and style sources to what your app actually uses.

## More examples

See the [`examples/`](examples/) directory:

- [`sse.rs`](examples/sse.rs) Server-Sent Events
- [`ws_echo.rs`](examples/ws_echo.rs) WebSocket echo with a browser test page
- [`routing.rs`](examples/routing.rs) path routing with [`matchit`](https://crates.io/crates/matchit)
- [`graceful_shutdown.rs`](examples/graceful_shutdown.rs) graceful shutdown via a `/quit` route

## License

Licensed under the [MIT license](LICENSE-MIT).
