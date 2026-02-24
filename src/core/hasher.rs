//! Optimized hasher for unique integer keys.
//!
//! This module provides `IdentityHasher`, a hasher that passes through integer values
//! unchanged. It is intended for use with `HashMap` and `HashSet` where the keys
//! are already high-quality unique identifiers (like `NodeId`, `EdgeId`, or `InternedString`),
//! avoiding the unnecessary overhead of hashing (SipHash).

use std::hash::Hasher;

/// A hasher that passes through u32 and u64 values unchanged.
///
/// Used for maps where keys are already unique integers or IDs.
/// This avoids the overhead of hashing (SipHash) for lookups.
#[derive(Default)]
pub struct IdentityHasher(u64);

impl Hasher for IdentityHasher {
    fn write(&mut self, bytes: &[u8]) {
        // Fallback for types that don't call write_u32/write_u64 directly.
        match bytes.len() {
            1 => self.0 = bytes[0] as u64,
            2 => {
                // SAFETY: length checked
                let arr: [u8; 2] = bytes.try_into().unwrap();
                self.0 = u16::from_le_bytes(arr) as u64;
            }
            4 => {
                // SAFETY: length checked
                let arr: [u8; 4] = bytes.try_into().unwrap();
                self.0 = u32::from_le_bytes(arr) as u64;
            }
            8 => {
                // SAFETY: length checked
                let arr: [u8; 8] = bytes.try_into().unwrap();
                self.0 = u64::from_le_bytes(arr);
            }
            16 => {
                // Mix high and low parts for u128 to minimize collisions
                // while keeping it deterministic
                let low_arr: [u8; 8] = bytes[0..8].try_into().unwrap();
                let high_arr: [u8; 8] = bytes[8..16].try_into().unwrap();
                let low = u64::from_le_bytes(low_arr);
                let high = u64::from_le_bytes(high_arr);
                self.0 = low ^ high;
            }
            _ => {
                // Fallback for non-integer types (e.g. strings, large integers)
                // Use a simple FNV-1a hash to avoid collisions.
                //
                // WARN: IdentityHasher is intended for primitive integers. Using it
                // with strings or other types is sub-optimal but we must provide
                // a valid hash to ensure correctness (no collisions).
                debug_assert!(
                    false,
                    "IdentityHasher used with non-primitive integer type (len={})",
                    bytes.len()
                );

                let mut hash: u64 = 0xcbf29ce484222325;
                for byte in bytes {
                    hash ^= *byte as u64;
                    hash = hash.wrapping_mul(0x100000001b3);
                }
                self.0 = hash;
            }
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.0 = i as u64;
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.0 = i as u64;
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.0 = i as u64;
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.0 = i;
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.0 = i as u64;
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hasher;

    #[test]
    fn test_identity_hasher_u32() {
        let mut hasher = IdentityHasher::default();
        hasher.write_u32(42);
        assert_eq!(hasher.finish(), 42);
    }

    #[test]
    fn test_identity_hasher_u64() {
        let mut hasher = IdentityHasher::default();
        hasher.write_u64(u64::MAX);
        assert_eq!(hasher.finish(), u64::MAX);
    }

    #[test]
    fn test_identity_hasher_write_fallback_u8() {
        let mut hasher = IdentityHasher::default();
        let bytes = 123u8.to_le_bytes();
        hasher.write(&bytes);
        assert_eq!(hasher.finish(), 123);
    }

    #[test]
    fn test_identity_hasher_write_fallback_u16() {
        let mut hasher = IdentityHasher::default();
        let bytes = 12345u16.to_le_bytes();
        hasher.write(&bytes);
        assert_eq!(hasher.finish(), 12345);
    }

    #[test]
    fn test_identity_hasher_write_fallback_u32() {
        let mut hasher = IdentityHasher::default();
        let bytes = 12345u32.to_le_bytes();
        hasher.write(&bytes);
        assert_eq!(hasher.finish(), 12345);
    }

    #[test]
    fn test_identity_hasher_write_fallback_u64() {
        let mut hasher = IdentityHasher::default();
        let val = 0x1234567890ABCDEFu64;
        let bytes = val.to_le_bytes();
        hasher.write(&bytes);
        assert_eq!(hasher.finish(), val);
    }

    #[test]
    fn test_identity_hasher_write_fallback_u128() {
        let mut hasher = IdentityHasher::default();
        // u128: 0xAAAA... ^ 0x5555...
        // Low: 0x5555555555555555
        // High: 0xAAAAAAAAAAAAAAAA
        let low = 0x5555555555555555u64;
        let high = 0xAAAAAAAAAAAAAAAAu64;
        let val = (high as u128) << 64 | (low as u128);

        let bytes = val.to_le_bytes();
        hasher.write(&bytes);

        assert_eq!(hasher.finish(), low ^ high);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "IdentityHasher used with non-primitive")]
    fn test_identity_hasher_panic_on_string_debug() {
        let mut h1 = IdentityHasher::default();
        h1.write(b"foo");
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn test_identity_hasher_fallback_strings_release() {
        // Test that different strings produce different hashes (no collision)
        let mut h1 = IdentityHasher::default();
        h1.write(b"foo");

        let mut h2 = IdentityHasher::default();
        h2.write(b"bar");

        assert_ne!(h1.finish(), h2.finish());

        // Ensure not just length
        let mut h3 = IdentityHasher::default();
        h3.write(b"123"); // len 3

        // "foo" and "123" are len 3, but should hash differently
        assert_ne!(h1.finish(), h3.finish());
    }
}
