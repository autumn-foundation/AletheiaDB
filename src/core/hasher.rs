//! Optimized hashers for internal integer id keys.
//!
//! Two types, and picking the wrong one has cost real throughput before:
//!
//! - [`IdHashBuilder`] (backed by [`IdHasher`]) is what id-keyed maps and sets
//!   should use. **Reach for this one.**
//! - [`IdentityHasher`] is the raw pass-through it is built on, kept public
//!   because it is a useful primitive, but it must not be handed to a `DashMap`
//!   — see below.
//!
//! Both avoid the overhead of hashing (SipHash) keys that are already unique
//! identifiers (like `NodeId`, `EdgeId`, or `InternedString`).
//!
//! # Why not just pass the id through?
//!
//! Because AletheiaDB's ids are *sequential*, not random, and two things read
//! the high bits of a hash: `DashMap` chooses a shard from them, and
//! `hashbrown` derives its SIMD control byte from them. A sequential id has no
//! high bits set, so a pass-through hash puts **every** entry in shard 0 and
//! gives every entry the same control byte. [`IdHasher`] adds a single
//! finalizing multiply that fixes both while staying ~1 cycle.
//!
//! # The Story of IdentityHasher
//!
//! In a database engine, we frequently use HashMaps and HashSets to look up entities
//! by their unique integer IDs. The default Rust hasher (SipHash) is designed to protect
//! against HashDoS attacks by scrambling the bits using a cryptographic algorithm.
//!
//! However, when our keys are already randomly distributed 64-bit integers generated
//! internally (like a `NodeId` or `EdgeId`), hashing them *again* provides zero benefit
//! and costs valuable CPU cycles.
//!
//! [`IdentityHasher`] bypasses this overhead by simply taking the integer key and using
//! it directly as the hash value. This single optimization saves a significant amount
//! of CPU time during massive graph traversals and index lookups.
//!
//! # Safety Warning
//!
//! **Do not use this hasher for maps exposed to untrusted user input.**
//! It is intentionally vulnerable to HashDoS attacks. Only use it for internal identifiers
//! or data where collisions cannot be maliciously crafted.
//!
//! # Examples
//!
//! ```rust
//! use std::collections::HashMap;
//! use std::hash::BuildHasherDefault;
//! use aletheiadb::core::hasher::IdentityHasher;
//!
//! // Create a HashMap optimized for integer keys
//! let mut map: HashMap<u64, String, BuildHasherDefault<IdentityHasher>> =
//!     HashMap::with_hasher(BuildHasherDefault::default());
//!
//! map.insert(42, "meaning of life".to_string());
//! ```

use std::hash::{BuildHasherDefault, Hasher};

const FNV_PRIME: u64 = 0x100000001b3;
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;

/// Multiplier for [`IdHasher`]'s finalizer: 2^64 / phi, rounded to the nearest
/// odd integer. This is Knuth's multiplicative ("Fibonacci") hash constant.
///
/// Odd, therefore coprime with 2^64, therefore `x -> x.wrapping_mul(FIB)` is a
/// *bijection* on `u64`. Two distinct ids can never be mapped onto the same
/// hash, so the finalizer cannot introduce a collision that identity hashing
/// would have avoided.
const FIB_MULTIPLIER: u64 = 0x9E37_79B9_7F4A_7C15;

/// [`BuildHasher`](std::hash::BuildHasher) for maps/sets keyed by internal,
/// sequential ids (`NodeId`, `EdgeId`, `NamespaceId`, `InternedString`, ...).
///
/// This is the blessed builder for id-keyed maps, and it is backed by
/// [`IdHasher`] rather than [`IdentityHasher`] — see that type for why a raw
/// pass-through hash is actively harmful once the map is a `DashMap`. Avoids
/// SipHash overhead on hot-path point lookups; see [`IdentityHasher`]'s
/// Safety Warning before using it for anything keyed by untrusted input.
pub type IdHashBuilder = BuildHasherDefault<IdHasher>;

/// A one-multiply hasher for internal integer ids: [`IdentityHasher`] plus a
/// finalizing multiply that moves entropy into the **high** bits.
///
/// # Why this exists
///
/// [`IdentityHasher`] returns the key unchanged, which is ideal for the *bucket
/// index* of a hash table (that reads the low bits, and sequential ids are
/// perfectly distributed there). It is a poor hash for the two places that read
/// the **high** bits:
///
/// 1. **`DashMap` shard selection.** `DashMap` picks a shard with
///    `(hash << 7) >> shift`, where `shift = 64 - log2(shard_amount)` — that is,
///    from the top bits. A sequential id has none set, so with an identity hash
///    every entry lands in shard 0 and every read and write on the map
///    serializes on that one shard's lock. On a 16-shard map an id must exceed
///    2^53 to reach shard 1; on a 64-core box (256 shards) fifteen out of
///    sixteen shards were already idle before this, and 255 of 256 after.
/// 2. **`hashbrown`'s SIMD control byte.** The top 7 bits become the per-entry
///    tag that probing compares against. Identity-hashed sequential ids all
///    share the tag `0`, so every probe group reports a match and falls through
///    to a full key comparison.
///
/// A single `wrapping_mul` by [`FIB_MULTIPLIER`] fixes both. Because the
/// multiplier is odd the map is a bijection, so no collision is introduced;
/// because it is irrational-derived the high bits vary richly for inputs that
/// differ only in the low bits.
///
/// # Performance
///
/// One integer multiply (~1 cycle) on top of [`IdentityHasher`]. Measured
/// end-to-end through `AletheiaDB::get_node` on a 4-core box, interleaving the
/// two builds across three passes to cancel machine drift
/// (`examples/concurrency_scaling.rs`):
///
/// | threads | pass-through | `IdHasher` | |
/// |--------:|-------------:|-----------:|--|
/// | 1 | 17.6M/s | 17.3M/s | unchanged |
/// | 2 | 11.6M/s | 19.7M/s | 1.69x |
/// | 4 | 11.6M/s | 28.2M/s | 2.44x |
/// | 8 | 11.5M/s | 28.9M/s | 2.50x |
///
/// Single-threaded cost is in the noise; the multiply buys back roughly what
/// the improved control-byte distribution saves. Commit throughput is
/// unaffected (writes are bound by the commit clock and fsync, not by hashing).
///
/// # Safety
///
/// Inherits [`IdentityHasher`]'s HashDoS exposure: an attacker who chooses keys
/// can still force collisions, since the finalizer is a public bijection. Use
/// it for internal ids only.
///
/// # Examples
///
/// ```rust
/// use std::hash::{BuildHasher, Hasher};
/// use aletheiadb::core::hasher::{IdHashBuilder, IdHasher};
///
/// // Distinct ids keep distinct hashes: the finalizer is a bijection.
/// let build = IdHashBuilder::default();
/// let hash_of = |id: u64| {
///     let mut h = build.build_hasher();
///     h.write_u64(id);
///     h.finish()
/// };
/// assert_ne!(hash_of(1), hash_of(2));
///
/// // ...but unlike an identity hash, the high bits actually move.
/// assert_ne!(hash_of(1) >> 57, hash_of(2) >> 57);
/// # let _ = IdHasher::default();
/// ```
#[derive(Default)]
pub struct IdHasher(IdentityHasher);

impl Hasher for IdHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.0.write(bytes);
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.0.write_u8(i);
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.0.write_u16(i);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.0.write_u32(i);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.0.write_u64(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.0.write_usize(i);
    }

    /// The identity hash, finalized by one multiply so the high bits vary.
    #[inline]
    fn finish(&self) -> u64 {
        self.0.finish().wrapping_mul(FIB_MULTIPLIER)
    }
}

/// A highly optimized hasher for pre-hashed or **already-random** integer keys.
///
/// This hasher implements a "pass-through" strategy for `u64` and `u32` values,
/// treating the input integer directly as the hash code. This eliminates the
/// CPU overhead of cryptographic hash functions (like SipHash) when the keys
/// are already high-quality random identifiers.
///
/// # Prefer [`IdHasher`] for AletheiaDB ids
///
/// AletheiaDB's `NodeId`/`EdgeId`/`InternedString` are allocated *sequentially*,
/// not randomly, so they have no entropy in their high bits. Passing them
/// through unchanged starves `DashMap`'s shard selection (every entry lands in
/// shard 0, serializing the whole map on one lock) and `hashbrown`'s SIMD
/// control byte. Use [`IdHashBuilder`] for those; this type is the primitive it
/// is built from, and is appropriate directly only when the keys really are
/// uniformly distributed over the full 64-bit range.
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
    /// Mixes a new value into the internal state.
    ///
    /// The goal of `update_state` is to handle both single-value hashing (like
    /// hashing a single `u64`) and composite hashing (like a struct with multiple fields).
    ///
    /// - **Single Value (Fast Path)**: If this is the first value written (`self.0 == 0`),
    ///   it simply overwrites the state. This means `hash(42)` is exactly `42`.
    /// - **Composite Value (Mixing)**: If the state is already dirty, it XORs the new
    ///   value and multiplies by `FNV_PRIME`. This ensures that `hash((1, 2))` is not
    ///   the same as `hash((2, 1))` or `hash(3)`, preventing catastrophic collisions
    ///   when hashing tuples or structs.
    ///
    /// # Arguments
    ///
    /// * `val` - The 64-bit integer to mix into the current hash state.
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
    /// Writes a slice of bytes into the hasher.
    ///
    /// While [`IdentityHasher`] is optimized for single integer writes (e.g., [`Self::write_u64`]),
    /// it must correctly handle arbitrary byte slices to satisfy the [`Hasher`] trait.
    ///
    /// # Implementation Details
    ///
    /// - **Fast Paths**: Byte slices of exactly 1, 2, 4, 8, or 16 bytes are cast directly
    ///   to their corresponding integer types and mixed into the state.
    /// - **Fallback**: For any other length (e.g., strings), the hasher falls back to a
    ///   standard FNV-1a algorithm. This is significantly slower but guarantees correctness
    ///   and avoids collisions.
    ///
    /// # Panics
    ///
    /// This function does not panic. The internal conversions from byte slices
    /// to integers use exact length checks.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::hash::Hasher;
    /// use aletheiadb::core::hasher::IdentityHasher;
    ///
    /// let mut hasher = IdentityHasher::default();
    ///
    /// // Fast path for 8 bytes
    /// hasher.write(&42u64.to_le_bytes());
    /// assert_eq!(hasher.finish(), 42);
    ///
    /// // Fallback path for strings
    /// let mut string_hasher = IdentityHasher::default();
    /// string_hasher.write(b"hello world");
    /// assert_ne!(string_hasher.finish(), 0);
    /// ```
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

    /// Writes a single `u8` into the hasher.
    ///
    /// Casts the value to `u64` and mixes it into the state.
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.update_state(i as u64);
    }

    /// Writes a single `u16` into the hasher.
    ///
    /// Casts the value to `u64` and mixes it into the state.
    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.update_state(i as u64);
    }

    /// Writes a single `u32` into the hasher.
    ///
    /// Casts the value to `u64` and mixes it into the state.
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.update_state(i as u64);
    }

    /// Writes a single `u64` into the hasher.
    ///
    /// This is the primary "happy path" for the [`IdentityHasher`]. It mixes
    /// the value directly into the state. If this is the only value written
    /// to the hasher, `finish()` will return `i` exactly.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::hash::Hasher;
    /// use aletheiadb::core::hasher::IdentityHasher;
    ///
    /// let mut hasher = IdentityHasher::default();
    /// hasher.write_u64(999);
    /// assert_eq!(hasher.finish(), 999);
    /// ```
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.update_state(i);
    }

    /// Writes a single `usize` into the hasher.
    ///
    /// Casts the value to `u64` and mixes it into the state.
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.update_state(i as u64);
    }

    /// Returns the computed hash value.
    ///
    /// If the hasher was fed a single integer, this will return that exact integer.
    /// If multiple values were fed, it will return the mixed composite hash.
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

    /// A reference FNV-1a implementation for comparison in tests.
    fn reference_fnv1a(bytes: &[u8], initial_state: u64) -> u64 {
        let mut hash = initial_state;
        for &byte in bytes {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    #[test]
    fn test_identity_hasher_write_fallback_fnv() {
        let mut hasher = IdentityHasher::default();
        // length 3 falls into the _ arm
        let bytes = [1u8, 2u8, 3u8];
        hasher.write(&bytes);

        let expected = reference_fnv1a(&bytes, FNV_OFFSET_BASIS);

        assert_eq!(hasher.finish(), expected);
    }

    #[test]
    fn test_identity_hasher_write_fallback_fnv_dirty() {
        let mut hasher = IdentityHasher::default();
        hasher.update_state(42); // make it dirty

        let bytes = [1u8, 2u8, 3u8];
        hasher.write(&bytes);

        let expected = reference_fnv1a(&bytes, 42u64);

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
mod sentinel_identity_hasher_tests {
    use super::*;
    use std::hash::Hasher;

    #[test]
    fn test_identity_hasher_update_state_bitwise_exhaustive() {
        let mut hasher = IdentityHasher::default();

        // Initial state is 0. Writing 0b1010 overwrites the state.
        // If `self.0 == 0` was mutated to `self.0 != 0`, it would try to XOR 0 and multiply by FNV_PRIME.
        // 0 ^ 0b1010 = 0b1010. 0b1010 * FNV_PRIME != 0b1010.
        hasher.update_state(0b1010);
        assert_eq!(hasher.finish(), 0b1010);

        // Now state is 0b1010. Writing 0b0101.
        // XOR: 0b1010 ^ 0b0101 = 0b1111 = 15
        // OR:  0b1010 | 0b0101 = 0b1111 = 15  (Wait, this doesn't kill OR)
        // AND: 0b1010 & 0b0101 = 0b0000 = 0

        // Let's use 0b1100 and 0b1010
        // XOR: 0b1100 ^ 0b1010 = 0b0110 (6)
        // OR:  0b1100 | 0b1010 = 0b1110 (14)
        // AND: 0b1100 & 0b1010 = 0b1000 (8)

        let mut h2 = IdentityHasher::default();
        h2.update_state(0b1100);
        assert_eq!(h2.finish(), 0b1100);

        h2.update_state(0b1010);

        let expected_xor = 0b0110u64.wrapping_mul(FNV_PRIME);
        let expected_or = 0b1110u64.wrapping_mul(FNV_PRIME);
        let expected_and = 0b1000u64.wrapping_mul(FNV_PRIME);

        assert_eq!(h2.finish(), expected_xor);
        assert_ne!(h2.finish(), expected_or);
        assert_ne!(h2.finish(), expected_and);
    }

    #[test]
    fn test_identity_hasher_write_empty_body_exhaustive() {
        let mut h = IdentityHasher::default();
        // If `write_u8` is empty, finish() is 0
        h.write_u8(42);
        assert_eq!(h.finish(), 42);
        assert_ne!(h.finish(), 0);

        let mut h = IdentityHasher::default();
        h.write_u16(42);
        assert_eq!(h.finish(), 42);
        assert_ne!(h.finish(), 0);

        let mut h = IdentityHasher::default();
        h.write_u32(42);
        assert_eq!(h.finish(), 42);
        assert_ne!(h.finish(), 0);

        let mut h = IdentityHasher::default();
        h.write_u64(42);
        assert_eq!(h.finish(), 42);
        assert_ne!(h.finish(), 0);

        let mut h = IdentityHasher::default();
        h.write_usize(42);
        assert_eq!(h.finish(), 42);
        assert_ne!(h.finish(), 0);

        // If `write` is empty, finish() is 0
        let mut h = IdentityHasher::default();
        h.write(&[42]);
        assert_eq!(h.finish(), 42);
        assert_ne!(h.finish(), 0);
    }

    #[test]
    fn test_identity_hasher_write_match_arms_exhaustive() {
        // We want to prove the fast path match arms (1, 2, 4, 8, 16) are taken.
        // If they are deleted, the code falls back to the `_ =>` FNV loop.
        // For length 1, write(42) fast path gives 42. FNV loop gives FNV_OFFSET_BASIS ^ 42 * FNV_PRIME.
        let mut h1 = IdentityHasher::default();
        h1.write(&[42]);
        assert_eq!(h1.finish(), 42); // Fast path
        let mut h1_fnv = IdentityHasher(FNV_OFFSET_BASIS);
        h1_fnv.0 ^= 42;
        h1_fnv.0 = h1_fnv.0.wrapping_mul(FNV_PRIME);
        assert_ne!(h1.finish(), h1_fnv.finish());

        let mut h2 = IdentityHasher::default();
        h2.write(&12345u16.to_le_bytes());
        assert_eq!(h2.finish(), 12345);

        let mut h4 = IdentityHasher::default();
        h4.write(&123456789u32.to_le_bytes());
        assert_eq!(h4.finish(), 123456789);

        let mut h8 = IdentityHasher::default();
        h8.write(&0x1234567890ABCDEFu64.to_le_bytes());
        assert_eq!(h8.finish(), 0x1234567890ABCDEFu64);

        // For length 16, we want to prove `low ^ high` is used, and it's XOR, not OR or AND
        // low: 0b1100, high: 0b1010
        let low = 0b1100u64;
        let high = 0b1010u64;
        let mut h16 = IdentityHasher::default();
        let mut bytes16 = [0u8; 16];
        bytes16[0..8].copy_from_slice(&low.to_le_bytes());
        bytes16[8..16].copy_from_slice(&high.to_le_bytes());
        h16.write(&bytes16);

        let expected_xor = low ^ high;
        let expected_or = low | high;
        let expected_and = low & high;

        assert_eq!(h16.finish(), expected_xor);
        assert_ne!(h16.finish(), expected_or);
        assert_ne!(h16.finish(), expected_and);
    }

    #[test]
    fn test_identity_hasher_write_fallback_bitwise_exhaustive() {
        // Test `if self.0 == 0` mutation to `!=`
        // If it's `!= 0`, it won't set FNV_OFFSET_BASIS, so starting state is 0.
        let mut h = IdentityHasher::default();
        let bytes = [42u8, 43u8, 44u8]; // length 3 hits `_ =>`
        h.write(&bytes);

        let mut expected = FNV_OFFSET_BASIS;
        expected ^= 42;
        expected = expected.wrapping_mul(FNV_PRIME);
        expected ^= 43;
        expected = expected.wrapping_mul(FNV_PRIME);
        expected ^= 44;
        expected = expected.wrapping_mul(FNV_PRIME);

        assert_eq!(h.finish(), expected);

        // Ensure starting from 0 doesn't produce the same result
        let mut wrong_expected = 0u64;
        wrong_expected ^= 42;
        wrong_expected = wrong_expected.wrapping_mul(FNV_PRIME);
        wrong_expected ^= 43;
        wrong_expected = wrong_expected.wrapping_mul(FNV_PRIME);
        wrong_expected ^= 44;
        wrong_expected = wrong_expected.wrapping_mul(FNV_PRIME);
        assert_ne!(h.finish(), wrong_expected);

        // Test loop bitwise mutations `^=` to `|=` or `&=`
        // In the loop, `self.0 ^= *byte as u64;`
        // We need byte to differentiate ^ from | and & with the current state.
        // FNV_OFFSET_BASIS is 0xcbf29ce484222325
        // Let's write `0x25` (the last byte of basis).
        // XOR: 0x...25 ^ 0x25 = 0x...00
        // OR:  0x...25 | 0x25 = 0x...25
        // AND: 0x...25 & 0x25 = 0x...25
        let mut h_bitwise = IdentityHasher::default();
        h_bitwise.write(&[0x25, 0x25, 0x25]);

        let mut expected_xor = FNV_OFFSET_BASIS;
        for &byte in &[0x25, 0x25, 0x25] {
            expected_xor ^= byte as u64;
            expected_xor = expected_xor.wrapping_mul(FNV_PRIME);
        }

        let mut expected_or = FNV_OFFSET_BASIS;
        for &byte in &[0x25, 0x25, 0x25] {
            expected_or |= byte as u64;
            expected_or = expected_or.wrapping_mul(FNV_PRIME);
        }

        let mut expected_and = FNV_OFFSET_BASIS;
        for &byte in &[0x25, 0x25, 0x25] {
            expected_and &= byte as u64;
            expected_and = expected_and.wrapping_mul(FNV_PRIME);
        }

        assert_eq!(h_bitwise.finish(), expected_xor);
        assert_ne!(h_bitwise.finish(), expected_or);
        assert_ne!(h_bitwise.finish(), expected_and);
    }

    #[test]
    fn test_identity_hasher_finish_return_exhaustive() {
        // Kill `replace finish -> u64 with 0` and `1`
        let mut h = IdentityHasher::default();
        h.write_u64(42);
        assert_eq!(h.finish(), 42);
        assert_ne!(h.finish(), 0);
        assert_ne!(h.finish(), 1);
    }
}

#[cfg(test)]
mod id_hasher_shard_distribution_tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::hash::BuildHasher;

    /// Mirrors `DashMap::determine_shard`, which is `pub(crate)` upstream:
    ///
    /// ```ignore
    /// // dashmap-6.1.0/src/lib.rs
    /// pub(crate) fn determine_shard(&self, hash: usize) -> usize {
    ///     // Leave the high 7 bits for the HashBrown SIMD tag.
    ///     (hash << 7) >> self.shift
    /// }
    /// ```
    ///
    /// `shard_amount` is `available_parallelism() * 4` rounded up to a power of
    /// two, and `shift = 64 - log2(shard_amount)`. Reproduced here rather than
    /// called so the invariant is pinned even though the real function is
    /// private; if DashMap ever changes this formula, these tests describe the
    /// property we relied on and should be revisited.
    fn determine_shard(hash: u64, shard_amount: usize) -> usize {
        let shift = usize::BITS as usize - shard_amount.trailing_zeros() as usize;
        ((hash as usize) << 7) >> shift
    }

    fn hash_with<S: BuildHasher>(build: &S, id: u64) -> u64 {
        let mut h = build.build_hasher();
        h.write_u64(id);
        h.finish()
    }

    fn shards_touched<S: BuildHasher>(
        build: &S,
        ids: impl Iterator<Item = u64>,
        n: usize,
    ) -> usize {
        ids.map(|id| determine_shard(hash_with(build, id), n))
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// The regression this whole change exists to prevent: sequential ids must
    /// reach more than one `DashMap` shard. With a pass-through hash they all
    /// land in shard 0 and every operation on the map serializes on one lock.
    #[test]
    fn sequential_ids_spread_across_dashmap_shards() {
        let ids = || 1u64..=10_000;
        let fixed = BuildHasherDefault::<IdHasher>::default();
        let broken = BuildHasherDefault::<IdentityHasher>::default();

        // Checked across every plausible machine size, since `shard_amount`
        // is derived from the core count at runtime. Bigger boxes allocate
        // *more* shards, which made the old behavior worse, not better.
        for shard_amount in [4usize, 16, 64, 256, 1024] {
            let used = shards_touched(&fixed, ids(), shard_amount);
            assert_eq!(
                used, shard_amount,
                "IdHasher should reach every one of {shard_amount} shards, reached {used}"
            );

            // Pin the bug itself, so this test explains what it is defending.
            let used_broken = shards_touched(&broken, ids(), shard_amount);
            assert_eq!(
                used_broken, 1,
                "a pass-through hash is expected to collapse to a single shard \
                 (that is the bug IdHasher fixes); it reached {used_broken}"
            );
        }
    }

    /// The finalizer multiplies by an odd constant, which is a bijection on
    /// `u64` — so it can never turn two distinct ids into one hash.
    #[test]
    fn finalizer_introduces_no_collisions() {
        let build = BuildHasherDefault::<IdHasher>::default();
        let seen: BTreeSet<u64> = (0u64..50_000).map(|id| hash_with(&build, id)).collect();
        assert_eq!(seen.len(), 50_000, "IdHasher collided on distinct ids");
    }

    /// `hashbrown` derives its per-entry SIMD control byte from the top 7 bits.
    /// Under a pass-through hash every sequential id shares the tag 0, so every
    /// probe group false-matches and falls through to a full key comparison.
    #[test]
    fn sequential_ids_vary_the_hashbrown_control_byte() {
        let fixed = BuildHasherDefault::<IdHasher>::default();
        let broken = BuildHasherDefault::<IdentityHasher>::default();

        fn distinct_tags<S: BuildHasher>(build: &S) -> usize {
            (1u64..=1000)
                .map(|id| hash_with(build, id) >> 57)
                .collect::<BTreeSet<_>>()
                .len()
        }

        let fixed_tags = distinct_tags(&fixed);
        assert!(
            fixed_tags > 100,
            "expected the control byte to vary widely, got {fixed_tags} distinct tags"
        );
        assert_eq!(
            distinct_tags(&broken),
            1,
            "pass-through hashing is expected to produce a single control byte"
        );
    }

    /// The finalizer must not *cost* us the property identity hashing was good
    /// at: multiplying by an odd constant is bijective modulo any power of two,
    /// so the low bits that drive the bucket index still cover every bucket.
    #[test]
    fn low_bits_still_cover_every_bucket() {
        let build = BuildHasherDefault::<IdHasher>::default();
        for bucket_bits in [4u32, 8, 12] {
            let buckets = 1usize << bucket_bits;
            let mask = (buckets - 1) as u64;
            let used: BTreeSet<u64> = (0..buckets as u64)
                .map(|id| hash_with(&build, id) & mask)
                .collect();
            assert_eq!(
                used.len(),
                buckets,
                "expected {buckets} distinct bucket indexes, got {}",
                used.len()
            );
        }
    }

    /// The finalizer is applied exactly once, at the end, and leaves the
    /// composite mixing `IdHasher` inherits from `IdentityHasher` untouched.
    #[test]
    fn finalizer_is_applied_once_over_inherited_mixing() {
        let mut composite = IdHasher::default();
        composite.write_u64(1);
        composite.write_u64(2);

        let mut inherited = IdentityHasher::default();
        inherited.write_u64(1);
        inherited.write_u64(2);

        assert_eq!(
            composite.finish(),
            inherited.finish().wrapping_mul(FIB_MULTIPLIER),
            "IdHasher must be exactly IdentityHasher plus one finalizing multiply"
        );

        // And it still mixes rather than letting the last write win.
        let mut single = IdHasher::default();
        single.write_u64(2);
        assert_ne!(composite.finish(), single.finish());
    }

    /// **Known limitation, pre-dating `IdHasher` and deliberately not changed
    /// here.** `IdentityHasher::update_state` assigns on the first write and
    /// then XOR-mixes, and XOR is commutative — so a two-field composite key
    /// hashes the same in either field order, contradicting `update_state`'s
    /// own doc comment ("ensures that `hash((1, 2))` is not the same as
    /// `hash((2, 1))`"). `IdHasher` inherits this.
    ///
    /// It is latent rather than live: no map in the crate is both tuple-keyed
    /// *and* id-hashed (`prop_index`'s tuple-keyed outer `DashMap` uses the
    /// default `RandomState`), so nothing currently collides. This test pins
    /// the behavior so a future fix is a deliberate, visible change rather than
    /// an accidental one.
    #[test]
    fn known_limitation_two_field_composites_ignore_field_order() {
        let mut a = IdHasher::default();
        a.write_u64(1);
        a.write_u64(2);

        let mut b = IdHasher::default();
        b.write_u64(2);
        b.write_u64(1);

        assert_eq!(
            a.finish(),
            b.finish(),
            "if this now differs, the inherited XOR mixing was fixed -- update \
             this test and IdentityHasher::update_state's doc comment together"
        );
    }

    /// Equal keys must hash equally — the contract every `HashMap` depends on.
    #[test]
    fn equal_keys_hash_equally() {
        let build = BuildHasherDefault::<IdHasher>::default();
        for id in [0u64, 1, 42, u64::MAX] {
            assert_eq!(hash_with(&build, id), hash_with(&build, id));
        }
    }
}
