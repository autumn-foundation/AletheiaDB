//! 🛡️ Sentry Differential Tests for SIMD Vector Operations
//!
//! These tests strictly verify that SIMD-accelerated implementations (AVX2, SSE2)
//! produce identical results to the reference scalar implementation.
//!
//! # Coverage
//! - Remainder handling (vectors with length not divisible by 8/4)
//! - Special floating point values (NaN, Infinity, -0.0)
//! - Consistency between Scalar vs SSE2 vs AVX2

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use crate::core::vector::simd::x86_ops;
use crate::core::vector::simd::{
    dot_and_magnitudes_scalar, dot_product_scalar, squared_diff_sum_scalar,
};

/// Helper to run differential test for a given pair of vectors.
///
/// Compares Scalar vs SSE2 (if available) vs AVX2 (if available).
fn verify_simd_consistency(a: &[f32], b: &[f32], context: &str) {
    assert_eq!(
        a.len(),
        b.len(),
        "Vectors must be same length for test helper"
    );

    // 1. Reference Scalar Results
    let scalar_dot = dot_product_scalar(a, b);
    let scalar_sq_diff = squared_diff_sum_scalar(a, b);
    let (scalar_dot_mag, scalar_mag_a, scalar_mag_b) = dot_and_magnitudes_scalar(a, b);

    // Verify scalar internal consistency
    // Use assert_float_eq to handle NaN/Inf correctly (assert_eq! fails on NaN != NaN)
    assert_float_eq(
        scalar_dot,
        scalar_dot_mag,
        "Scalar dot product implementations differ",
        context,
    );

    // 2. SSE2 Checks
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if is_x86_feature_detected!("sse2") {
        unsafe {
            let sse2_dot = x86_ops::dot_product_sse2(a, b);
            let sse2_sq_diff = x86_ops::squared_diff_sum_sse2(a, b);
            let (sse2_dot_mag, sse2_mag_a, sse2_mag_b) = x86_ops::dot_and_magnitudes_sse2(a, b);

            assert_float_eq(scalar_dot, sse2_dot, "Scalar vs SSE2 dot_product", context);
            assert_float_eq(
                scalar_sq_diff,
                sse2_sq_diff,
                "Scalar vs SSE2 squared_diff_sum",
                context,
            );
            assert_float_eq(
                scalar_dot_mag,
                sse2_dot_mag,
                "Scalar vs SSE2 dot_and_magnitudes (dot)",
                context,
            );
            assert_float_eq(
                scalar_mag_a,
                sse2_mag_a,
                "Scalar vs SSE2 dot_and_magnitudes (mag_a)",
                context,
            );
            assert_float_eq(
                scalar_mag_b,
                sse2_mag_b,
                "Scalar vs SSE2 dot_and_magnitudes (mag_b)",
                context,
            );
        }
    }

    // 3. AVX2 Checks
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
        unsafe {
            let avx2_dot = x86_ops::dot_product_avx2(a, b);
            let avx2_sq_diff = x86_ops::squared_diff_sum_avx2(a, b);
            let (avx2_dot_mag, avx2_mag_a, avx2_mag_b) = x86_ops::dot_and_magnitudes_avx2(a, b);

            assert_float_eq(scalar_dot, avx2_dot, "Scalar vs AVX2 dot_product", context);
            assert_float_eq(
                scalar_sq_diff,
                avx2_sq_diff,
                "Scalar vs AVX2 squared_diff_sum",
                context,
            );
            assert_float_eq(
                scalar_dot_mag,
                avx2_dot_mag,
                "Scalar vs AVX2 dot_and_magnitudes (dot)",
                context,
            );
            assert_float_eq(
                scalar_mag_a,
                avx2_mag_a,
                "Scalar vs AVX2 dot_and_magnitudes (mag_a)",
                context,
            );
            assert_float_eq(
                scalar_mag_b,
                avx2_mag_b,
                "Scalar vs AVX2 dot_and_magnitudes (mag_b)",
                context,
            );
        }
    }
}

/// Robust floating point equality check that handles NaN and Infinity.
fn assert_float_eq(a: f32, b: f32, metric: &str, context: &str) {
    if a.is_nan() {
        assert!(
            b.is_nan(),
            "{}: Expected NaN, got {:?} ({})",
            metric,
            b,
            context
        );
        return;
    }
    if b.is_nan() {
        panic!("{}: Expected {:?}, got NaN ({})", metric, a, context);
    }
    if a.is_infinite() {
        assert_eq!(a, b, "{}: Infinite values differ ({})", metric, context);
        return;
    }

    // Allow small epsilon difference due to association reordering in SIMD
    // SIMD horizontal sums may add numbers in different order than scalar loop
    const EPSILON: f32 = 1e-5;
    let diff = (a - b).abs();
    assert!(
        diff < EPSILON,
        "{}: Mismatch scalar={:?} simd={:?} diff={:?} ({})",
        metric,
        a,
        b,
        diff,
        context
    );
}

#[test]
fn test_simd_consistency_prime_lengths() {
    // Test lengths that force heavy usage of remainder loops
    // SSE2 chunks = 4, AVX2 chunks = 8
    // Length 17: AVX2 (2 chunks + 1 rem), SSE2 (4 chunks + 1 rem)
    // Length 31: AVX2 (3 chunks + 7 rem), SSE2 (7 chunks + 3 rem)
    let lengths = [1, 3, 4, 7, 8, 9, 15, 17, 31, 63, 65];

    for len in lengths {
        let a: Vec<f32> = (0..len).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..len).map(|i| (i as f32) * 0.5).collect();

        verify_simd_consistency(&a, &b, &format!("len={}", len));
    }
}

#[test]
fn test_simd_consistency_special_values() {
    // NaN propagation
    let v_nan = vec![1.0, 2.0, f32::NAN, 4.0];
    let v_normal = vec![1.0, 1.0, 1.0, 1.0];
    verify_simd_consistency(&v_nan, &v_normal, "contains NaN");

    // Infinity propagation
    let v_inf = vec![1.0, f32::INFINITY, 3.0, 4.0];
    verify_simd_consistency(&v_inf, &v_normal, "contains Infinity");

    // Negative Infinity
    let v_neg_inf = vec![1.0, f32::NEG_INFINITY, 3.0, 4.0];
    verify_simd_consistency(&v_neg_inf, &v_normal, "contains -Infinity");

    // Zero handling (subnormals check)
    let v_zero = vec![0.0, -0.0, 1e-40, 0.0];
    verify_simd_consistency(&v_zero, &v_normal, "zeros and subnormals");
}

#[test]
fn test_simd_consistency_large_vectors() {
    // Verify accumulation behavior over larger arrays
    let len = 1024 + 7; // Large prime-ish length
    let a: Vec<f32> = (0..len).map(|i| (i % 10) as f32).collect();
    let b: Vec<f32> = (0..len).map(|i| ((i + 5) % 10) as f32).collect();

    verify_simd_consistency(&a, &b, "large vector");
}
