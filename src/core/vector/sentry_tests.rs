use super::ops::*;
use super::simd::*;

// ============================================================================
// Unaligned Memory Access Tests
// ============================================================================

/// Helper to create a byte-aligned f32 slice.
///
/// This ensures we test unaligned loads (which `loadu` handles) vs aligned loads (which crash).
/// AVX2 requires 32-byte alignment for aligned loads.
/// SSE2 requires 16-byte alignment.
/// f32 only requires 4-byte alignment.
///
/// We construct a buffer where the f32 data starts at an offset that is 4-byte aligned
/// but definitely NOT 32-byte aligned.
fn with_unaligned_f32_slice<F>(len: usize, f: F)
where
    F: FnOnce(&[f32]),
{
    // Allocate a buffer with enough space for padding
    // We need 4 bytes for alignment offset + len * 4 bytes for data
    // plus extra to be safe
    let mut buffer = vec![0u8; 64 + len * 4];

    // Find a starting position that is 4-byte aligned but NOT 32-byte aligned
    let ptr = buffer.as_ptr() as usize;
    let mut offset = 0;
    // Use bitwise AND to check alignment to satisfy clippy::manual_is_multiple_of
    while (ptr + offset) & 3 != 0 || (ptr + offset) & 31 == 0 {
        offset += 1;
    }

    // Ensure we have enough space
    assert!(offset + len * 4 <= buffer.len());

    // Create the slice
    let slice_ptr = unsafe { buffer.as_ptr().add(offset) as *const f32 };
    let slice = unsafe { std::slice::from_raw_parts(slice_ptr, len) };

    // Verify alignment
    let slice_addr = slice.as_ptr() as usize;
    assert_eq!(slice_addr % 4, 0, "Slice must be 4-byte aligned for f32");
    assert_ne!(
        slice_addr % 32,
        0,
        "Slice must NOT be 32-byte aligned for testing unaligned loads"
    );

    // Initialize with data (safe because we allocated enough bytes)
    // We need a mutable slice to write, but we only have a const one.
    // However, we own the buffer, so we can write to it via the buffer.
    // Let's populate the buffer with f32 bytes.
    for i in 0..len {
        let val = (i as f32) * 1.0;
        let bytes = val.to_ne_bytes();
        for j in 0..4 {
            buffer[offset + i * 4 + j] = bytes[j];
        }
    }

    f(slice);
}

/// Helper to create a byte-aligned mutable f32 slice.
///
/// Identical logic to `with_unaligned_f32_slice` but yields `&mut [f32]`.
fn with_unaligned_f32_slice_mut<F>(len: usize, f: F)
where
    F: FnOnce(&mut [f32]),
{
    let mut buffer = vec![0u8; 64 + len * 4];
    let ptr = buffer.as_ptr() as usize;
    let mut offset = 0;
    while (ptr + offset) & 3 != 0 || (ptr + offset) & 31 == 0 {
        offset += 1;
    }
    assert!(offset + len * 4 <= buffer.len());

    let slice_ptr = unsafe { buffer.as_mut_ptr().add(offset) as *mut f32 };
    let slice = unsafe { std::slice::from_raw_parts_mut(slice_ptr, len) };

    for (i, val) in slice.iter_mut().enumerate() {
        *val = (i as f32) * 1.0;
    }

    f(slice);
}

#[test]
fn test_simd_unaligned_load_dot_product() {
    // Test dot product with unaligned memory
    // If the implementation uses aligned loads (vmovaps), this will crash (SIGSEGV)
    let len = 100;
    with_unaligned_f32_slice(len, |a| {
        with_unaligned_f32_slice(len, |b| {
            let result = dot_product(a, b).unwrap();
            let expected: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
            assert!(
                (result - expected).abs() < 1e-4,
                "Unaligned dot product failed: {} vs {}",
                result,
                expected
            );
        });
    });
}

#[test]
fn test_simd_unaligned_load_cosine_similarity() {
    // Test cosine similarity with unaligned memory
    let len = 128; // Multiple of 32 to trigger main loops
    with_unaligned_f32_slice(len, |a| {
        with_unaligned_f32_slice(len, |b| {
            let result = cosine_similarity(a, b).unwrap();
            // We know the data is 0..127, so it's not zero-vector
            assert!(result > 0.9, "Cosine similarity should be valid"); // Auto-correlation is high for linear ramp
        });
    });
}

#[test]
fn test_simd_unaligned_load_euclidean_distance() {
    let len = 33; // Odd length to test remainder
    with_unaligned_f32_slice(len, |a| {
        with_unaligned_f32_slice(len, |b| {
            let result = euclidean_distance(a, b).unwrap();
            assert!(result >= 0.0);
        });
    });
}

// ============================================================================
// NaN/Inf Propagation Tests
// ============================================================================

#[test]
fn test_dot_product_nan_propagation_exact() {
    // 💣 Risk: SIMD reductions might mask NaNs if not careful (e.g. min/max ops)
    // Dot product sums should propagate NaN.
    let a = vec![1.0, 2.0, 3.0, f32::NAN, 5.0];
    let b = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    let result = dot_product(&a, &b).unwrap();
    assert!(result.is_nan(), "Dot product should propagate NaN");
}

#[test]
fn test_dot_product_inf_propagation_exact() {
    // Inf + anything = Inf (unless -Inf)
    let a = vec![1.0, 2.0, f32::INFINITY, 4.0];
    let b = vec![1.0, 2.0, 1.0, 4.0];
    let result = dot_product(&a, &b).unwrap();
    assert_eq!(
        result,
        f32::INFINITY,
        "Dot product should propagate Infinity"
    );
}

#[test]
fn test_cosine_similarity_subnormal_handling() {
    // Subnormal numbers (very small, close to zero)
    let val = f32::MIN_POSITIVE / 10.0; // Denormal
    let a = vec![val, val];
    let b = vec![val, val];

    // Should handle without underflow to zero causing div-by-zero if possible,
    // or return 0.0 if magnitude becomes 0.0.
    // Magnitude of a = sqrt(val^2 + val^2) = val * sqrt(2)
    // Dot = val*val + val*val = 2 * val^2
    // Cos = 2*val^2 / (val*sqrt(2) * val*sqrt(2)) = 2*val^2 / 2*val^2 = 1.0

    let result = cosine_similarity(&a, &b).unwrap();

    // If it underflows to zero, magnitude will be 0, result 0.0.
    // If it preserves precision, result 1.0.
    // We accept either, but mostly check for no panic.
    assert!(!result.is_nan());
}

// ============================================================================
// Small Vector & Edge Case Tests
// ============================================================================

#[test]
fn test_simd_vector_len_1() {
    let a = vec![2.0];
    let b = vec![3.0];
    let result = dot_product(&a, &b).unwrap();
    assert_eq!(result, 6.0);
}

#[test]
fn test_simd_vector_len_3() {
    // Less than SSE width (4)
    let a = vec![1.0, 2.0, 3.0];
    let b = vec![2.0, 3.0, 4.0];
    let result = dot_product(&a, &b).unwrap(); // 2 + 6 + 12 = 20
    assert_eq!(result, 20.0);
}

#[test]
fn test_simd_vector_len_7() {
    // Less than AVX width (8), more than SSE (4)
    let a = vec![1.0; 7];
    let b = vec![1.0; 7];
    let result = dot_product(&a, &b).unwrap();
    assert_eq!(result, 7.0);
}

#[test]
fn test_simd_vector_len_exact_chunk() {
    // Exactly 8 (AVX chunk)
    let a = vec![1.0; 8];
    let b = vec![1.0; 8];
    let result = dot_product(&a, &b).unwrap();
    assert_eq!(result, 8.0);
}

#[test]
fn test_simd_vector_len_exact_chunk_plus_one() {
    // 9 (AVX chunk + 1)
    let a = vec![1.0; 9];
    let b = vec![1.0; 9];
    let result = dot_product(&a, &b).unwrap();
    assert_eq!(result, 9.0);
}

// ============================================================================
// Zero Vector Tests
// ============================================================================

#[test]
fn test_cosine_similarity_zero_vector_lhs() {
    let a = vec![0.0, 0.0, 0.0];
    let b = vec![1.0, 2.0, 3.0];
    let result = cosine_similarity(&a, &b).unwrap();
    assert_eq!(result, 0.0);
}

#[test]
fn test_cosine_similarity_zero_vector_rhs() {
    let a = vec![1.0, 2.0, 3.0];
    let b = vec![0.0, 0.0, 0.0];
    let result = cosine_similarity(&a, &b).unwrap();
    assert_eq!(result, 0.0);
}

#[test]
fn test_cosine_similarity_both_zero() {
    let a = vec![0.0, 0.0];
    let b = vec![0.0, 0.0];
    let result = cosine_similarity(&a, &b).unwrap();
    assert_eq!(result, 0.0);
}

// ============================================================================
// SIMD/FFI Robustness Tests
// ============================================================================

#[test]
fn test_simd_dot_and_magnitudes_zero_length() {
    // 🧪 Strategy: Explicitly test the core SIMD primitive with empty vectors
    // to ensure safe FFI handling (no buffer over-reads).
    let a: Vec<f32> = vec![];
    let b: Vec<f32> = vec![];
    let (dot, mag_a, mag_b) = super::simd::dot_and_magnitudes(&a, &b);
    assert_eq!(dot, 0.0);
    assert_eq!(mag_a, 0.0);
    assert_eq!(mag_b, 0.0);
}

#[test]
fn test_simd_dot_and_magnitudes_nan() {
    // 💣 Risk: Verify that NaN values are propagated correctly and don't cause crashes.
    // If simsimd returns None (due to NaN), the fallback implementation must trigger
    // and also return NaN (or consistent result).
    let a = vec![1.0, f32::NAN, 3.0];
    let b = vec![1.0, 2.0, 3.0];
    let (dot, mag_a, mag_b) = super::simd::dot_and_magnitudes(&a, &b);

    // Dot product should be NaN because a[1] is NaN
    assert!(dot.is_nan());
    // Mag A should be NaN
    assert!(mag_a.is_nan());
    // Mag B should be valid (14.0)
    assert_eq!(mag_b, 14.0);
}

#[test]
fn test_simd_dot_and_magnitudes_inf() {
    // 💣 Risk: Verify Infinity handling.
    let a = vec![1.0, f32::INFINITY, 3.0];
    let b = vec![1.0, 2.0, 3.0];
    let (dot, mag_a, mag_b) = super::simd::dot_and_magnitudes(&a, &b);

    assert!(dot.is_infinite());
    assert!(mag_a.is_infinite());
    assert_eq!(mag_b, 14.0);
}

#[test]
fn test_simd_squared_diff_sum_zero_length() {
    // 🧪 Strategy: Test squared_diff_sum with empty vectors
    let a: Vec<f32> = vec![];
    let b: Vec<f32> = vec![];
    let res = super::simd::squared_diff_sum(&a, &b);
    assert_eq!(res, 0.0);
}

#[test]
fn test_simd_dot_product_sum_zero_length() {
    // 🧪 Strategy: Test dot_product_sum with empty vectors
    let a: Vec<f32> = vec![];
    let b: Vec<f32> = vec![];
    let res = super::simd::dot_product_sum(&a, &b);
    assert_eq!(res, 0.0);
}

#[test]
fn test_simd_dot_and_magnitudes_large_vector() {
    // 🧪 Strategy: Test with large vectors to exercise SIMD loop unrolling and remainder handling.
    let len = 1023; // Prime number large enough to have chunks and remainder
    let a: Vec<f32> = (0..len).map(|i| (i % 10) as f32).collect();
    let b: Vec<f32> = (0..len).map(|i| ((i + 1) % 10) as f32).collect();

    let (dot, mag_a, mag_b) = super::simd::dot_and_magnitudes(&a, &b);

    // Calculate expected scalar values
    let expected_dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let expected_mag_a: f32 = a.iter().map(|x| x * x).sum();
    let expected_mag_b: f32 = b.iter().map(|x| x * x).sum();

    // Allow small epsilon for floating point accumulation differences
    // 0.01 is stricter but should still pass if SIMD implementation is reasonably precise
    let epsilon = 0.01;
    assert!(
        (dot - expected_dot).abs() < epsilon,
        "Dot product mismatch: {} vs {}",
        dot,
        expected_dot
    );
    assert!(
        (mag_a - expected_mag_a).abs() < epsilon,
        "Mag A mismatch: {} vs {}",
        mag_a,
        expected_mag_a
    );
    assert!(
        (mag_b - expected_mag_b).abs() < epsilon,
        "Mag B mismatch: {} vs {}",
        mag_b,
        expected_mag_b
    );
}

#[test]
fn test_simd_dot_product_associativity() {
    // 🧪 Strategy: Check if SIMD (chunked sum) produces significantly different results
    // from Scalar (sequential sum) for a sensitive dataset.
    // Floating point addition is non-associative.

    let len = 1000;
    // Use values with alternating magnitudes to exacerbate rounding errors
    let a: Vec<f32> = (0..len)
        .map(|i| if i % 2 == 0 { 1.0e5 } else { 1.0 })
        .collect();
    let b: Vec<f32> = (0..len)
        .map(|i| if i % 2 == 0 { 1.0 } else { -1.0e5 })
        .collect();

    // Scalar sum: (1e5 * 1) + (1 * -1e5) + ... = 1e5 - 1e5 = 0
    // But sequential sum might drift.
    let scalar_dot = super::simd::dot_product_scalar(&a, &b);

    // SIMD sum: sums 8 lanes independently.
    // Lane 0: 1e5, 1e5, ... -> Sum(1e5)
    // Lane 1: -1e5, -1e5, ... -> Sum(-1e5)
    // Then horizontal sum.
    let simd_dot = super::simd::dot_product_sum(&a, &b);

    // We expect them to be close, but maybe not identical.
    // This test documents the behavior.
    let diff = (scalar_dot - simd_dot).abs();

    // They should be reasonably close for this balanced dataset
    assert!(
        diff < 1.0,
        "SIMD vs Scalar divergence: scalar {}, simd {}, diff {}",
        scalar_dot,
        simd_dot,
        diff
    );
}

// ============================================================================
// Scale In Place Tests (Added via Sentry consolidation)
// ============================================================================

#[test]
fn test_scale_in_place_basic() {
    let mut v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    scale_in_place(&mut v, 2.0);
    assert_eq!(v, vec![2.0, 4.0, 6.0, 8.0, 10.0]);
}

#[test]
fn test_scale_in_place_zero_length() {
    // Should not panic
    let mut v: Vec<f32> = vec![];
    scale_in_place(&mut v, 2.0);
    assert!(v.is_empty());
}

#[test]
fn test_scale_in_place_unaligned() {
    // 💣 Risk: SIMD operations using aligned instructions on unaligned memory cause SIGSEGV.
    // 🧪 Strategy: Force unaligned memory access using helper.
    with_unaligned_f32_slice_mut(100, |v| {
        // Capture original values for verification
        let original: Vec<f32> = v.to_vec();
        scale_in_place(&mut *v, 2.0);

        for (i, &val) in v.iter().enumerate() {
            assert!(
                (val - original[i] * 2.0).abs() < 1e-6,
                "Index {}: {} vs {}",
                i,
                val,
                original[i] * 2.0
            );
        }
    });
}

#[test]
fn test_scale_in_place_large_vector() {
    // 🧪 Strategy: Use prime length to test SIMD loop unrolling + remainder handling
    let len = 1023;
    let mut v: Vec<f32> = (0..len).map(|i| i as f32).collect();
    let original = v.clone();

    scale_in_place(&mut v, 0.5);

    for (i, &val) in v.iter().enumerate() {
        assert!((val - original[i] * 0.5).abs() < 1e-6);
    }
}

#[test]
fn test_scale_in_place_nan_scalar() {
    // 💣 Risk: NaN should propagate to all elements.
    let mut v = vec![1.0, 2.0, 3.0];
    scale_in_place(&mut v, f32::NAN);

    for val in v {
        assert!(val.is_nan());
    }
}

#[test]
fn test_scale_in_place_inf_scalar() {
    // 💣 Risk: Infinity should propagate.
    let mut v = vec![1.0, -2.0, 0.0];
    scale_in_place(&mut v, f32::INFINITY);

    assert_eq!(v[0], f32::INFINITY);
    assert_eq!(v[1], f32::NEG_INFINITY);
    assert!(v[2].is_nan()); // 0 * Inf = NaN
}

#[test]
fn test_scale_in_place_zero_scalar() {
    // 💣 Risk: Zero scalar should zero out the vector.
    // Note: Inf * 0 is NaN, so we test that too.
    let mut v = vec![1.0, 2.0, f32::INFINITY, f32::NAN];
    scale_in_place(&mut v, 0.0);

    assert_eq!(v[0], 0.0);
    assert_eq!(v[1], 0.0);
    assert!(v[2].is_nan()); // Inf * 0 = NaN
    assert!(v[3].is_nan()); // NaN * 0 = NaN
}

// ============================================================================
// Normalization Consistency Tests
// ============================================================================

#[test]
fn test_normalize_consistency_small_vector() {
    use super::constants::SQUARED_MAGNITUDE_THRESHOLD;

    // Construct a vector with squared magnitude slightly below threshold
    // threshold is 1e-14
    let small_val = (0.5 * SQUARED_MAGNITUDE_THRESHOLD).sqrt();
    let small_vec = vec![small_val];

    // normalize() returns zero vector
    let normalized = normalize(&small_vec);
    assert_eq!(normalized[0], 0.0, "normalize() should zero out small vectors");

    // normalize_in_place()
    let mut in_place_vec = small_vec.clone();
    normalize_in_place(&mut in_place_vec);

    // This assertion verifies the fix
    assert_eq!(in_place_vec[0], 0.0,
        "normalize_in_place() should zero out small vectors to match normalize(), but got {:?}", in_place_vec);

    assert_eq!(normalized, in_place_vec, "normalize and normalize_in_place should be consistent");
}
