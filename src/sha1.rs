// From https://en.wikipedia.org/wiki/SHA-1

pub fn sha1(msg: &[u8]) -> [u8; 20] {
    let mut sha1 = Sha1::new();
    let n = msg.len();

    let msg = {
        let mut i = 0;
        while i + 64 <= n {
            let chunk: &[u8; 64] = msg[i..i + 64].try_into().unwrap();
            sha1.update_chunk(chunk);
            i += 64;
        }
        &msg[i..]
    };

    let k = msg.len(); // k <= 63
    let mut chunk = [0u8; 64];
    chunk[..k].copy_from_slice(msg);
    chunk[k] = 0x80;
    if k >= 56 {
        sha1.update_chunk(&chunk);
        chunk.fill(0);
    }
    let len_bits = (n as u64) * 8;
    chunk[56..64].copy_from_slice(&len_bits.to_be_bytes());
    sha1.update_chunk(&chunk);

    sha1.result()
}

#[cfg(test)]
fn sha1_hex(msg: &[u8]) -> String {
    let mut out = String::with_capacity(40);
    for byte in sha1(msg) {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

struct Sha1 {
    h: [u32; 5],
}

impl Sha1 {
    pub fn new() -> Self {
        Sha1 {
            h: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0],
        }
    }

    fn update_chunk(&mut self, chunk: &[u8; 64]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[4 * i..4 * i + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let mut a = self.h[0];
        let mut b = self.h[1];
        let mut c = self.h[2];
        let mut d = self.h[3];
        let mut e = self.h[4];

        #[allow(clippy::needless_range_loop)]
        for i in 0..80 {
            let f: u32;
            let k: u32;
            if i < 20 {
                f = (b & c) | (!b & d);
                k = 0x5a827999;
            } else if i < 40 {
                f = b ^ c ^ d;
                k = 0x6ed9eba1;
            } else if i < 60 {
                f = (b & c) | (b & d) | (c & d);
                k = 0x8f1bbcdc;
            } else {
                f = b ^ c ^ d;
                k = 0xca62c1d6;
            }

            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
    }

    fn result(self) -> [u8; 20] {
        let mut out = [0u8; 20];
        for (i, &h) in self.h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&h.to_be_bytes());
        }
        out
    }
}

#[test]
fn test_rfc_3174() {
    assert_eq!(
        sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
    );

    {
        let mut input = String::new();
        for _ in 0..1_000_000 {
            input.push('a');
        }
        assert_eq!(
            sha1_hex(input.as_bytes()),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }

    {
        let mut input = String::new();
        for _ in 0..80 {
            input.push_str("01234567");
        }

        assert_eq!(
            sha1_hex(input.as_bytes()),
            "dea356a2cddd90c7a7ecedc5ebb563934f460452"
        );
    }
}

#[test]
fn test_wikipedia() {
    assert_eq!(
        sha1_hex(b"The quick brown fox jumps over the lazy dog"),
        "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
    );
    assert_eq!(
        sha1_hex(b"The quick brown fox jumps over the lazy cog"),
        "de9f2c7fd25e1b3afad3e85a0bd17d9b100db4b3"
    );
    assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
}
