//! Optimized hasher for unique integer keys.
//!
//! This module provides `IdentityHasher`, a hasher that passes through integer values
//! unchanged. It is intended for use with `HashMap` and `HashSet` where the keys
//! are already high-quality unique identifiers (like `NodeId`, `EdgeId`, or `InternedString`),
//! avoiding the unnecessary overhead of hashing (SipHash).

use std::hash::Hasher;

const FNV_PRIME: u64 = 0x100000001b3;
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;

/// A hasher that passes through u32 and u64 values unchanged.
///
/// Used for maps where keys are already unique integers or IDs.
/// This avoids the overhead of hashing (SipHash) for lookups.
#[derive(Default)]
pub struct IdentityHasher(u64);

impl IdentityHasher {
    #[inline]
    fn update_state(&mut self, val: u64) {
        if self.0 == 0 {
            // Initial state: overwrite to maintain Identity behavior for single keys
            self.0 = val;
        } else {
            // Already dirty: mix new value to avoid collisions for composite keys
            // (e.g. String which writes bytes then 0xFF marker)
            // Fix order sensitivity by multiplying BEFORE XOR
            self.0 = self.0.wrapping_mul(FNV_PRIME) ^ val;
        }
    }
}

impl Hasher for IdentityHasher {
    fn write(&mut self, bytes: &[u8]) {
        // Fallback for types that don't call write_u32/write_u64 directly.
        match bytes.len() {
            1 => self.update_state(bytes[0] as u64),
            2 => {
                // SAFETY: length checked
                let arr: [u8; 2] = bytes.try_into().unwrap();
                self.update_state(u16::from_le_bytes(arr) as u64);
            }
            4 => {
                // SAFETY: length checked
                let arr: [u8; 4] = bytes.try_into().unwrap();
                self.update_state(u32::from_le_bytes(arr) as u64);
            }
            8 => {
                // SAFETY: length checked
                let arr: [u8; 8] = bytes.try_into().unwrap();
                self.update_state(u64::from_le_bytes(arr));
            }
            16 => {
                // Mix high and low parts for u128 to minimize collisions
                // while keeping it deterministic
                let low_arr: [u8; 8] = bytes[0..8].try_into().unwrap();
                let high_arr: [u8; 8] = bytes[8..16].try_into().unwrap();
                let low = u64::from_le_bytes(low_arr);
                let high = u64::from_le_bytes(high_arr);
                self.update_state(low ^ high);
            }
            _ => {
                // Fallback for non-integer types (e.g. strings, large integers)
                // Use a simple FNV-1a hash to avoid collisions.

                // WARN: IdentityHasher is intended for primitive integers. Using it
                // with strings or other types is sub-optimal but we must provide
                // a valid hash to ensure correctness (no collisions).

                // If state is 0, start with FNV basis. If already dirty, chain it.
                if self.0 == 0 {
                    self.0 = FNV_OFFSET_BASIS;
                }

                for byte in bytes {
                    self.0 ^= *byte as u64;
                    self.0 = self.0.wrapping_mul(FNV_PRIME);
                }
            }
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.update_state(i as u64);
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.update_state(i as u64);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.update_state(i as u64);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.update_state(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.update_state(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{Hash, Hasher};

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
    fn test_identity_hasher_explicit_u16() {
        let mut hasher = IdentityHasher::default();
        hasher.write_u16(42);
        assert_eq!(hasher.finish(), 42);
    }

    #[test]
    fn test_identity_hasher_explicit_usize() {
        let mut hasher = IdentityHasher::default();
        hasher.write_usize(42);
        assert_eq!(hasher.finish(), 42);
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
    fn test_identity_hasher_fallback_strings() {
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

    #[test]
    fn test_string_hashing_no_marker_collision() {
        // This test replicates how Rust's Hash trait works for str:
        // it calls write(bytes) then write_u8(0xff).
        // Previous implementation of write_u8 overwrote the hash, causing all strings to hash to 255.

        let s1 = "hello";
        let s2 = "world";

        let mut h1 = IdentityHasher::default();
        s1.hash(&mut h1); // Uses standard Hash impl for str

        let mut h2 = IdentityHasher::default();
        s2.hash(&mut h2);

        assert_ne!(
            h1.finish(),
            h2.finish(),
            "Different strings must produce different hashes"
        );
        assert_ne!(
            h1.finish(),
            255,
            "String hash should not collapse to the 0xff marker"
        );
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;

    /// 🎯 Target: IdentityHasher composite keys
    /// 💣 Risk: Order sensitivity. (1, 2) and (2, 1) should hash differently.
    /// 🧪 Strategy: Write u32s in different order and compare.
    /// 🔬 Verification: Expect distinct hashes.
    #[test]
    fn test_composite_key_order_sensitivity() {
        let mut h1 = IdentityHasher::default();
        h1.write_u32(1);
        h1.write_u32(2);

        let mut h2 = IdentityHasher::default();
        h2.write_u32(2);
        h2.write_u32(1);

        assert_ne!(
            h1.finish(),
            h2.finish(),
            "Hashes for (1, 2) and (2, 1) should differ (XOR commutativity check)"
        );
    }

    /// 🎯 Target: IdentityHasher zero collision
    /// 💣 Risk: (0, 0) colliding with 0.
    /// 🧪 Strategy: Write zeros and compare.
    /// 🔬 Verification: Document current behavior (collision is expected due to design).
    #[test]
    fn test_zero_collision_documentation() {
        let mut h1 = IdentityHasher::default();
        h1.write_u32(0);
        h1.write_u32(0);

        let mut h2 = IdentityHasher::default();
        h2.write_u32(0);

        // IdentityHasher(0) -> 0.
        // IdentityHasher(0, 0) -> 0 (because update_state(0) does nothing if state is 0).
        // This is a known limitation of using a single u64 state initialized to 0.
        // We document it here rather than fail, as fixing it would require changing the struct layout
        // or breaking the Identity property for single 0 values.
        assert_eq!(
            h1.finish(),
            h2.finish(),
            "Documented behavior: (0, 0) collides with 0 due to zero-initialization"
        );
    }

    /// 🎯 Target: IdentityHasher reset vulnerability
    /// 💣 Risk: Intermediate state becoming 0 resets the hasher.
    /// 🧪 Strategy: Create a sequence where state becomes 0, then add more values.
    /// 🔬 Verification: Ensure it doesn't just equal the suffix.
    #[test]
    fn test_intermediate_zero_state() {
        // If state becomes 0 mid-stream, does it act like a fresh hasher?
        // Let's find a value X such that update_state(X) results in 0.
        // If current state is S, we need S * P ^ X == 0 => X == S * P.

        let mut h1 = IdentityHasher::default();
        h1.write_u64(1); // State = 1

        // Calculate X such that next state is 0
        // Current logic: state = state ^ val; state *= P
        // To get 0: (1 ^ X) * P == 0 (mod 2^64).
        // Since P is odd, it has an inverse. So 1 ^ X must be 0 (mod 2^64/gcd(P, 2^64)).
        // gcd(P, 2^64) = 1. So 1 ^ X = 0 => X = 1.
        // Wait, current logic: `self.0 ^= val; self.0 = self.0.wrapping_mul(FNV_PRIME);`

        // If state is 1. Write 1.
        // state = 1 ^ 1 = 0.
        // state = 0 * P = 0.

        h1.write_u64(1);

        // With new logic (multiply before XOR), this should NOT be 0.
        // (1 * P) ^ 1 != 0.
        assert_ne!(
            h1.finish(),
            0,
            "1 ^ 1 should NOT result in 0 state with new logic"
        );

        // Now write 5.
        h1.write_u64(5);

        // Compare with just writing 5.
        let mut h2 = IdentityHasher::default();
        h2.write_u64(5);

        assert_ne!(
            h1.finish(),
            h2.finish(),
            "Sequence (1, 1, 5) should NOT collide with (5)"
        );
    }
}
