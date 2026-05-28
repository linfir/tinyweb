# tinyweb

A minimal, zero-dependency (except the `log` crate), synchronous HTTP/1.1 server library
intended for local development servers,
not for production use.

## Features

- Thread-per-connection model, no async runtime needed
- Built-in path traversal and injection protection
- Automatic MIME type detection
- Server-Sent Events (SSE) support

## Usage

```rust,no_run
use tinyweb::{Config, ContentType, Method, Request, Response};

fn main() {
    tinyweb::serve("127.0.0.1:8080", Config::default(), |req: &Request| {
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
use tinyweb::{AnyResponse, Config, Method, Request, Response, SseResponse};

fn main() {
    tinyweb::serve("127.0.0.1:8080", Config::default(), |req: &Request| -> AnyResponse {
        match (req.method, req.path.as_str()) {
            (Method::GET, "/events") => SseResponse::new(|w| {
                for i in 0..10u32 {
                    thread::sleep(Duration::from_secs(1));
                    if w.send(&i.to_string()).is_err() {
                        break;
                    }
                }
            }).into(),
            _ => Response::not_found().into(),
        }
    });
}
```

## Limits

- HTTP/1.1 only; request body is not read
- 8 KB request buffer (headers only)
- Maximum 100 concurrent connections by default;
  excess receives HTTP 503
  (configurable via [`Config`])
- 5-second read and write timeouts by default
  (configurable via [`Config`])

## License

Licensed under the [MIT license](LICENSE-MIT).
