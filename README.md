# tinyweb

A minimal, synchronous HTTP/1.1 server library,
intended for local development servers;
probably fine behind a robust reverse proxy in production.

## Features

- Only depends on the [`log`](https://crates.io/crates/log) crate
- Thread pool model, no async runtime needed
- Built-in path traversal and injection protection
- Automatic MIME type detection
- Server-Sent Events (SSE) support

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

- [`sse.rs`](examples/sse.rs) — Server-Sent Events
- [`routing.rs`](examples/routing.rs) — path routing with [`matchit`](https://crates.io/crates/matchit)

## License

Licensed under the [MIT license](LICENSE-MIT).
