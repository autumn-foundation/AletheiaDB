#![allow(clippy::collapsible_if)]
use aletheiadb::core::vector::ops::cosine_similarity;
use proptest::prelude::*;

proptest! {
    // Basic fuzzing with "normal" floats
    #[test]
    fn test_cosine_similarity_no_panic(
        (a, b) in proptest::collection::vec(proptest::num::f32::NORMAL, 0..100)
            .prop_flat_map(|v| {
                let len = v.len();
                (Just(v), proptest::collection::vec(proptest::num::f32::NORMAL, len..=len))
            })
    ) {
        let res = cosine_similarity(&a, &b);
        if let Ok(val) = res {
            if !val.is_nan() {
                prop_assert!((-1.0..=1.0).contains(&val), "Result {} out of range", val);
            }
        }
    }

    // Chaos fuzzing with "ANY" floats (NaN, Inf, Subnormal)
    #[test]
    fn test_cosine_similarity_chaos(
        (a, b) in proptest::collection::vec(proptest::num::f32::ANY, 0..100)
            .prop_flat_map(|v| {
                let len = v.len();
                (Just(v), proptest::collection::vec(proptest::num::f32::ANY, len..=len))
            })
    ) {
        let res = cosine_similarity(&a, &b);
        if let Ok(val) = res {
            // NaN is acceptable if inputs are garbage, but panic is not.
            // The debug_assert in ops.rs panics if result > 1.0 + 1e-2.
            if !val.is_nan() {
                 prop_assert!((-1.0..=1.0).contains(&val), "Result {} out of range", val);
            }
        }
    }

    // Targeted precision attack: very small numbers (denormals)
    #[test]
    fn test_cosine_similarity_precision(
        (a, b) in proptest::collection::vec(-1e-30_f32..1e-30_f32, 1..100)
            .prop_flat_map(|v| {
                let len = v.len();
                (Just(v), proptest::collection::vec(-1e-30_f32..1e-30_f32, len..=len))
            })
    ) {
        let res = cosine_similarity(&a, &b);
        if let Ok(val) = res {
            if !val.is_nan() {
                prop_assert!((-1.0..=1.0).contains(&val), "Result {} out of range", val);
            }
        }
    }
}

#[test]
fn test_cosine_similarity_repro_regression() {
    // Specific values known to cause precision issues (from repro_panic.rs)
    // These produce dot / mag > 1.0
    let a = vec![-8.161245e-22f32];
    let b = vec![-125.53673f32];

    // This call should NOT panic, even in debug mode.
    // Previously it returned 1.0 (clamped).
    // Now, since `a` has squared magnitude < 1e-14, it is treated as a zero vector.
    // This ensures consistency with normalize().
    let res = cosine_similarity(&a, &b).unwrap();

    assert!((-1.0..=1.0).contains(&res), "Result {} out of range", res);

    // Should be 0.0 because `a` is effectively zero
    assert_eq!(res, 0.0, "Result {} should be 0.0 (noise vector)", res);
}
