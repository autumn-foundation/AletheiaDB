//! Deterministic, dependency-free PRNG (SplitMix64).
//!
//! The benchmark generator must be reproducible: the same seed must yield the
//! same graph on every machine. Rather than pull in the `rand` crate (whose
//! defaults are not guaranteed stable across versions), we use a tiny
//! [SplitMix64](https://prng.di.unimi.it/splitmix64.c) generator whose output
//! is fixed by this source. This is a generator for *fixture data*, not for
//! cryptography or statistics — it only needs to be uniform-ish and stable.

/// A deterministic SplitMix64 pseudo-random number generator.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Create a generator seeded with `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Return the next raw `u64`.
    pub fn next_u64(&mut self) -> u64 {
        // Constants from the reference SplitMix64 implementation.
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Return a `usize` in `[0, bound)`. `bound` must be non-zero.
    pub fn next_below(&mut self, bound: usize) -> usize {
        debug_assert!(bound > 0, "bound must be non-zero");
        (self.next_u64() % bound as u64) as usize
    }

    /// Return an `f32` in `[0.0, 1.0)`.
    pub fn next_f32(&mut self) -> f32 {
        // Use the top 24 bits for a uniform float in [0,1).
        let bits = (self.next_u64() >> 40) as u32; // 24 bits
        bits as f32 / (1u32 << 24) as f32
    }
}
