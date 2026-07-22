use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::Duration,
};

use tinyweb::{AnyResponse, Config, Method, Request, Response, SseResponse};

fn start_server<F>(handler: F, config: Config) -> u16
where
    F: Fn(&Request) -> Response + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        tinyweb::serve(listener, config, handler);
    });
    std::thread::sleep(Duration::from_millis(100));
    port
}

fn start_server_graceful<F, R>(
    handler: F,
    config: Config,
) -> (u16, mpsc::Sender<()>, JoinHandle<()>)
where
    F: Fn(&Request) -> R + Send + Sync + 'static,
    R: Into<AnyResponse>,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let join = std::thread::spawn(move || {
        tinyweb::serve_graceful(listener, config, handler, stop_rx);
    });
    std::thread::sleep(Duration::from_millis(100));
    (port, stop_tx, join)
}

fn raw_request(port: u16, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    stream.write_all(request).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn status_line(response: &str) -> &str {
    response.lines().next().unwrap_or("")
}

#[test]
fn test_post_form_body_parsed() {
    let captured = Arc::new(Mutex::new(None::<(Vec<String>, Vec<String>)>));
    let captured2 = captured.clone();
    let port = start_server(
        move |req| {
            if req.method == Method::POST && req.path == "/form" {
                let name = req.form.get("name").cloned().unwrap_or_default();
                let msg = req.form.get("msg").cloned().unwrap_or_default();
                *captured2.lock().unwrap() = Some((name, msg));
                Response::ok(tinyweb::ContentType::PLAIN, "ok")
            } else {
                Response::not_found()
            }
        },
        Config::default(),
    );

    let body = b"name=Alice&msg=hello+world";
    let request = format!(
        "POST /form HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut req_bytes = request.into_bytes();
    req_bytes.extend_from_slice(body);
    let resp = raw_request(port, &req_bytes);
    assert!(status_line(&resp).contains("200"), "response: {}", resp);

    let (name, msg) = captured.lock().unwrap().take().unwrap();
    assert_eq!(name, ["Alice"]);
    assert_eq!(msg, ["hello world"]);
}

#[test]
fn test_post_body_exceeds_limit_returns_413() {
    let mut config = Config::default();
    config.max_body_size = 10;
    let port = start_server(
        |_req| Response::ok(tinyweb::ContentType::PLAIN, "ok"),
        config,
    );

    let body = b"name=toolongvalue";
    let request = format!(
        "POST /upload HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut req_bytes = request.into_bytes();
    req_bytes.extend_from_slice(body);
    let resp = raw_request(port, &req_bytes);
    assert!(status_line(&resp).contains("413"), "response: {}", resp);
}

#[test]
fn test_post_non_form_content_type() {
    let captured_body = Arc::new(Mutex::new(Vec::new()));
    let captured_body2 = captured_body.clone();
    let port = start_server(
        move |req| {
            *captured_body2.lock().unwrap() = req.body.clone();
            assert!(req.form.is_empty());
            Response::ok(tinyweb::ContentType::PLAIN, "ok")
        },
        Config::default(),
    );

    let body = b"hello plain";
    let request = format!(
        "POST /data HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut req_bytes = request.into_bytes();
    req_bytes.extend_from_slice(body);
    let resp = raw_request(port, &req_bytes);
    assert!(status_line(&resp).contains("200"), "response: {}", resp);
    assert_eq!(*captured_body.lock().unwrap(), body);
}

#[test]
fn test_get_request_body_and_form_empty() {
    let port = start_server(
        |req| {
            assert!(req.body.is_empty());
            assert!(req.form.is_empty());
            Response::ok(tinyweb::ContentType::PLAIN, "ok")
        },
        Config::default(),
    );

    let resp = raw_request(port, b"GET /hello HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(status_line(&resp).contains("200"), "response: {}", resp);
}

#[test]
fn test_post_zero_content_length() {
    let port = start_server(
        |req| {
            assert!(req.body.is_empty());
            assert!(req.form.is_empty());
            Response::ok(tinyweb::ContentType::PLAIN, "ok")
        },
        Config::default(),
    );

    let resp = raw_request(
        port,
        b"POST /empty HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
    );
    assert!(status_line(&resp).contains("200"), "response: {}", resp);
}

#[test]
fn test_post_percent_encoded_form_values() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured2 = captured.clone();
    let port = start_server(
        move |req| {
            let city = req.form.get("city").cloned().unwrap_or_default();
            *captured2.lock().unwrap() = city;
            Response::ok(tinyweb::ContentType::PLAIN, "ok")
        },
        Config::default(),
    );

    let body = b"city=San%20Francisco";
    let request = format!(
        "POST /form HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut req_bytes = request.into_bytes();
    req_bytes.extend_from_slice(body);
    let resp = raw_request(port, &req_bytes);
    assert!(status_line(&resp).contains("200"), "response: {}", resp);
    assert_eq!(*captured.lock().unwrap(), ["San Francisco"]);
}

#[test]
fn test_content_length_with_sign_rejected() {
    let port = start_server(
        |_req| Response::ok(tinyweb::ContentType::PLAIN, "ok"),
        Config::default(),
    );

    let resp = raw_request(
        port,
        b"POST /x HTTP/1.1\r\nHost: localhost\r\nContent-Length: +5\r\n\r\nhello",
    );
    assert!(status_line(&resp).contains("400"), "response: {}", resp);
}

#[test]
fn test_duplicate_host_rejected() {
    let port = start_server(
        |_req| Response::ok(tinyweb::ContentType::PLAIN, "ok"),
        Config::default(),
    );

    let resp = raw_request(
        port,
        b"GET / HTTP/1.1\r\nHost: localhost\r\nHost: evil\r\n\r\n",
    );
    assert!(status_line(&resp).contains("400"), "response: {}", resp);
}

#[test]
fn test_shutdown_and_join() {
    let (port, stop_tx, join) = start_server_graceful(
        |_req| Response::ok(tinyweb::ContentType::PLAIN, "ok"),
        Config::default(),
    );

    // server is up
    let resp = raw_request(port, b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(status_line(&resp).contains("200"), "response: {}", resp);

    stop_tx.send(()).unwrap();
    join.join().unwrap(); // must return promptly
}

#[test]
fn test_shutdown_closes_keep_alive() {
    let (port, stop_tx, join) = start_server_graceful(
        |_req| Response::ok(tinyweb::ContentType::PLAIN, "ok"),
        Config::default(),
    );

    // Hold a keep-alive connection open across the shutdown.
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).unwrap();
    let resp1 = String::from_utf8_lossy(&buf[..n]).to_lowercase();
    assert!(resp1.contains("connection: keep-alive"), "resp1: {}", resp1);

    stop_tx.send(()).unwrap();
    std::thread::sleep(Duration::from_millis(100)); // let the flag get set

    // A request sent during shutdown is served, then the connection is closed.
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let n = stream.read(&mut buf).unwrap();
    let resp2 = String::from_utf8_lossy(&buf[..n]).to_lowercase();
    assert!(resp2.contains("200"), "resp2: {}", resp2);
    assert!(resp2.contains("connection: close"), "resp2: {}", resp2);

    join.join().unwrap();
}

#[test]
fn test_shutdown_drains_idle_keep_alive_promptly() {
    let mut config = Config::default();
    config.idle_timeout = Duration::from_secs(30);
    config.shutdown_timeout = Some(Duration::from_secs(10));
    let (port, stop_tx, join) = start_server_graceful(
        |_req| Response::ok(tinyweb::ContentType::PLAIN, "ok"),
        config,
    );

    // Open a keep-alive connection and leave it idle (no second request).
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).unwrap();
    let resp1 = String::from_utf8_lossy(&buf[..n]).to_lowercase();
    assert!(resp1.contains("connection: keep-alive"), "resp1: {}", resp1);

    let start = std::time::Instant::now();
    stop_tx.send(()).unwrap();
    join.join().unwrap();
    // Must drain well under idle_timeout/shutdown_timeout, not wait them out.
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "shutdown took {:?}",
        start.elapsed()
    );
}

#[test]
fn test_shutdown_timeout_cuts_stuck_connection() {
    let mut config = Config::default();
    config.shutdown_timeout = Some(Duration::from_millis(200));
    let (port, stop_tx, join) = start_server_graceful(
        |_req| {
            // Ignores the shutdown signal entirely.
            std::thread::sleep(Duration::from_secs(5));
            Response::ok(tinyweb::ContentType::PLAIN, "late")
        },
        config,
    );

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    std::thread::sleep(Duration::from_millis(100)); // let the request reach the handler

    let start = std::time::Instant::now();
    stop_tx.send(()).unwrap();

    // The client socket must be cut at ~200ms, not held until the handler ends.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).unwrap_or(0);
    assert_eq!(n, 0, "expected the connection to be cut");
    assert!(start.elapsed() < Duration::from_secs(2));

    // serve_graceful returns after the abort grace, without joining the stuck worker.
    join.join().unwrap();
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn test_shutdown_drains_sse() {
    let (port, stop_tx, join) = start_server_graceful(
        |_req| {
            SseResponse::new(|w| {
                while !w.is_shutdown() {
                    if w.keepalive().is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
        },
        Config::default(),
    );

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    stream
        .write_all(b"GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]).to_lowercase();
    assert!(resp.contains("text/event-stream"), "resp: {}", resp);

    let start = std::time::Instant::now();
    stop_tx.send(()).unwrap();
    join.join().unwrap();
    // The handler polls is_shutdown, so the drain must beat the 2s timeout.
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "shutdown took {:?}",
        start.elapsed()
    );
}

#[test]
fn test_head_sse_sends_headers_only() {
    let (port, _stop_tx, _join) = start_server_graceful(
        |_req| {
            SseResponse::new(|w| {
                let _ = w.send("should not be sent");
            })
        },
        Config::default(),
    );

    let resp = raw_request(port, b"HEAD /events HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let lower = resp.to_lowercase();
    assert!(lower.contains("200"), "response: {}", resp);
    assert!(lower.contains("text/event-stream"), "response: {}", resp);
    assert!(!resp.contains("data:"), "response: {}", resp);
}

#[test]
fn test_204_suppresses_body_and_content_length() {
    let port = start_server(
        |_req| {
            Response::new()
                .with_status(tinyweb::StatusCode::NoContent)
                .with_body(tinyweb::ContentType::PLAIN, "leak")
        },
        Config::default(),
    );

    let resp = raw_request(port, b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(status_line(&resp).contains("204"), "response: {}", resp);
    assert!(
        !resp.to_lowercase().contains("content-length"),
        "response: {}",
        resp
    );
    assert!(!resp.contains("leak"), "response: {}", resp);
}

#[test]
fn test_304_suppresses_body_and_content_length() {
    let port = start_server(
        |_req| Response::error(tinyweb::StatusCode::NotModified),
        Config::default(),
    );

    let resp = raw_request(port, b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(status_line(&resp).contains("304"), "response: {}", resp);
    assert!(
        !resp.to_lowercase().contains("content-length"),
        "response: {}",
        resp
    );
}

#[test]
fn test_handler_panic_returns_500() {
    let port = start_server(|_req| panic!("test panic"), Config::default());

    let resp = raw_request(port, b"GET /panic HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(status_line(&resp).contains("500"), "response: {}", resp);

    // worker must still be alive -- next request succeeds
    let resp = raw_request(port, b"GET /ok HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(status_line(&resp).contains("500"), "response: {}", resp);
}
