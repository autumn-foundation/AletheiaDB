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
        // Try to interpret as u64 (little endian) if length matches.
        if let Ok(bytes) = bytes.try_into() {
            self.0 = u64::from_le_bytes(bytes);
        } else if let Ok(bytes) = bytes.try_into() {
            self.0 = u32::from_le_bytes(bytes) as u64;
        } else {
            // Fallback for unknown types - just use length to avoid collision on empty vs non-empty
            // This case shouldn't happen for primitive integer keys we care about.
            self.0 = bytes.len() as u64;
        }
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
    fn test_identity_hasher_write_fallback_other() {
        let mut hasher = IdentityHasher::default();
        let bytes = [1u8, 2, 3];
        hasher.write(&bytes);
        // Fallback is length
        assert_eq!(hasher.finish(), 3);
    }
}
