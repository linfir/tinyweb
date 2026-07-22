use std::{
    io::{self, Write},
    net::TcpStream,
    time::Duration,
};

use crate::{
    date::Date,
    generated::{ContentType, HeaderName, StatusCode},
    types::HeaderValue,
};

/// A regular HTTP response.
pub struct Response {
    status_code: StatusCode,
    content_type: Option<HeaderValue>,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Vec<u8>,
}
impl Response {
    /// Returns a [`StatusCode::Ok`] response with no headers and an empty body.
    pub fn new() -> Self {
        Response {
            status_code: StatusCode::Ok,
            content_type: None,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Returns the response with the status code updated.
    pub fn with_status(mut self, status_code: StatusCode) -> Self {
        self.status_code = status_code;
        self
    }

    /// Returns the response with the given body and content type.
    pub fn with_body(mut self, content_type: ContentType, body: impl Into<Vec<u8>>) -> Self {
        self.content_type =
            Some(HeaderValue::new(content_type.into_string()).expect("valid content type"));
        self.body = body.into();
        self
    }

    /// Returns the response with an additional HTTP header.
    ///
    /// `Content-Length`, `Connection`, `Keep-Alive`, and `Date` are managed by
    /// the server and silently ignored if passed here.
    /// Use [`Response::with_body`] to set `Content-Type`.
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.add_header(name, value);
        self
    }

    fn add_header(&mut self, name: HeaderName, value: HeaderValue) {
        if name.as_str().eq_ignore_ascii_case("content-length")
            || name.as_str().eq_ignore_ascii_case("connection")
            || name.as_str().eq_ignore_ascii_case("keep-alive")
            || name.as_str().eq_ignore_ascii_case("date")
        {
            return;
        }
        if name.as_str().eq_ignore_ascii_case("content-type") {
            self.content_type = Some(value);
            return;
        }
        self.headers.push((name, value));
    }

    /// Returns a [`StatusCode::NotFound`] response with an empty body.
    pub fn not_found() -> Self {
        Self::error(StatusCode::NotFound)
    }

    /// Returns a response with the given (error) status code and an empty body.
    pub fn error(status_code: StatusCode) -> Self {
        Self::new().with_status(status_code)
    }

    /// Returns a [`StatusCode::Ok`] response with the given content type and body.
    pub fn ok(content_type: ContentType, body: impl Into<Vec<u8>>) -> Self {
        Self::new().with_body(content_type, body)
    }

    /// Returns a [`StatusCode::Ok`] response with a MIME type inferred from `ext`.
    ///
    /// `ext` should be the file extension without a leading dot (e.g. `"html"`).
    /// If the extension is unknown, `application/octet-stream` is used
    /// and a `log::warn!` diagnostic is emitted.
    pub fn file(ext: Option<&str>, body: impl Into<Vec<u8>>) -> Self {
        let mime = ContentType::from_extension(ext).unwrap_or_else(|| {
            log::warn!("Unknown file extension: {:?}", ext);
            ContentType::DEFAULT
        });
        Self::new().with_body(mime, body)
    }

    /// Returns a [`StatusCode::TemporaryRedirect`] to `to`.
    ///
    /// Use [`HeaderValue::new`] to construct the target URL,
    /// which validates that it contains no CR or LF.
    pub fn redirect(to: HeaderValue) -> Self {
        Self::new()
            .with_status(StatusCode::TemporaryRedirect)
            .with_header(HeaderName::LOCATION, to)
    }

    pub(crate) fn status_code(&self) -> StatusCode {
        self.status_code
    }

    // 1xx, 204, and 304 responses must not carry a body; sending one would
    // desync keep-alive clients (RFC 9110 s6.4.1).
    fn is_bodiless(&self) -> bool {
        let code = self.status_code.as_u16();
        matches!(code, 100..=199 | 204 | 304)
    }

    pub(crate) fn body_len(&self) -> usize {
        if self.is_bodiless() {
            0
        } else {
            self.body.len()
        }
    }

    /// Send the response over the given TCP stream.
    /// `keep_alive` is `Some(timeout)` to keep the connection open, `None` to close it.
    pub(crate) fn send(
        &self,
        stream: &TcpStream,
        keep_alive: Option<Duration>,
        send_body: bool,
        date: &Date,
    ) -> std::io::Result<()> {
        let mut w = io::BufWriter::new(stream);

        write!(
            w,
            "HTTP/1.1 {} {}\r\n",
            self.status_code.as_u16(),
            self.status_code.as_str()
        )?;

        write!(w, "Date: {}\r\n", date.http())?;
        if let Some(ct) = &self.content_type {
            write!(w, "Content-Type: {}\r\n", ct.as_str())?;
        }
        for (name, value) in &self.headers {
            write!(w, "{}: {}\r\n", name.as_str(), value.as_str())?;
        }
        let send_body = send_body && !self.is_bodiless();
        if self.is_bodiless() {
            if !self.body.is_empty() {
                log::warn!("dropping body of {} response", self.status_code.as_u16());
            }
        } else {
            write!(w, "Content-Length: {}\r\n", self.body.len())?;
        }
        if let Some(timeout) = keep_alive {
            write!(w, "Connection: keep-alive\r\n")?;
            if !timeout.is_zero() {
                write!(w, "Keep-Alive: timeout={}\r\n", timeout.as_secs())?;
            }
        } else {
            write!(w, "Connection: close\r\n")?;
        }
        write!(w, "\r\n")?;

        if send_body {
            w.write_all(&self.body)?;
        }
        w.flush()
    }
}

impl Default for Response {
    fn default() -> Self {
        Self::new()
    }
}
