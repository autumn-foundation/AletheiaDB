//! Low-level vector-math kernels (dot product, squared magnitude, squared
//! difference, element-wise scaling).
//!
//! # Acceleration strategy (Issue #426)
//!
//! When the crate is built with the `simsimd` feature (on by default) the
//! reduction kernels dispatch to the [`simsimd`] crate, which selects the best
//! available SIMD backend **at runtime** — x86 SSE/AVX2/AVX-512 *and* ARM
//! NEON/SVE. This replaced the crate's previous hand-written, x86-only
//! `unsafe` AVX2/SSE2 intrinsics: there is now **zero `unsafe` code in this
//! module**, and ARM targets get first-class SIMD acceleration instead of a
//! scalar fallback.
//!
//! When the feature is disabled (`--no-default-features`) every kernel uses the
//! portable scalar implementation below, which modern compilers auto-vectorize.
//! The scalar functions are also used as a *fallback within* the simsimd path:
//! simsimd returns `Option<f64>` and yields `None` for e.g. length mismatches,
//! and to preserve the exact NaN/Inf-propagation contract the public API
//! guarantees we recompute with the scalar kernel on `None` rather than
//! substituting a sentinel value.
//!
//! The element-wise `scale_in_place` map has no simsimd reduction equivalent
//! and is a plain scalar loop in both configurations.

// ============================================================================
// Scalar kernels
// ============================================================================
//
// These are the reference implementations. They are the primary kernels when
// the `simsimd` feature is off, and the `None`-fallback when it is on, so they
// are never dead code.

/// Scalar computation of dot product and both squared magnitudes in one pass.
#[inline]
pub(crate) fn dot_and_magnitudes_scalar(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    a.iter().zip(b.iter()).fold(
        (0.0f32, 0.0f32, 0.0f32),
        |(dot, mag_a, mag_b), (&ai, &bi)| (dot + ai * bi, mag_a + ai * ai, mag_b + bi * bi),
    )
}

/// Scalar computation of the sum of squared differences.
#[inline]
pub(crate) fn squared_diff_sum_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(&ai, &bi)| {
            let diff = ai - bi;
            diff * diff
        })
        .sum()
}

/// Scalar computation of the dot product.
#[inline]
pub(crate) fn dot_product_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(&ai, &bi)| ai * bi).sum()
}

/// Scalar computation of the squared magnitude.
#[inline]
pub(crate) fn squared_magnitude_scalar(v: &[f32]) -> f32 {
    v.iter().map(|&x| x * x).sum()
}

/// Scales a vector in place by `scalar` (element-wise map; auto-vectorized).
#[inline]
pub(crate) fn scale_in_place_scalar(v: &mut [f32], scalar: f32) {
    for x in v.iter_mut() {
        *x *= scalar;
    }
}

// ============================================================================
// simsimd helpers
// ============================================================================

/// simsimd-backed dot product with a scalar fallback on `None`.
///
/// simsimd computes in wide precision and returns `Option<f64>`; `None` only
/// arises for inputs simsimd rejects (e.g. a length mismatch). Falling back to
/// the scalar kernel — rather than a sentinel like `0.0` — preserves the exact
/// NaN/Inf-propagation contract the public API asserts.
#[cfg(feature = "simsimd")]
#[inline]
fn simsimd_dot(a: &[f32], b: &[f32]) -> f32 {
    use simsimd::SpatialSimilarity;
    match f32::dot(a, b) {
        Some(v) => v as f32,
        None => dot_product_scalar(a, b),
    }
}

/// simsimd-backed squared magnitude (`dot(v, v)`) with a scalar fallback.
#[cfg(feature = "simsimd")]
#[inline]
fn simsimd_squared_magnitude(v: &[f32]) -> f32 {
    use simsimd::SpatialSimilarity;
    match f32::dot(v, v) {
        Some(x) => x as f32,
        None => squared_magnitude_scalar(v),
    }
}

// ============================================================================
// Public(crate) kernel dispatch
// ============================================================================

/// Computes the dot product and both squared magnitudes.
///
/// With `simsimd` this is three SIMD-accelerated reductions (`a·b`, `a·a`,
/// `b·b`); on any `None` it recomputes the whole tuple with the scalar kernel
/// to keep NaN/Inf behavior identical. Without the feature it is a single
/// scalar pass.
#[inline]
pub(crate) fn dot_and_magnitudes(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    assert_eq!(a.len(), b.len());
    #[cfg(feature = "simsimd")]
    {
        use simsimd::SpatialSimilarity;
        match (f32::dot(a, b), f32::dot(a, a), f32::dot(b, b)) {
            (Some(dot), Some(mag_a), Some(mag_b)) => (dot as f32, mag_a as f32, mag_b as f32),
            // Any rejection → recompute the whole tuple with the scalar kernel
            // so NaN / Inf propagate exactly as the public contract requires.
            _ => dot_and_magnitudes_scalar(a, b),
        }
    }
    #[cfg(not(feature = "simsimd"))]
    {
        dot_and_magnitudes_scalar(a, b)
    }
}

/// Computes the sum of squared differences (squared Euclidean distance).
#[inline]
pub(crate) fn squared_diff_sum(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    #[cfg(feature = "simsimd")]
    {
        use simsimd::SpatialSimilarity;
        match f32::l2sq(a, b) {
            Some(v) => v as f32,
            None => squared_diff_sum_scalar(a, b),
        }
    }
    #[cfg(not(feature = "simsimd"))]
    {
        squared_diff_sum_scalar(a, b)
    }
}

/// Computes the dot product.
#[inline]
pub(crate) fn dot_product_sum(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    #[cfg(feature = "simsimd")]
    {
        simsimd_dot(a, b)
    }
    #[cfg(not(feature = "simsimd"))]
    {
        dot_product_scalar(a, b)
    }
}

/// Computes the squared magnitude of a vector.
#[inline]
pub(crate) fn squared_magnitude(v: &[f32]) -> f32 {
    #[cfg(feature = "simsimd")]
    {
        simsimd_squared_magnitude(v)
    }
    #[cfg(not(feature = "simsimd"))]
    {
        squared_magnitude_scalar(v)
    }
}

/// Scales a vector in place by `scalar`.
///
/// Element-wise map with no simsimd reduction equivalent; a plain scalar loop
/// the compiler auto-vectorizes, identical in both feature configurations.
#[inline]
pub(crate) fn scale_in_place(v: &mut [f32], scalar: f32) {
    scale_in_place_scalar(v, scalar);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scale_in_place_matches_scalar_reference() {
        let mut v = vec![1.0f32; 17];
        scale_in_place(&mut v, 2.0);
        assert_eq!(v, vec![2.0f32; 17]);
    }

    #[test]
    fn test_squared_magnitude_dispatch() {
        let v = vec![1.0f32, 2.0, 3.0, 4.0, 5.0]; // 1+4+9+16+25 = 55
        assert_eq!(squared_magnitude(&v), 55.0);
        assert_eq!(squared_magnitude_scalar(&v), 55.0);
    }
}
