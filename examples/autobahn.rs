//! Echo server for the Autobahn WebSocket testsuite:
//! docker run --rm --network host -v "$PWD/config:/config" -v "$PWD/reports:/reports" \
//!     crossbario/autobahn-testsuite wstest -m fuzzingclient -s /config/fuzzingclient.json

use std::net::TcpListener;

use tinyweb::{AnyResponse, Config, Request, Response, WsResponse};

fn main() {
    env_logger::init();

    let listener = TcpListener::bind("127.0.0.1:9002").unwrap_or_else(|e| {
        eprintln!("bind: {e}");
        std::process::exit(1);
    });
    let mut config = Config::default();
    // The suite sends messages up to 16 MB.
    config.max_ws_message_size = 64 * 1024 * 1024;
    tinyweb::serve(listener, config, |_req: &Request| -> AnyResponse {
        if !_req.upgradable() {
            return Response::not_found().into();
        }
        WsResponse::new(|ws| {
            while let Ok(Some(msg)) = ws.recv() {
                if ws.send(msg).is_err() {
                    break;
                }
            }
        })
        .into()
    });
}
