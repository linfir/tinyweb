use std::{
    io::{self, Write},
    net::TcpStream,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
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
        self.content_type = Some(HeaderValue(content_type.into_string()));
        self.body = body.into();
        self
    }

    /// Returns the response with an additional HTTP header.
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

    /// Send the response over the given TCP stream.
    /// `keep_alive` is `Some(timeout)` to keep the connection open, `None` to close it.
    pub(crate) fn send(
        &self,
        stream: &mut TcpStream,
        keep_alive: Option<Duration>,
        send_body: bool,
    ) -> std::io::Result<()> {
        let mut w = io::BufWriter::new(&mut *stream);

        write!(
            w,
            "HTTP/1.1 {} {}\r\n",
            self.status_code.as_u16(),
            self.status_code.as_str()
        )?;

        write!(w, "Date: {}\r\n", http_date())?;
        if let Some(ct) = &self.content_type {
            write!(w, "Content-Type: {}\r\n", ct.as_str())?;
        }
        for (name, value) in &self.headers {
            write!(w, "{}: {}\r\n", name.as_str(), value.as_str())?;
        }
        write!(w, "Content-Length: {}\r\n", self.body.len())?;
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

// Formats the current UTC time as an HTTP-date (RFC 7231 §7.1.1.1).
pub(crate) fn http_date() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    http_date_from(secs)
}

fn http_date_from(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let sod = secs % 86400;
    let (h, m, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let dow = (days + 4) % 7; // epoch was a Thursday; 0 = Sunday
    // Civil date from days (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = mp + if mp < 10 { 3 } else { -9 };
    let y = y + if mo <= 2 { 1 } else { 0 };
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[dow as usize],
        d,
        MONTHS[(mo - 1) as usize],
        y,
        h,
        m,
        s,
    )
}

#[test]
fn test_http_date_from() {
    assert_eq!(http_date_from(0), "Thu, 01 Jan 1970 00:00:00 GMT");
    assert_eq!(http_date_from(86399), "Thu, 01 Jan 1970 23:59:59 GMT");
    assert_eq!(http_date_from(86400), "Fri, 02 Jan 1970 00:00:00 GMT");
    assert_eq!(http_date_from(951782400), "Tue, 29 Feb 2000 00:00:00 GMT");
    assert_eq!(http_date_from(1735732800), "Wed, 01 Jan 2025 12:00:00 GMT");
}

#[test]
fn test_http_date_from_random() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/date_test_cases.txt");
    let Ok(data) = std::fs::read_to_string(path) else {
        return;
    };
    for line in data.lines() {
        let (secs, expected) = line.split_once(' ').unwrap();
        assert_eq!(http_date_from(secs.parse().unwrap()), expected);
    }
}

impl Default for Response {
    fn default() -> Self {
        Self::new()
    }
}
