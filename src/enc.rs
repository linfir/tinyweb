#![allow(dead_code)]

pub fn latin1_to_utf8(input: &[u8]) -> String {
    input.iter().map(|&b| b as char).collect()
}

#[test]
fn test_latin1_to_utf8() {
    assert_eq!(latin1_to_utf8(b"hello"), "hello");
    assert_eq!(latin1_to_utf8(&[0xe9]), "\u{e9}");
}

pub fn utf8_to_latin1(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    for c in input.chars() {
        let code = c as u32;
        if code <= 0xFF {
            out.push(code as u8);
        } else {
            return None;
        }
    }
    Some(out)
}

#[test]
fn test_utf8_to_latin1() {
    assert_eq!(utf8_to_latin1("hello"), Some(b"hello".to_vec()));
    assert_eq!(utf8_to_latin1("é"), Some(vec![0xe9]));
    assert_eq!(utf8_to_latin1("€"), None);
}

pub fn percent_decode(input: &[u8]) -> Option<String> {
    let mut out = Vec::with_capacity(input.len());
    let mut chars = input.iter().copied();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next()?;
            let lo = chars.next()?;
            let hex = [hi, lo];
            let hex_str = std::str::from_utf8(&hex).ok()?;
            let val = u8::from_str_radix(hex_str, 16).ok()?;
            out.push(val);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

#[test]
fn test_percent_decode() {
    assert_eq!(
        percent_decode(b"hello%20world%21"),
        Some("hello world!".to_string())
    );
    assert_eq!(percent_decode(b"foo%2"), None);
    assert_eq!(percent_decode(b"foo%XXbar"), None);
    assert_eq!(percent_decode(b"%C3%A9"), Some("é".to_string()));
}

pub fn percent_encode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len() + 16);
    for b in input.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b),
            _ => out.extend_from_slice(format!("%{:02X}", b).as_bytes()),
        }
    }
    String::from_utf8(out).expect("Invalid UTF-8 in percent encoding")
}

#[test]
fn test_percent_encode() {
    assert_eq!(percent_encode("hello world!"), "hello%20world%21");
    assert_eq!(
        percent_encode("Jean-Paul & Saint-Étienne"),
        "Jean-Paul%20%26%20Saint-%C3%89tienne"
    );
    assert_eq!(percent_encode("AZaz09-_.~"), "AZaz09-_.~");
}
