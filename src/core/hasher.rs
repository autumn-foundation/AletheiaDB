//! Optimized hasher for unique integer keys.
//!
//! This module provides `IdentityHasher`, a hasher that passes through integer values
//! unchanged. It is intended for use with `HashMap` and `HashSet` where the keys
//! are already high-quality unique identifiers (like `NodeId`, `EdgeId`, or `InternedString`),
//! avoiding the unnecessary overhead of hashing (SipHash).

use std::hash::Hasher;

const FNV_PRIME: u64 = 0x100000001b3;
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;

/// A highly optimized hasher for pre-hashed or unique integer keys.
///
/// This hasher implements a "pass-through" strategy for `u64` and `u32` values,
/// treating the input integer directly as the hash code. This eliminates the
/// CPU overhead of cryptographic hash functions (like SipHash) when the keys
/// are already high-quality random identifiers (e.g., `NodeId`, `EdgeId`, `InternedString`).
///
/// # Performance
///
/// - **Integers**: O(1), single instruction (move).
/// - **Strings/Bytes**: Fallback to FNV-1a (slower, but provided for safety).
///
/// # Safety
///
/// This hasher is **not** resistant to HashDoS attacks. It should only be used
/// with internal IDs or trusted input. Do not use this for `HashMap`s exposed
/// to untrusted user input where keys could be crafted to cause collisions.
///
/// # Examples
///
/// ```rust
/// use std::collections::HashMap;
/// use std::hash::BuildHasherDefault;
/// use aletheiadb::core::hasher::IdentityHasher;
///
/// // Create a HashMap optimized for integer keys
/// let mut map: HashMap<u64, String, BuildHasherDefault<IdentityHasher>> =
///     HashMap::with_hasher(BuildHasherDefault::default());
///
/// map.insert(42, "meaning of life".to_string());
/// ```
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
            self.0 ^= val;
            self.0 = self.0.wrapping_mul(FNV_PRIME);
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
    fn test_identity_hasher_composite_writes() {
        let mut h = IdentityHasher::default();
        h.write_u64(1);
        h.write_u64(2);

        // Should NOT be 2 (overwrite)
        assert_ne!(h.finish(), 2, "Hasher should mix values, not overwrite");

        // Expected: (1 ^ 2) * FNV_PRIME
        // write(1): self.0 = 1.
        // write(2): self.0 = (1 ^ 2) * FNV_PRIME = 3 * FNV_PRIME.
        let expected = 3u64.wrapping_mul(FNV_PRIME);
        assert_eq!(h.finish(), expected);
    }

    #[test]
    fn test_identity_hasher_bitwise_update_logic() {
        let mut h = IdentityHasher::default();
        h.update_state(42);
        assert_eq!(h.finish(), 42);

        h.update_state(7);
        // Correct is bitwise XOR: (42 ^ 7) * FNV_PRIME = 45 * FNV_PRIME
        let expected = 45u64.wrapping_mul(FNV_PRIME);
        assert_eq!(h.finish(), expected);

        let mut h2 = IdentityHasher::default();
        h2.update_state(1);
        assert_eq!(h2.finish(), 1);
    }

    #[test]
    fn test_identity_hasher_write_length_variations() {
        // Assert exactly what happens for specific sized writes
        let mut h1 = IdentityHasher::default();
        h1.write(&[0x12]);
        assert_eq!(h1.finish(), 0x12);

        let mut h2 = IdentityHasher::default();
        h2.write(&[0x12, 0x34]);
        assert_eq!(h2.finish(), 0x3412);

        let mut h4 = IdentityHasher::default();
        h4.write(&[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(h4.finish(), 0x78563412);

        let mut h8 = IdentityHasher::default();
        h8.write(&[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]);
        assert_eq!(h8.finish(), 0xF0DEBC9A78563412);

        let mut h16 = IdentityHasher::default();
        h16.write(&[
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
            0xFF, 0x00,
        ]);
        let low = 0x8877665544332211u64;
        let high = 0x00FFEEDDCCBBAA99u64;
        assert_eq!(h16.finish(), low ^ high);
    }

    #[test]
    fn test_identity_hasher_empty_and_fallback_states() {
        let mut hasher = IdentityHasher::default();
        hasher.write(&[]);
        // Explicitly assert it doesn't default to 0
        assert_ne!(hasher.finish(), 0);

        // Fallback for an unhandled byte length (e.g. 3) does not collision
        let mut h3 = IdentityHasher::default();
        h3.write(&[0x01, 0x02, 0x03]);
        let h3_val = h3.finish();
        assert_ne!(h3_val, 0);

        // Dirty FNV chaining
        let mut h3_dirty = IdentityHasher::default();
        h3_dirty.update_state(0x42);
        h3_dirty.write(&[0x01, 0x02, 0x03]);
        assert_ne!(h3_dirty.finish(), h3_val);
    }

    #[test]
    fn test_identity_hasher_exact_primitives() {
        let mut h8 = IdentityHasher::default();
        h8.write_u8(1);
        assert_eq!(h8.finish(), 1);

        let mut h16 = IdentityHasher::default();
        h16.write_u16(2);
        assert_eq!(h16.finish(), 2);

        let mut h32 = IdentityHasher::default();
        h32.write_u32(3);
        assert_eq!(h32.finish(), 3);

        let mut h64 = IdentityHasher::default();
        h64.write_u64(4);
        assert_eq!(h64.finish(), 4);

        let mut husize = IdentityHasher::default();
        husize.write_usize(5);
        assert_eq!(husize.finish(), 5);

        let mut h_empty = IdentityHasher::default();
        h_empty.write_u64(0);
        assert_eq!(h_empty.finish(), 0);
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

    #[test]
    fn test_identity_hasher_update_state_mutants() {
        let mut hasher = IdentityHasher::default();

        // Test first branch (self.0 == 0)
        hasher.update_state(42);
        assert_eq!(hasher.finish(), 42);

        // Test else branch (self.0 != 0)
        hasher.update_state(10);
        let expected = 42u64 ^ 10u64;
        let expected = expected.wrapping_mul(FNV_PRIME);
        assert_eq!(hasher.finish(), expected);
    }

    #[test]
    fn test_identity_hasher_write_len_1() {
        let mut hasher = IdentityHasher::default();
        hasher.write(&[42u8]);
        assert_eq!(hasher.finish(), 42);
    }

    #[test]
    fn test_identity_hasher_write_len_2() {
        let mut hasher = IdentityHasher::default();
        let bytes = 12345u16.to_le_bytes();
        hasher.write(&bytes);
        assert_eq!(hasher.finish(), 12345);
    }

    #[test]
    fn test_identity_hasher_write_len_4() {
        let mut hasher = IdentityHasher::default();
        let bytes = 123456789u32.to_le_bytes();
        hasher.write(&bytes);
        assert_eq!(hasher.finish(), 123456789);
    }

    #[test]
    fn test_identity_hasher_write_len_8() {
        let mut hasher = IdentityHasher::default();
        let bytes = 123456789012345u64.to_le_bytes();
        hasher.write(&bytes);
        assert_eq!(hasher.finish(), 123456789012345);
    }

    #[test]
    fn test_identity_hasher_write_len_16() {
        let mut hasher = IdentityHasher::default();
        let low = 0x1111111111111111u64;
        let high = 0x2222222222222222u64;
        let mut bytes = [0u8; 16];
        bytes[0..8].copy_from_slice(&low.to_le_bytes());
        bytes[8..16].copy_from_slice(&high.to_le_bytes());
        hasher.write(&bytes);
        assert_eq!(hasher.finish(), low ^ high);
    }

    #[test]
    fn test_identity_hasher_write_fallback_fnv() {
        let mut hasher = IdentityHasher::default();
        // length 3 falls into the _ arm
        let bytes = [1u8, 2u8, 3u8];
        hasher.write(&bytes);

        let mut expected = FNV_OFFSET_BASIS;
        expected ^= 1;
        expected = expected.wrapping_mul(FNV_PRIME);
        expected ^= 2;
        expected = expected.wrapping_mul(FNV_PRIME);
        expected ^= 3;
        expected = expected.wrapping_mul(FNV_PRIME);

        assert_eq!(hasher.finish(), expected);
    }

    #[test]
    fn test_identity_hasher_write_fallback_fnv_dirty() {
        let mut hasher = IdentityHasher::default();
        hasher.update_state(42); // make it dirty

        let bytes = [1u8, 2u8, 3u8];
        hasher.write(&bytes);

        let mut expected = 42u64;
        expected ^= 1;
        expected = expected.wrapping_mul(FNV_PRIME);
        expected ^= 2;
        expected = expected.wrapping_mul(FNV_PRIME);
        expected ^= 3;
        expected = expected.wrapping_mul(FNV_PRIME);

        assert_eq!(hasher.finish(), expected);
    }

    #[test]
    fn test_identity_hasher_write_u8() {
        let mut hasher = IdentityHasher::default();
        hasher.write_u8(42);
        assert_eq!(hasher.finish(), 42);
    }

    #[test]
    fn test_identity_hasher_write_u16() {
        let mut hasher = IdentityHasher::default();
        hasher.write_u16(42);
        assert_eq!(hasher.finish(), 42);
    }

    #[test]
    fn test_identity_hasher_write_u32() {
        let mut hasher = IdentityHasher::default();
        hasher.write_u32(42);
        assert_eq!(hasher.finish(), 42);
    }

    #[test]
    fn test_identity_hasher_write_u64() {
        let mut hasher = IdentityHasher::default();
        hasher.write_u64(42);
        assert_eq!(hasher.finish(), 42);
    }

    #[test]
    fn test_identity_hasher_write_usize() {
        let mut hasher = IdentityHasher::default();
        hasher.write_usize(42);
        assert_eq!(hasher.finish(), 42);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// A reference FNV-1a implementation for comparison.
    fn reference_fnv1a(bytes: &[u8]) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        for &byte in bytes {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    proptest! {
        /// Property: IdentityHasher's fallback logic for arbitrary byte slices exactly matches a
        /// standard FNV-1a implementation when starting from an empty (0) state.
        ///
        /// This verifies the `_ =>` arm in `IdentityHasher::write`.
        #[test]
        fn prop_identity_hasher_fallback_matches_fnv1a(
            bytes in prop::collection::vec(any::<u8>(), 0..100)
        ) {
            // IdentityHasher's fast paths trigger on exact lengths: 1, 2, 4, 8, 16.
            // When testing the FNV fallback, we ignore these fast-path lengths.
            let is_fast_path = matches!(bytes.len(), 1 | 2 | 4 | 8 | 16);
            if !is_fast_path {
                let mut hasher = IdentityHasher::default();
                hasher.write(&bytes);
                let actual = hasher.finish();

                let expected = if bytes.is_empty() {
                    FNV_OFFSET_BASIS // If bytes is empty but we hit `_ =>`, we do `self.0 = FNV_OFFSET_BASIS` and return.
                } else {
                    reference_fnv1a(&bytes)
                };

                prop_assert_eq!(
                    actual,
                    expected,
                    "IdentityHasher FNV fallback failed for length {}",
                    bytes.len()
                );
            }
        }

        /// Property: IdentityHasher chains FNV-1a correctly if the state is already dirty.
        #[test]
        fn prop_identity_hasher_fallback_chained(
            bytes in prop::collection::vec(any::<u8>(), 1..100)
        ) {
            let is_fast_path = matches!(bytes.len(), 1 | 2 | 4 | 8 | 16);
            if !is_fast_path {
                let mut hasher = IdentityHasher::default();
                // Dirty the state
                hasher.write_u32(42);

                // Fallback will now chain onto this dirty state
                hasher.write(&bytes);
                let actual = hasher.finish();

                let mut expected_state = 42u64;
                for &byte in bytes.iter() {
                    expected_state ^= byte as u64;
                    expected_state = expected_state.wrapping_mul(FNV_PRIME);
                }

                prop_assert_eq!(actual, expected_state);
            }
        }
    }
}

#[cfg(test)]
mod sentinel_hasher_tests {
    use super::*;
    use std::hash::Hasher;

    #[test]
    fn test_sentinel_update_state_exact_bounds() {
        let mut h = IdentityHasher::default();
        h.update_state(42);
        assert_eq!(h.0, 42); // Replaces empty implementation check

        h.update_state(0);
        assert_eq!(h.0, 42u64.wrapping_mul(FNV_PRIME)); // Checks `^=` vs `|=` and `&=`

        let mut h2 = IdentityHasher(5);
        h2.update_state(3);
        // (5 ^ 3) * FNV_PRIME
        let expected = (5u64 ^ 3u64).wrapping_mul(FNV_PRIME);
        assert_eq!(h2.0, expected);

        // Ensure that ^= wasn't changed to &= (5 & 3 = 1)
        assert_ne!(h2.0, (5u64 & 3u64).wrapping_mul(FNV_PRIME));

        // Ensure that ^= wasn't changed to |= (5 | 3 = 7)
        assert_ne!(h2.0, (5u64 | 3u64).wrapping_mul(FNV_PRIME));
    }

    #[test]
    fn test_sentinel_write_exact_match_arms() {
        // len 1
        let mut h = IdentityHasher::default();
        h.write(&[0xFF]);
        assert_eq!(h.finish(), 255);

        // len 2
        let mut h = IdentityHasher::default();
        h.write(&[0x12, 0x34]);
        assert_eq!(h.finish(), 0x3412);

        // len 4
        let mut h = IdentityHasher::default();
        h.write(&[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(h.finish(), 0x78563412);

        // len 8
        let mut h = IdentityHasher::default();
        h.write(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        assert_eq!(h.finish(), 0x8877665544332211);

        // len 16
        let mut h = IdentityHasher::default();
        let bytes: [u8; 16] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
            0x17, 0x18,
        ];
        h.write(&bytes);
        let low = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let high = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let expected = low ^ high;
        assert_eq!(h.finish(), expected);
        assert_ne!(h.finish(), low | high);
        assert_ne!(h.finish(), low & high);
    }

    #[test]
    fn test_sentinel_write_fallback_bitwise() {
        let mut h = IdentityHasher::default();
        h.write(&[0x11, 0x22, 0x33]); // len 3, falls through to _
        let mut expected = FNV_OFFSET_BASIS;
        expected ^= 0x11;
        expected = expected.wrapping_mul(FNV_PRIME);
        expected ^= 0x22;
        expected = expected.wrapping_mul(FNV_PRIME);
        expected ^= 0x33;
        expected = expected.wrapping_mul(FNV_PRIME);
        assert_eq!(h.finish(), expected);

        let mut bad_expected_and = FNV_OFFSET_BASIS;
        bad_expected_and &= 0x11;
        bad_expected_and = bad_expected_and.wrapping_mul(FNV_PRIME);
        bad_expected_and &= 0x22;
        bad_expected_and = bad_expected_and.wrapping_mul(FNV_PRIME);
        bad_expected_and &= 0x33;
        bad_expected_and = bad_expected_and.wrapping_mul(FNV_PRIME);
        assert_ne!(h.finish(), bad_expected_and);

        let mut bad_expected_or = FNV_OFFSET_BASIS;
        bad_expected_or |= 0x11;
        bad_expected_or = bad_expected_or.wrapping_mul(FNV_PRIME);
        bad_expected_or |= 0x22;
        bad_expected_or = bad_expected_or.wrapping_mul(FNV_PRIME);
        bad_expected_or |= 0x33;
        bad_expected_or = bad_expected_or.wrapping_mul(FNV_PRIME);
        assert_ne!(h.finish(), bad_expected_or);
    }

    #[test]
    fn test_sentinel_primitive_writes_not_empty() {
        let mut h1 = IdentityHasher::default();
        h1.write_u8(1);
        assert_eq!(h1.0, 1);

        let mut h2 = IdentityHasher::default();
        h2.write_u16(2);
        assert_eq!(h2.0, 2);

        let mut h3 = IdentityHasher::default();
        h3.write_u32(3);
        assert_eq!(h3.0, 3);

        let mut h4 = IdentityHasher::default();
        h4.write_u64(4);
        assert_eq!(h4.0, 4);

        let mut h5 = IdentityHasher::default();
        h5.write_usize(5);
        assert_eq!(h5.0, 5);

        assert_eq!(h5.finish(), 5);
        assert_ne!(h5.finish(), 0);
        assert_ne!(h5.finish(), 1);
    }
}
