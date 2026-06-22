//! Minimal, dependency-free lowercase hex encode/decode.
//!
//! Shared by the changefeed continuation cursor (`crate::core::changefeed`) and the
//! encryption key provider so the two cannot drift on edge cases (odd length, non-hex
//! characters). Kept in `core` because it has no feature gating and is reachable from every
//! build configuration.

/// Encode bytes as a lowercase hex string (two chars per byte).
pub(crate) fn encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Decode a hex string into bytes.
///
/// Returns `None` if the string has odd length or contains a non-hex character. Accepts both
/// lowercase and uppercase hex digits.
pub(crate) fn decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        let hi = hex_val(chunk[0])?;
        let lo = hex_val(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

/// Convert a single ASCII hex digit to its value, or `None` if not a hex digit.
fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let data = b"hello:world:0:42";
        assert_eq!(decode(&encode(data)).as_deref(), Some(&data[..]));
    }

    #[test]
    fn rejects_odd_length() {
        assert!(decode("abc").is_none());
    }

    #[test]
    fn rejects_non_hex() {
        assert!(decode("zz").is_none());
    }

    #[test]
    fn accepts_uppercase() {
        assert_eq!(decode("FF").as_deref(), Some(&[0xffu8][..]));
    }
}
