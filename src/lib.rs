#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

mod enc;
mod server;
mod sse;

pub use crate::{server::*, sse::SseWriter};

include!(concat!(env!("OUT_DIR"), "/generated.rs"));

/// An HTTP header name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderName(HeaderNameInner);

impl HeaderName {
    /// Returns a custom header name.
    ///
    /// Returns `Err` if `name` contains `\r`, `\n`, or `:`.
    pub fn custom(name: &str) -> Result<Self, &'static str> {
        if name.contains(['\r', '\n', ':']) {
            return Err("header name contains invalid characters");
        }
        Ok(HeaderName(HeaderNameInner::Custom(name.to_string())))
    }

    /// Returns the header name string (e.g. `"Cache-Control"`).
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
