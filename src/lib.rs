#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

mod enc;
mod log;
mod server;
mod sse;

use std::fmt;

pub use crate::{server::*, sse::SseWriter};

include!(concat!(env!("OUT_DIR"), "/generated.rs"));

/// A content type (MIME type) for HTTP responses.
#[derive(Debug, Clone)]
pub struct ContentType(ContentTypeInner);

impl ContentType {
    /// Returns a custom content type from a MIME type string.
    ///
    /// Returns `Err` if `value` is empty, contains CR (`\r`), LF (`\n`), or
    /// NUL (`\0`), or does not have the required `type/subtype` structure.
    pub fn custom(value: &str) -> Result<Self, &'static str> {
        if value.is_empty() {
            return Err("MIME type must not be empty");
        }
        if value.contains(['\r', '\n', '\0']) {
            return Err("MIME type must not contain CR, LF, or NUL");
        }
        let slash = value.find('/').ok_or("MIME type must contain '/'")?;
        if slash == 0 {
            return Err("MIME type must have a non-empty type before '/'");
        }
        let subtype = value[slash + 1..].split(';').next().unwrap_or("").trim();
        if subtype.is_empty() {
            return Err("MIME type must have a non-empty subtype after '/'");
        }
        Ok(ContentType(ContentTypeInner::Custom(value.to_string())))
    }

    /// Returns the MIME type string (e.g. `"text/html; charset=utf-8"`).
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn into_string(self) -> String {
        match self.0 {
            ContentTypeInner::Custom(s) => s,
            other => other.as_str().to_string(),
        }
    }
}

/// An HTTP header name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderName(HeaderNameInner);

impl HeaderName {
    /// Returns a custom header name.
    ///
    /// Returns `Err` if `name` is empty or contains a character outside the
    /// RFC 7230 `tchar` set (`A–Z`, `a–z`, `0–9`, `!#$%&'*+-.^_\`|~`).
    pub fn custom(name: &str) -> Result<Self, &'static str> {
        if name.is_empty()
            || !name.bytes().all(|b| {
                matches!(b,
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
                    | b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*'
                    | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
                )
            })
        {
            return Err("header name contains invalid characters");
        }
        Ok(HeaderName(HeaderNameInner::Custom(name.to_string())))
    }

    /// Returns the header name string (e.g. `"Cache-Control"`).
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
