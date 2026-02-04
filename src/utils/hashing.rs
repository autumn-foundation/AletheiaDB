//! Hashing utilities for performance optimization.
//!
//! This module provides specialized hashers for cases where the default SipHash
//! overhead is unnecessary, such as when keys are already unique integers (IDs).

use std::hash::Hasher;

/// A hasher that passes through integer values unchanged.
///
/// This is designed for use with `HashMap` or `DashMap` where the keys are
/// already unique identifiers (like `NodeId`, `u64`, `u32`). By avoiding the
/// mixing steps of cryptographic hash functions (like SipHash), we achieve
/// significantly faster lookups on hot paths.
///
/// # Safety
///
/// This hasher should **ONLY** be used when:
/// 1. Keys are already unique/random-ish or sequential
/// 2. Protection against HashDoS attacks is not required for these specific maps
/// 3. The distribution of keys is known to be good (e.g., sequential IDs map perfectly to power-of-2 capacity buckets)
#[derive(Debug, Default, Clone, Copy)]
pub struct IdentityHasher(u64);

impl Hasher for IdentityHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // Fallback for types that don't use specific write_* methods.
        // We try to interpret the bytes as a u64 or u32 if lengths match.
        // This covers cases where `Hash` implementation calls `write` directly.
        match bytes.len() {
            8 => {
                // SAFETY: We checked length is 8.
                let val = u64::from_le_bytes(bytes.try_into().unwrap());
                self.0 = val;
            }
            4 => {
                // SAFETY: We checked length is 4.
                let val = u32::from_le_bytes(bytes.try_into().unwrap());
                self.0 = val as u64;
            }
            _ => {
                // For other lengths, we just use the length mixed with bytes.
                // This shouldn't happen for our use case (IDs), but we need a safe fallback.
                let mut hash = bytes.len() as u64;
                for &b in bytes {
                    hash = hash.wrapping_add(b as u64);
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

    #[test]
    fn test_write_methods() {
        let mut hasher = IdentityHasher::default();

        hasher.write_u8(42);
        assert_eq!(hasher.finish(), 42);

        hasher.write_u16(1000);
        assert_eq!(hasher.finish(), 1000);

        hasher.write_u32(100_000);
        assert_eq!(hasher.finish(), 100_000);

        hasher.write_u64(10_000_000_000);
        assert_eq!(hasher.finish(), 10_000_000_000);

        hasher.write_usize(500);
        assert_eq!(hasher.finish(), 500);
    }

    #[test]
    fn test_write_fallback() {
        let mut hasher = IdentityHasher::default();

        // Length 3 (not 4 or 8)
        let bytes = [1u8, 2, 3];
        hasher.write(&bytes);

        // Expected: len(3) + 1 + 2 + 3 = 9
        // Logic:
        // hash = 3
        // hash += 1 => 4
        // hash += 2 => 6
        // hash += 3 => 9
        assert_eq!(hasher.finish(), 9);
    }

    #[test]
    fn test_write_exact_matches() {
        let mut hasher = IdentityHasher::default();

        // 4 bytes
        let bytes4 = 12345u32.to_le_bytes();
        hasher.write(&bytes4);
        assert_eq!(hasher.finish(), 12345);

        // 8 bytes
        let bytes8 = 1234567890123456789u64.to_le_bytes();
        hasher.write(&bytes8);
        assert_eq!(hasher.finish(), 1234567890123456789);
    }
}
