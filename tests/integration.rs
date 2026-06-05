use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    time::Duration,
};

use tinyweb::{Config, Method, Request, Response};

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
fn test_handler_panic_returns_500() {
    let port = start_server(|_req| panic!("test panic"), Config::default());

    let resp = raw_request(port, b"GET /panic HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(status_line(&resp).contains("500"), "response: {}", resp);

    // worker must still be alive -- next request succeeds
    let resp = raw_request(port, b"GET /ok HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(status_line(&resp).contains("500"), "response: {}", resp);
}
