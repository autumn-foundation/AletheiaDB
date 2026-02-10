use proptest::prelude::*;
use super::simd::*;

// Constants for floating point comparison
const EPSILON: f32 = 1e-5;

// Helper function for float comparison
fn assert_approx_eq(a: f32, b: f32, context: &str) {
    if a.is_nan() && b.is_nan() {
        return;
    }
    if a.is_infinite() && b.is_infinite() {
        if a.is_sign_positive() == b.is_sign_positive() {
            return;
        }
    }
    let diff = (a - b).abs();
    // Use a mix of absolute and relative error
    if diff > EPSILON && diff > EPSILON * a.abs().max(b.abs()) {
        panic!("{} mismatch: {} vs {} (diff: {})", context, a, b, diff);
    }
}

proptest! {
    #[test]
    fn test_dot_product_sum_matches_scalar(
        a in proptest::collection::vec(proptest::num::f32::ANY, 0..1024),
        b in proptest::collection::vec(proptest::num::f32::ANY, 0..1024),
    ) {
        // Ensure same length
        let len = std::cmp::min(a.len(), b.len());
        let a = &a[0..len];
        let b = &b[0..len];

        let scalar = dot_product_scalar(a, b);
        let simd = dot_product_sum(a, b);

        assert_approx_eq(scalar, simd, "dot_product_sum");

        // Force AVX2/SSE2 checks if available on the platform
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        unsafe {
            if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
                let avx2 = super::simd::x86_ops::dot_product_avx2(a, b);
                assert_approx_eq(scalar, avx2, "dot_product_avx2");
            }
            if is_x86_feature_detected!("sse2") {
                let sse2 = super::simd::x86_ops::dot_product_sse2(a, b);
                assert_approx_eq(scalar, sse2, "dot_product_sse2");
            }
        }
    }

    #[test]
    fn test_squared_diff_sum_matches_scalar(
        a in proptest::collection::vec(proptest::num::f32::ANY, 0..1024),
        b in proptest::collection::vec(proptest::num::f32::ANY, 0..1024),
    ) {
        let len = std::cmp::min(a.len(), b.len());
        let a = &a[0..len];
        let b = &b[0..len];

        let scalar = squared_diff_sum_scalar(a, b);
        let simd = squared_diff_sum(a, b);

        assert_approx_eq(scalar, simd, "squared_diff_sum");

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        unsafe {
            if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
                let avx2 = super::simd::x86_ops::squared_diff_sum_avx2(a, b);
                assert_approx_eq(scalar, avx2, "squared_diff_sum_avx2");
            }
            if is_x86_feature_detected!("sse2") {
                let sse2 = super::simd::x86_ops::squared_diff_sum_sse2(a, b);
                assert_approx_eq(scalar, sse2, "squared_diff_sum_sse2");
            }
        }
    }

    #[test]
    fn test_dot_and_magnitudes_matches_scalar(
        a in proptest::collection::vec(proptest::num::f32::ANY, 0..1024),
        b in proptest::collection::vec(proptest::num::f32::ANY, 0..1024),
    ) {
        let len = std::cmp::min(a.len(), b.len());
        let a = &a[0..len];
        let b = &b[0..len];

        let (dot_s, mag_a_s, mag_b_s) = dot_and_magnitudes_scalar(a, b);
        let (dot_v, mag_a_v, mag_b_v) = dot_and_magnitudes(a, b);

        assert_approx_eq(dot_s, dot_v, "dot_and_magnitudes (dot)");
        assert_approx_eq(mag_a_s, mag_a_v, "dot_and_magnitudes (mag_a)");
        assert_approx_eq(mag_b_s, mag_b_v, "dot_and_magnitudes (mag_b)");

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        unsafe {
            if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
                let (dot, mag_a, mag_b) = super::simd::x86_ops::dot_and_magnitudes_avx2(a, b);
                assert_approx_eq(dot_s, dot, "dot_and_magnitudes_avx2 (dot)");
                assert_approx_eq(mag_a_s, mag_a, "dot_and_magnitudes_avx2 (mag_a)");
                assert_approx_eq(mag_b_s, mag_b, "dot_and_magnitudes_avx2 (mag_b)");
            }
            if is_x86_feature_detected!("sse2") {
                let (dot, mag_a, mag_b) = super::simd::x86_ops::dot_and_magnitudes_sse2(a, b);
                assert_approx_eq(dot_s, dot, "dot_and_magnitudes_sse2 (dot)");
                assert_approx_eq(mag_a_s, mag_a, "dot_and_magnitudes_sse2 (mag_a)");
                assert_approx_eq(mag_b_s, mag_b, "dot_and_magnitudes_sse2 (mag_b)");
            }
        }
    }

    #[test]
    fn test_scale_in_place_matches_scalar(
        v in proptest::collection::vec(proptest::num::f32::ANY, 0..1024),
        scalar in proptest::num::f32::ANY,
    ) {
        let mut v_scalar = v.clone();
        let mut v_simd = v.clone();

        scale_in_place_scalar(&mut v_scalar, scalar);
        scale_in_place(&mut v_simd, scalar);

        for (i, (s, v)) in v_scalar.iter().zip(v_simd.iter()).enumerate() {
            assert_approx_eq(*s, *v, &format!("scale_in_place idx {}", i));
        }

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        unsafe {
            if is_x86_feature_detected!("avx2") {
                let mut v_avx2 = v.clone();
                super::simd::x86_ops::scale_in_place_avx2(&mut v_avx2, scalar);
                for (i, (s, v)) in v_scalar.iter().zip(v_avx2.iter()).enumerate() {
                    assert_approx_eq(*s, *v, &format!("scale_in_place_avx2 idx {}", i));
                }
            }
            if is_x86_feature_detected!("sse2") {
                let mut v_sse2 = v.clone();
                super::simd::x86_ops::scale_in_place_sse2(&mut v_sse2, scalar);
                for (i, (s, v)) in v_scalar.iter().zip(v_sse2.iter()).enumerate() {
                    assert_approx_eq(*s, *v, &format!("scale_in_place_sse2 idx {}", i));
                }
            }
        }
    }
}
