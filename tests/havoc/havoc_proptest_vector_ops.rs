//! Property-based testing for Vector SIMD operations.
//!
//! # HAVOC: SIMD SAFETY
//! This test uses `proptest` to generate random vectors (including NaN, Inf)
//! to verify that SIMD operations (AVX2/SSE2) do not segfault or panic unexpectedly.

use aletheiadb::core::vector::ops::{cosine_similarity, cosine_similarity_normalized};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn fuzz_cosine_similarity(
        (vec1, vec2) in (1usize..1024).prop_flat_map(|len| {
            (proptest::collection::vec(any::<f32>(), len),
             proptest::collection::vec(any::<f32>(), len))
        })
    ) {
        let result = std::panic::catch_unwind(|| {
            let _ = cosine_similarity(&vec1, &vec2);
        });
        prop_assert!(result.is_ok(), "cosine_similarity panicked!");
    }

    #[test]
    fn fuzz_cosine_similarity_normalized(
        (vec1, vec2) in (1usize..1024).prop_flat_map(|len| {
            (proptest::collection::vec(any::<f32>(), len),
             proptest::collection::vec(any::<f32>(), len))
        })
    ) {
        let result = std::panic::catch_unwind(|| {
            let _ = cosine_similarity_normalized(&vec1, &vec2);
        });
        prop_assert!(result.is_ok(), "cosine_similarity_normalized panicked!");
    }
}
