use std::net::TcpListener;

use matchit::Router;
use tinyweb::{Config, ContentType, Method, Request, Response};

fn main() {
    let mut router = Router::new();
    router.insert("/", "index").unwrap();
    router.insert("/users/:id", "user").unwrap();

    let listener = TcpListener::bind("127.0.0.1:8080").unwrap_or_else(|e| {
        eprintln!("bind: {e}");
        std::process::exit(1);
    });
    tinyweb::serve(listener, Config::default(), move |req: &Request| {
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
