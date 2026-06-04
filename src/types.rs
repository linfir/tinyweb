use std::{borrow::Cow, fmt};

pub use crate::generated::*;

impl ContentType {
    /// Returns a new content type from a MIME type string.
    ///
    /// Returns `Err` if `value` is empty, contains CR (`\r`), LF (`\n`), or
    /// NUL (`\0`), or does not have the required `type/subtype` structure.
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
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
        Ok(ContentType(ContentTypeInner::Custom(value)))
    }

    /// Returns the MIME type string (e.g. `"text/html; charset=utf-8"`).
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Converts the content type into an owned string.
    pub fn into_string(self) -> String {
        match self.0 {
            ContentTypeInner::Custom(s) => s,
            other => other.as_str().to_string(),
        }
    }
}

impl HeaderName {
    /// Returns a new header name.
    ///
    /// Returns `Err` if `name` is empty or contains a character outside the
    /// RFC 7230 `tchar` set (`A-Z`, `a-z`, `0-9`, `!#$%&'*+-.^_\`|~`).
    pub fn new(name: impl Into<String>) -> Result<Self, &'static str> {
        let name = name.into();
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
        Ok(HeaderName(HeaderNameInner::Custom(name)))
    }

    /// Returns the header name string (e.g. `"Cache-Control"`).
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Converts the header name into an owned string.
    pub fn into_string(self) -> String {
        match self.0 {
            HeaderNameInner::Custom(s) => s,
            other => other.as_str().to_string(),
        }
    }
}

/// An HTTP header value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderValue(Cow<'static, str>);

fn validate_header_value(value: &str) -> Result<(), &'static str> {
    if value.contains(['\r', '\n', '\0']) {
        Err("header value must not contain CR, LF, or NUL")
    } else {
        Ok(())
    }
}

impl HeaderValue {
    /// Returns `Err` if `value` contains CR (`\r`), LF (`\n`), or NUL (`\0`).
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        validate_header_value(&value)?;
        Ok(HeaderValue(Cow::Owned(value)))
    }

    /// Returns a `HeaderValue` from a static string without allocating.
    ///
    /// Returns `Err` if `value` contains CR (`\r`), LF (`\n`), or NUL (`\0`).
    pub fn from_static(value: &'static str) -> Result<Self, &'static str> {
        validate_header_value(value)?;
        Ok(HeaderValue(Cow::Borrowed(value)))
    }

    /// Returns the header value as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Converts the header value into an owned string.
    pub fn into_string(self) -> String {
        self.0.into_owned()
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
