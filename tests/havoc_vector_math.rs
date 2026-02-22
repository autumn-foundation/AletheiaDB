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
                prop_assert!(val >= -1.0 && val <= 1.0, "Result {} out of range", val);
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
            // The debug_assert in ops.rs panics if result > 1.0 + 1e-3.
            if !val.is_nan() {
                 prop_assert!(val >= -1.0 && val <= 1.0, "Result {} out of range", val);
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
                prop_assert!(val >= -1.0 && val <= 1.0, "Result {} out of range", val);
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
    // It should return 1.0 (clamped).
    let res = cosine_similarity(&a, &b).unwrap();

    assert!(res >= -1.0 && res <= 1.0, "Result {} out of range", res);

    // Ideally it should be exactly 1.0 or very close
    assert!((res - 1.0).abs() < 1e-6, "Result {} should be ~1.0", res);
}
