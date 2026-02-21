use super::ops::*;
use super::simd::*;
use std::mem::MaybeUninit;

#[test]
fn test_normalize_nan_handling() {
    // 🛡️ Sentry: Ensure normalize handles NaN vectors safely (returns NaN, no UB/crash)
    let v = vec![1.0, f32::NAN, 3.0];
    let normalized = normalize(&v);

    // Result should contain NaNs (NaN propagates)
    // We check that it has the correct length and safe values (f32 is always safe, but check logic)
    assert_eq!(normalized.len(), 3);
    assert!(normalized[1].is_nan());
}

#[test]
fn test_normalize_inf_handling() {
    // 🛡️ Sentry: Ensure normalize handles Inf safely
    let v = vec![1.0, f32::INFINITY, 3.0];
    let normalized = normalize(&v);

    // Magnitude is Inf. 1/Inf = 0.
    // So expected result is [0, NaN, 0] or [0, 0, 0] depending on Inf * 0 implementation.
    // Inf * 0 is NaN.

    assert_eq!(normalized.len(), 3);
    // 1.0 * 0 = 0
    assert_eq!(normalized[0], 0.0);
    // Inf * 0 = NaN
    assert!(normalized[1].is_nan());
    // 3.0 * 0 = 0
    assert_eq!(normalized[2], 0.0);
}

#[test]
fn test_normalize_zero_handling() {
    // 🛡️ Sentry: Ensure normalize handles Zero vector (returns Zero, no div-by-zero)
    let v = vec![0.0, 0.0, 0.0];
    let normalized = normalize(&v);

    assert_eq!(normalized, vec![0.0, 0.0, 0.0]);
}

#[test]
fn test_scale_and_copy_correctness() {
    // 🛡️ Sentry: Verify scale_and_copy writes to ALL elements correctly.
    // This is critical because normalize() relies on this to initialize the vector.

    let src = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    // Create uninitialized destination
    let mut dst_vec: Vec<MaybeUninit<f32>> = Vec::with_capacity(5);
    unsafe { dst_vec.set_len(5) };

    scale_and_copy(&src, &mut dst_vec, 2.0);

    // Verify result without transmute
    // SAFETY: We initialized dst_vec with valid floats via scale_and_copy
    let dst_slice =
        unsafe { std::slice::from_raw_parts(dst_vec.as_ptr() as *const f32, dst_vec.len()) };

    assert_eq!(dst_slice, &[2.0, 4.0, 6.0, 8.0, 10.0]);
}

#[test]
fn test_scale_and_copy_large_vector() {
    // 🛡️ Sentry: Test with larger vector to trigger SIMD loop + remainder
    let len = 1024 + 7; // 1031 elements
    let src: Vec<f32> = (0..len).map(|i| i as f32).collect();

    let mut dst_vec: Vec<MaybeUninit<f32>> = Vec::with_capacity(len);
    unsafe { dst_vec.set_len(len) };

    scale_and_copy(&src, &mut dst_vec, 2.0);

    // Verify result without transmute
    let dst_slice =
        unsafe { std::slice::from_raw_parts(dst_vec.as_ptr() as *const f32, dst_vec.len()) };

    for (i, val) in dst_slice.iter().enumerate() {
        assert_eq!(*val, (i as f32) * 2.0);
    }
}
