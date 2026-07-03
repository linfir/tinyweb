use std::{net::TcpListener, sync::mpsc};

use tinyweb::{Config, ContentType, Method, Request, Response};

fn main() {
    env_logger::init();

    let listener = TcpListener::bind("127.0.0.1:8080").unwrap_or_else(|e| {
        eprintln!("bind: {e}");
        std::process::exit(1);
    });

    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    let stop_for_ctrlc = stop_tx.clone();
    ctrlc::set_handler(move || {
        let _ = stop_for_ctrlc.send(());
    })
    .expect("failed to set Ctrl+C handler");

    tinyweb::serve_graceful(
        listener,
        Config::default(),
        move |req: &Request| match (req.method, req.path.as_str()) {
            (Method::GET, "/") => Response::ok(ContentType::HTML, "<h1>Hello!</h1>"),
            (Method::GET, "/quit") => {
                let _ = stop_tx.send(());
                Response::ok(ContentType::PLAIN, "Shutting down...")
            }
            _ => Response::not_found(),
        },
        stop_rx,
    );
}
