# tinyweb

A minimal, synchronous HTTP/1.1 server library
intended for local development servers,
not production use.

## Features

- Thread-per-connection model, no async runtime needed
- Built-in path traversal and injection protection
- Automatic MIME type detection
- Server-Sent Events (SSE) support

## Usage

```toml
[dependencies]
tinyweb = "0.1"
log = "0.4"
```

```rust,no_run
use tinyweb::{ContentType, Method, Request, Response};

fn main() {
    tinyweb::serve("127.0.0.1:8080", |req: &Request| {
        match (req.method, req.path.as_str()) {
            (Method::GET, "/") => Response::ok(ContentType::Html, "<h1>Hello!</h1>"),
            _ => Response::not_found(),
        }
    });
}
```

## Server-Sent Events

```rust,no_run
use std::{thread, time::Duration};
use tinyweb::{Method, Request, Response};

fn main() {
    tinyweb::serve("127.0.0.1:8080", |req: &Request| {
        match (req.method, req.path.as_str()) {
            (Method::GET, "/events") => Response::sse(|w| {
                for i in 0..10u32 {
                    thread::sleep(Duration::from_secs(1));
                    if w.send(&i.to_string()).is_err() {
                        break;
                    }
                }
            }),
            _ => Response::not_found(),
        }
    });
}
```

## Limits

- HTTP/1.1 only; request body is not read
- 8 KB request buffer (headers only)
- Maximum 100 concurrent connections; excess receives HTTP 503 error code
- 5-second read and write timeouts

## License

Licensed under the [MIT license](LICENSE-MIT).
