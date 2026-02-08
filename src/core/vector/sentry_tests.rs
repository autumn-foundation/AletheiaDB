use super::ops::*;

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
