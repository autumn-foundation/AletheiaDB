use super::ops::*;
use super::simd::*;

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
    // Use proper MaybeUninit pattern via Vec capacity
    let mut dst = Vec::with_capacity(5);

    // Get uninitialized slice
    let dst_uninit = dst.spare_capacity_mut();
    // Slice to exact length (though capacity should be 5, defensive coding)
    let dst_slice = &mut dst_uninit[..5];

    scale_and_copy(&src, dst_slice, 2.0);

    // Safety: scale_and_copy initializes the memory
    unsafe { dst.set_len(5) };

    assert_eq!(dst, vec![2.0, 4.0, 6.0, 8.0, 10.0]);
}

#[test]
fn test_scale_and_copy_large_vector() {
    // 🛡️ Sentry: Test with larger vector to trigger SIMD loop + remainder
    let len = 1024 + 7; // 1031 elements
    let src: Vec<f32> = (0..len).map(|i| i as f32).collect();

    let mut dst = Vec::with_capacity(len);
    let dst_uninit = dst.spare_capacity_mut();
    let dst_slice = &mut dst_uninit[..len];

    scale_and_copy(&src, dst_slice, 2.0);

    unsafe { dst.set_len(len) };

    for (i, val) in dst.iter().enumerate() {
        assert_eq!(*val, (i as f32) * 2.0);
    }
}

#[test]
fn test_normalize_initialization_safety() {
    // 🛡️ Sentry: Explicitly verify that normalize returns a fully initialized vector
    // This targets the new implementation using spare_capacity_mut
    let v = vec![1.0, 2.0, 3.0];
    let normalized = normalize(&v);

    // Check values are correct (and thus initialized)
    let mag = (1.0*1.0 + 2.0*2.0 + 3.0*3.0f32).sqrt();
    assert!((normalized[0] - 1.0/mag).abs() < 1e-6);
    assert!((normalized[1] - 2.0/mag).abs() < 1e-6);
    assert!((normalized[2] - 3.0/mag).abs() < 1e-6);
}
