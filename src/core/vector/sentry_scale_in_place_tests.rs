use super::simd::*;

// ============================================================================
// Helper Functions (Copied from sentry_tests.rs)
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
    F: FnOnce(&mut [f32]),
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

    // Create the mutable slice
    let slice_ptr = unsafe { buffer.as_mut_ptr().add(offset) as *mut f32 };
    let slice = unsafe { std::slice::from_raw_parts_mut(slice_ptr, len) };

    // Verify alignment
    let slice_addr = slice.as_ptr() as usize;
    assert_eq!(slice_addr % 4, 0, "Slice must be 4-byte aligned for f32");
    assert_ne!(
        slice_addr % 32,
        0,
        "Slice must NOT be 32-byte aligned for testing unaligned loads"
    );

    // Initialize with data (safe because we allocated enough bytes)
    for i in 0..len {
        slice[i] = (i as f32) * 1.0;
    }

    f(slice);
}

// ============================================================================
// Basic Tests
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
    // 🧪 Strategy: Force unaligned memory access.
    with_unaligned_f32_slice(100, |v| {
        // Capture original values for verification
        let original: Vec<f32> = v.to_vec();
        scale_in_place(v, 2.0);

        for (i, &val) in v.iter().enumerate() {
            assert!((val - original[i] * 2.0).abs() < 1e-6, "Index {}: {} vs {}", i, val, original[i] * 2.0);
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

// ============================================================================
// Special Value Tests (NaN, Inf)
// ============================================================================

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
// Explicit SIMD Path Coverage
// ============================================================================

#[test]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn test_scale_in_place_explicit_simd() {
    // 🧪 Strategy: Explicitly call internal SIMD functions (unsafe) to verify logic correctness
    // regardless of runtime feature detection.

    let original = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];

    // 1. Test SSE2 (Baseline for x86_64)
    if is_x86_feature_detected!("sse2") {
        let mut v = original.clone();
        unsafe {
            super::simd::x86_ops::scale_in_place_sse2(&mut v, 2.0);
        }
        assert_eq!(v, vec![2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 18.0]);
    }

    // 2. Test AVX2 (Conditional)
    if is_x86_feature_detected!("avx2") {
        let mut v = original.clone();
        unsafe {
            super::simd::x86_ops::scale_in_place_avx2(&mut v, 2.0);
        }
        assert_eq!(v, vec![2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 18.0]);
    }
}
