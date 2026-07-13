//! # Context
//! This module contains "Sentry" tests focused on the memory safety and
//! correctness of the vector-math operations (now `unsafe`-free after Issue
//! #426), particularly around zero/NaN/Inf handling in normalization.
//!
//! # Usage
//! These tests verify that core vector operations safely handle invalid floats (`NaN`, `Infinity`)
//! without crashing or invoking undefined behavior.
//!
//! # Details
//! Specifically, it ensures that:
//! - `normalize()` properly returns `NaN` if inputs are `NaN` or `Infinity`.
//! - `normalize()` handles Zero vectors correctly.
//! - `normalize()` writes every element correctly across a range of lengths.
//!
//! ## Panics
//! These tests assert that invalid values map gracefully to predictable floating-point representations
//! rather than panicking or faulting.
//!
//! ## Examples
//! ```rust,ignore
//! let v = vec![1.0, f32::NAN, 3.0];
//! let normalized = normalize(&v);
//! assert!(normalized[1].is_nan());
//! ```

use super::ops::*;

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
fn test_normalize_writes_all_elements() {
    // 🛡️ Sentry: Verify normalize() produces a fully-initialized, correctly
    // scaled vector (it no longer relies on any uninitialized-memory path).
    // src = [3, 4] has magnitude 5, so normalize -> [0.6, 0.8].
    let normalized = normalize(&[3.0, 4.0]);
    assert!((normalized[0] - 0.6).abs() < 1e-6);
    assert!((normalized[1] - 0.8).abs() < 1e-6);
}

#[test]
fn test_normalize_large_vector_all_elements() {
    // 🛡️ Sentry: Test with a larger vector to exercise the full length.
    let len = 1024 + 7; // 1031 elements, a non-power-of-two length
    let src: Vec<f32> = (0..len).map(|i| (i + 1) as f32).collect();

    let normalized = normalize(&src);
    // Every element scaled by the same positive factor => unit length overall.
    let mag_sq: f64 = normalized.iter().map(|&x| (x as f64) * (x as f64)).sum();
    assert!((mag_sq.sqrt() - 1.0).abs() < 1e-4);
    assert_eq!(normalized.len(), len);
    // Direction preserved: strictly increasing input stays strictly increasing.
    assert!(normalized[0] < normalized[len - 1]);
}
