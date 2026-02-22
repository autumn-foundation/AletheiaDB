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
    let mut dst = vec![0.0; 5]; // Pre-fill with 0 to verify overwrite

    // Helper to treat initialized slice as uninitialized for testing
    unsafe fn as_uninit_mut(s: &mut [f32]) -> &mut [std::mem::MaybeUninit<f32>] {
        // SAFETY: s is a valid mutable slice. MaybeUninit<f32> has same layout as f32.
        unsafe {
            std::slice::from_raw_parts_mut(
                s.as_mut_ptr() as *mut std::mem::MaybeUninit<f32>,
                s.len(),
            )
        }
    }

    scale_and_copy(&src, unsafe { as_uninit_mut(&mut dst) }, 2.0);

    assert_eq!(dst, vec![2.0, 4.0, 6.0, 8.0, 10.0]);
}

#[test]
fn test_scale_and_copy_large_vector() {
    // 🛡️ Sentry: Test with larger vector to trigger SIMD loop + remainder
    let len = 1024 + 7; // 1031 elements
    let src: Vec<f32> = (0..len).map(|i| i as f32).collect();
    let mut dst = vec![0.0; len];

    // Helper to treat initialized slice as uninitialized for testing
    unsafe fn as_uninit_mut(s: &mut [f32]) -> &mut [std::mem::MaybeUninit<f32>] {
        // SAFETY: s is a valid mutable slice. MaybeUninit<f32> has same layout as f32.
        unsafe {
            std::slice::from_raw_parts_mut(
                s.as_mut_ptr() as *mut std::mem::MaybeUninit<f32>,
                s.len(),
            )
        }
    }

    scale_and_copy(&src, unsafe { as_uninit_mut(&mut dst) }, 2.0);

    for (i, val) in dst.iter().enumerate() {
        assert_eq!(*val, (i as f32) * 2.0);
    }
}
