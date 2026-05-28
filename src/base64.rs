const B64: &[u8; 64] = b"\
    ABCDEFGHIJKLMNOPQRSTUVWXYZ\
    abcdefghijklmnopqrstuvwxyz\
    0123456789\
    +/";

pub fn encode(data: &[u8]) -> String {
    let n = data.len();
    let mut output = String::with_capacity(n.div_ceil(3) * 4);

    let mut i = 0;
    while i + 3 <= n {
        encode_3bytes(&mut output, data[i..i + 3].try_into().unwrap());
        i += 3;
    }

    // Padding
    if i < n {
        let mut buf = [0u8; 3];
        buf[..n - i].copy_from_slice(&data[i..]);
        encode_3bytes(&mut output, &buf);
        let pad = 3 - (n - i);
        for _ in 0..pad {
            output.pop();
        }
        for _ in 0..pad {
            output.push('=');
        }
    }

    output
}

fn encode_3bytes(out: &mut String, src: &[u8; 3]) {
    out.push(B64[(src[0] >> 2) as usize] as char);
    out.push(B64[((src[0] & 0x03) << 4 | (src[1] >> 4)) as usize] as char);
    out.push(B64[((src[1] & 0x0f) << 2 | (src[2] >> 6)) as usize] as char);
    out.push(B64[(src[2] & 0x3f) as usize] as char);
}

#[test]
fn test_encode() {
    assert_eq!(encode(b""), "");
    assert_eq!(encode(b"f"), "Zg==");
    assert_eq!(encode(b"fo"), "Zm8=");
    assert_eq!(encode(b"foo"), "Zm9v");
    assert_eq!(encode(b"foob"), "Zm9vYg==");
    assert_eq!(encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(encode(b"hello"), "aGVsbG8=");
    assert_eq!(encode(&[0xff, 0xee, 0xdd, 0xcc]), "/+7dzA==");
    assert_eq!(encode(b"\x00\x01\x02"), "AAEC");
}
