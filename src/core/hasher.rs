//! Optimized hasher for unique integer keys.
//!
//! This module provides `IdentityHasher`, a hasher that passes through integer values
//! unchanged. It is intended for use with `HashMap` and `HashSet` where the keys
//! are already high-quality unique identifiers (like `NodeId`, `EdgeId`, or `InternedString`),
//! avoiding the unnecessary overhead of hashing (SipHash).

use std::hash::Hasher;

/// A hasher that passes through integer values unchanged.
///
/// Used for maps where keys are already unique integers or IDs.
/// This avoids the overhead of hashing (SipHash) for lookups.
///
/// # Behavior for non-u64 types
///
/// - **u8, u16, u32, usize**: Cast to u64.
/// - **u128**: XORs high and low 64 bits.
/// - **Signed integers**: Cast to unsigned equivalent.
/// - **Byte slices**: Uses FNV-1a hashing to prevent collisions.
#[derive(Default)]
pub struct IdentityHasher(u64);

impl Hasher for IdentityHasher {
    fn write(&mut self, bytes: &[u8]) {
        // Fallback for variable length types: FNV-1a hash
        // Initial offset basis
        let mut hash = 0xcbf29ce484222325u64;
        // FNV prime
        const PRIME: u64 = 0x100000001b3;

        for &byte in bytes {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(PRIME);
        }
        self.0 = hash;
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
    fn write_u128(&mut self, i: u128) {
        self.0 = (i as u64) ^ ((i >> 64) as u64);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.0 = i as u64;
    }

    #[inline]
    fn write_i8(&mut self, i: i8) {
        self.0 = i as u64;
    }

    #[inline]
    fn write_i16(&mut self, i: i16) {
        self.0 = i as u64;
    }

    #[inline]
    fn write_i32(&mut self, i: i32) {
        self.0 = i as u64;
    }

    #[inline]
    fn write_i64(&mut self, i: i64) {
        self.0 = i as u64;
    }

    #[inline]
    fn write_i128(&mut self, i: i128) {
        self.0 = (i as u64) ^ ((i >> 64) as u64);
    }

    #[inline]
    fn write_isize(&mut self, i: isize) {
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
    fn test_identity_hasher_u8_cast() {
        let mut hasher = IdentityHasher::default();
        hasher.write_u8(42);
        assert_eq!(hasher.finish(), 42);
    }

    #[test]
    fn test_identity_hasher_fnv_bytes() {
        let mut hasher = IdentityHasher::default();
        let bytes = [1u8, 2, 3];
        hasher.write(&bytes);
        // Should not be 3 (length) anymore
        assert_ne!(hasher.finish(), 3);

        let mut hasher2 = IdentityHasher::default();
        hasher2.write(&[1u8, 2, 3]);
        assert_eq!(hasher.finish(), hasher2.finish());
    }
}

#[cfg(test)]
mod reproduction_tests {
    use super::*;
    use std::hash::Hasher;

    #[test]
    fn test_repro_u8_collision() {
        let mut h1 = IdentityHasher::default();
        h1.write_u8(1);

        let mut h2 = IdentityHasher::default();
        h2.write_u8(2);

        assert_ne!(h1.finish(), h2.finish(), "u8 values should not collide");
        assert_eq!(h1.finish(), 1);
        assert_eq!(h2.finish(), 2);
    }

    #[test]
    fn test_repro_bytes_collision() {
        let mut h1 = IdentityHasher::default();
        h1.write(&[1, 2, 3]);

        let mut h2 = IdentityHasher::default();
        h2.write(&[4, 5, 6]);

        assert_ne!(h1.finish(), h2.finish(), "Different byte arrays of same length should not collide");
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use std::hash::Hasher;

    #[test]
    fn test_identity_hasher_unsigned_coverage() {
        let mut hasher = IdentityHasher::default();

        hasher.write_u16(42);
        assert_eq!(hasher.finish(), 42);

        hasher.write_usize(12345);
        assert_eq!(hasher.finish(), 12345);

        // u128 uses XOR folding: low ^ high
        hasher.write_u128(0x0000000000000001_0000000000000002);
        // low=2, high=1 -> 2 ^ 1 = 3
        assert_eq!(hasher.finish(), 3);
    }

    #[test]
    fn test_identity_hasher_signed_coverage() {
        let mut hasher = IdentityHasher::default();

        hasher.write_i8(10);
        assert_eq!(hasher.finish(), 10);

        hasher.write_i16(20);
        assert_eq!(hasher.finish(), 20);

        hasher.write_i32(30);
        assert_eq!(hasher.finish(), 30);

        hasher.write_i64(40);
        assert_eq!(hasher.finish(), 40);

        hasher.write_isize(50);
        assert_eq!(hasher.finish(), 50);

        // i128 uses XOR folding: low ^ high
        // 1 << 64 is high bit 1, low bits 0.
        // 2 is low bits 2.
        let val: i128 = (1 << 64) | 2;
        hasher.write_i128(val);
        // low=2, high=1 -> 2 ^ 1 = 3
        assert_eq!(hasher.finish(), 3);
    }
}
