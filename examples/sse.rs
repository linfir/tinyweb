use std::{thread, time::Duration};

use tinyweb::{AnyResponse, Config, Method, Request, Response, SseResponse};

fn main() {
    tinyweb::serve(
        "127.0.0.1:8080",
        Config::default(),
        |req: &Request| -> AnyResponse {
            match (req.method, req.path.as_str()) {
                (Method::GET, "/events") => SseResponse::new(|w| {
                    for i in 0..10u32 {
                        thread::sleep(Duration::from_secs(1));
                        if w.send(&i.to_string()).is_err() {
                            break;
                        }
                    }
                })
                .into(),
                _ => Response::not_found().into(),
            }
        },
    );
}
