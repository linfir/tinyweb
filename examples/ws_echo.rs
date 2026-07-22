use std::net::TcpListener;

use tinyweb::{AnyResponse, Config, ContentType, Method, Request, Response, WsResponse};

const PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>tinyweb ws echo</title>
<input id="msg" placeholder="type and press enter" autofocus>
<pre id="log"></pre>
<script>
const log = (s) => document.getElementById("log").textContent += s + "\n";
const ws = new WebSocket(`ws://${location.host}/ws`);
ws.onopen = () => log("connected");
ws.onmessage = (e) => log("< " + e.data);
ws.onclose = (e) => log(`closed (${e.code})`);
document.getElementById("msg").addEventListener("change", (e) => {
  log("> " + e.target.value);
  ws.send(e.target.value);
  e.target.value = "";
});
</script>
"#;

fn main() {
    env_logger::init();

    let listener = TcpListener::bind("127.0.0.1:8080").unwrap_or_else(|e| {
        eprintln!("bind: {e}");
        std::process::exit(1);
    });
    tinyweb::serve(
        listener,
        Config::default(),
        |req: &Request| -> AnyResponse {
            match (req.method, req.path.as_str()) {
                (Method::GET, "/") => Response::ok(ContentType::HTML, PAGE).into(),
                (Method::GET, "/ws") => WsResponse::new(|ws| {
                    while let Ok(Some(msg)) = ws.recv() {
                        if ws.send(msg).is_err() {
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
