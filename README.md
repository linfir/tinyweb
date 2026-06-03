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

## Usage

```rust,no_run
use tinyweb::{Config, ContentType, Method, Request, Response};

fn main() {
    tinyweb::serve("127.0.0.1:8080", Config::default(), |req: &Request| {
        match (req.method, req.path.as_str()) {
            (Method::GET, "/") => Response::ok(ContentType::HTML, "<h1>Hello!</h1>"),
            _ => Response::not_found(),
        }
    });
}
```

## More examples

See the [`examples/`](examples/) directory:

- [`sse.rs`](examples/sse.rs) Server-Sent Events
- [`routing.rs`](examples/routing.rs) path routing with [`matchit`](https://crates.io/crates/matchit)

## License

Licensed under the [MIT license](LICENSE-MIT).
