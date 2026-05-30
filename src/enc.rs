pub(crate) fn percent_decode(input: &[u8]) -> Option<String> {
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
    assert_eq!(percent_decode(b"%C3%A9"), Some("é".into()));
    assert_eq!(percent_decode(b"foo%2Fbar"), Some("foo/bar".into())); // slash decoded normally
    assert_eq!(percent_decode(b"foo%2fbar"), Some("foo/bar".into()));
}
