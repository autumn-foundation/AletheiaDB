//! Property-based testing for Vector SIMD operations.
//!
//! # HAVOC: SIMD SAFETY
//! This test uses `proptest` to generate random vectors (including NaN, Inf)
//! to verify that SIMD operations (AVX2/SSE2) do not segfault or panic unexpectedly.
//!
//! We explicitly test with random lengths to exercise both the SIMD vector loop
//! and the scalar tail processing loop.

use aletheiadb::core::vector::{
    cosine_similarity, dot_product, euclidean_distance, normalize, squared_euclidean_distance,
};
use proptest::prelude::*;

proptest! {
    #[test]
    fn fuzz_cosine_similarity(
        (vec1, vec2) in (0usize..1024).prop_flat_map(|len| {
            (proptest::collection::vec(any::<f32>(), len),
             proptest::collection::vec(any::<f32>(), len))
        })
    ) {
        // Address Comment ID 2804352134: Unwrap result and remove redundant emptiness check.
        // Address Comment ID 2804354932: Generated vectors now always have matching lengths.
        let _ = cosine_similarity(&vec1, &vec2).unwrap();
    }

    #[test]
    fn fuzz_dot_product(
        (vec1, vec2) in (0usize..1024).prop_flat_map(|len| {
            (proptest::collection::vec(any::<f32>(), len),
             proptest::collection::vec(any::<f32>(), len))
        })
    ) {
        let _ = dot_product(&vec1, &vec2);
    }

    #[test]
    fn fuzz_euclidean_distance(
        (vec1, vec2) in (0usize..1024).prop_flat_map(|len| {
            (proptest::collection::vec(any::<f32>(), len),
             proptest::collection::vec(any::<f32>(), len))
        })
    ) {
        let _ = euclidean_distance(&vec1, &vec2);
        let _ = squared_euclidean_distance(&vec1, &vec2);
    }

    #[test]
    fn fuzz_normalize(vec in proptest::collection::vec(any::<f32>(), 0..1024)) {
        let _ = normalize(&vec);
    }
}
