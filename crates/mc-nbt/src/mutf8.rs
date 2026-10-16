//! Java's "modified UTF-8", used by NBT strings.
//!
//! Differences from standard UTF-8: U+0000 is written as `C0 80`, and characters
//! outside the BMP are written as a UTF-16 surrogate pair, each half as 3 bytes.

pub fn encode_mutf8(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for unit in s.encode_utf16() {
        match unit {
            0x0001..=0x007F => out.push(unit as u8),
            0x0000 | 0x0080..=0x07FF => {
                out.push(0xC0 | (unit >> 6) as u8);
                out.push(0x80 | (unit & 0x3F) as u8);
            }
            _ => {
                out.push(0xE0 | (unit >> 12) as u8);
                out.push(0x80 | ((unit >> 6) & 0x3F) as u8);
                out.push(0x80 | (unit & 0x3F) as u8);
            }
        }
    }
    out
}

/// Returns `None` on malformed input (including unpaired surrogates).
pub fn decode_mutf8(bytes: &[u8]) -> Option<String> {
    // Fast path: plain ASCII without NUL is identical in both encodings.
    if bytes.iter().all(|&b| (1..0x80).contains(&b)) {
        return Some(String::from_utf8(bytes.to_vec()).expect("ASCII is UTF-8"));
    }
    let mut units = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let continuation = |at: usize| match bytes.get(at) {
            Some(&c) if c & 0xC0 == 0x80 => Some(u16::from(c & 0x3F)),
            _ => None,
        };
        let unit = if b & 0x80 == 0 && b != 0 {
            i += 1;
            u16::from(b)
        } else if b & 0xE0 == 0xC0 {
            let unit = (u16::from(b & 0x1F) << 6) | continuation(i + 1)?;
            i += 2;
            unit
        } else if b & 0xF0 == 0xE0 {
            let unit =
                (u16::from(b & 0x0F) << 12) | (continuation(i + 1)? << 6) | continuation(i + 2)?;
            i += 3;
            unit
        } else {
            return None;
        };
        units.push(unit);
    }
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_uses_two_bytes() {
        assert_eq!(encode_mutf8("a\0b"), b"a\xc0\x80b");
        assert_eq!(decode_mutf8(b"a\xc0\x80b").as_deref(), Some("a\0b"));
    }

    #[test]
    fn supplementary_uses_surrogate_pair() {
        // U+1F600 = D83D DE00, each surrogate as 3 bytes.
        assert_eq!(encode_mutf8("😀"), b"\xed\xa0\xbd\xed\xb8\x80");
        assert_eq!(
            decode_mutf8(b"\xed\xa0\xbd\xed\xb8\x80").as_deref(),
            Some("😀")
        );
    }

    #[test]
    fn bmp_matches_utf8() {
        let s = "Привет, мир — ü";
        assert_eq!(encode_mutf8(s), s.as_bytes());
        assert_eq!(decode_mutf8(s.as_bytes()).as_deref(), Some(s));
    }

    #[test]
    fn malformed_is_rejected() {
        assert_eq!(decode_mutf8(b"\x00"), None); // raw NUL is not allowed
        assert_eq!(decode_mutf8(b"\xc3"), None); // truncated
        assert_eq!(decode_mutf8(b"\xed\xa0\xbd"), None); // lone surrogate
        assert_eq!(decode_mutf8(b"\xf0\x9f\x98\x80"), None); // 4-byte UTF-8
    }
}
