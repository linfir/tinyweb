# tinyweb

A minimal, synchronous HTTP/1.1 server library.
Intended for local development servers;
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

## Routing

For path patterns beyond simple string matching (e.g. `/users/:id`),
[`matchit`](https://crates.io/crates/matchit) pairs well with tinyweb:

```rust,no_run
use matchit::Router;
use tinyweb::{Config, ContentType, Method, Request, Response};

fn main() {
    let mut router = Router::new();
    router.insert("/", "index").unwrap();
    router.insert("/users/:id", "user").unwrap();

    tinyweb::serve("127.0.0.1:8080", Config::default(), move |req: &Request| {
        let Ok(matched) = router.at(req.path.as_str()) else {
            return Response::not_found();
        };
        match (req.method, *matched.value) {
            (Method::GET, "index") => Response::ok(ContentType::HTML, "<h1>Hello!</h1>"),
            (Method::GET, "user") => {
                let id = matched.params.get("id").unwrap_or("unknown");
                Response::ok(ContentType::HTML, format!("<h1>User {id}</h1>"))
            }
            _ => Response::not_found(),
        }
    });
}
```

## Limits

- HTTP/1.1 only
- 8 KB request buffer (for the request head)
- Thread pool sized to the number of CPUs by default (configurable via [`Config`])
- 5-second read and write timeouts by default (configurable via [`Config`])

## License

Licensed under the [MIT license](LICENSE-MIT).
