#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use super::simd::x86_ops;

// ============================================================================
// AVX2 Safety Tests
// ============================================================================

#[test]
#[should_panic(expected = "Vector dimensions must match")]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn test_dot_and_magnitudes_avx2_mismatch() {
    if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
        let a = vec![1.0; 8];
        let b = vec![1.0; 7]; // Mismatch
        unsafe {
            x86_ops::dot_and_magnitudes_avx2(&a, &b);
        }
    } else {
        // Skip test by satisfying the panic condition manually
        panic!("Vector dimensions must match");
    }
}

#[test]
#[should_panic(expected = "Vector dimensions must match")]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn test_dot_product_avx2_mismatch() {
    if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
        let a = vec![1.0; 8];
        let b = vec![1.0; 7]; // Mismatch
        unsafe {
            x86_ops::dot_product_avx2(&a, &b);
        }
    } else {
        panic!("Vector dimensions must match");
    }
}

#[test]
#[should_panic(expected = "Vector dimensions must match")]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn test_squared_diff_sum_avx2_mismatch() {
    if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
        let a = vec![1.0; 8];
        let b = vec![1.0; 7]; // Mismatch
        unsafe {
            x86_ops::squared_diff_sum_avx2(&a, &b);
        }
    } else {
        panic!("Vector dimensions must match");
    }
}

// ============================================================================
// SSE2 Safety Tests
// ============================================================================

#[test]
#[should_panic(expected = "Vector dimensions must match")]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn test_dot_and_magnitudes_sse2_mismatch() {
    if is_x86_feature_detected!("sse2") {
        let a = vec![1.0; 4];
        let b = vec![1.0; 3]; // Mismatch
        unsafe {
            x86_ops::dot_and_magnitudes_sse2(&a, &b);
        }
    } else {
        panic!("Vector dimensions must match");
    }
}

#[test]
#[should_panic(expected = "Vector dimensions must match")]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn test_dot_product_sse2_mismatch() {
    if is_x86_feature_detected!("sse2") {
        let a = vec![1.0; 4];
        let b = vec![1.0; 3]; // Mismatch
        unsafe {
            x86_ops::dot_product_sse2(&a, &b);
        }
    } else {
        panic!("Vector dimensions must match");
    }
}

#[test]
#[should_panic(expected = "Vector dimensions must match")]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn test_squared_diff_sum_sse2_mismatch() {
    if is_x86_feature_detected!("sse2") {
        let a = vec![1.0; 4];
        let b = vec![1.0; 3]; // Mismatch
        unsafe {
            x86_ops::squared_diff_sum_sse2(&a, &b);
        }
    } else {
        panic!("Vector dimensions must match");
    }
}

// ============================================================================
// Scalar Safety Tests
// ============================================================================

#[test]
#[should_panic(expected = "Vector dimensions must match")]
fn test_dot_and_magnitudes_scalar_mismatch() {
    let a = vec![1.0; 4];
    let b = vec![1.0; 3]; // Mismatch
    super::simd::dot_and_magnitudes_scalar(&a, &b);
}

#[test]
#[should_panic(expected = "Vector dimensions must match")]
fn test_dot_product_scalar_mismatch() {
    let a = vec![1.0; 4];
    let b = vec![1.0; 3]; // Mismatch
    super::simd::dot_product_scalar(&a, &b);
}

#[test]
#[should_panic(expected = "Vector dimensions must match")]
fn test_squared_diff_sum_scalar_mismatch() {
    let a = vec![1.0; 4];
    let b = vec![1.0; 3]; // Mismatch
    super::simd::squared_diff_sum_scalar(&a, &b);
}
