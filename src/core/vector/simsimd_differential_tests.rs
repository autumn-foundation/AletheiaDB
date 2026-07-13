//! Differential and convention tests for the simsimd-backed vector kernels
//! (Issue #426).
//!
//! These tests pin down two things:
//!
//! 1. **Conventions** — the exact numeric contract of the public similarity /
//!    distance API (cosine of identical / orthogonal / opposite vectors, known
//!    euclidean / dot values, and the empty / zero-vector edge cases). These
//!    values must be identical whether the crate is built with the `simsimd`
//!    feature (SIMD kernels, x86 AVX / ARM NEON via runtime dispatch) or with
//!    `--no-default-features` (scalar fallback).
//!
//! 2. **Differential agreement** — for many random vectors across a range of
//!    dimensions (including the common embedding sizes 384/768/1024/1536/3072
//!    and awkward tail lengths 1/3/7/17) the value produced by whichever kernel
//!    path is compiled agrees with an independent high-precision (`f64`) scalar
//!    reference within a tight relative tolerance. Because the public op routes
//!    through simsimd when the feature is on and through the scalar fallback
//!    when it is off, running this file under both cargo feature configurations
//!    exercises *both* code paths against the same reference.

use super::ops::{
    cosine_similarity, cosine_similarity_normalized, dot_product, euclidean_distance, magnitude,
    normalize, squared_euclidean_distance, squared_magnitude,
};

/// Deterministic pseudo-random generator (SplitMix64) so the differential
/// sweep is reproducible without pulling a dev-dependency into this module.
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform f32 in [-1.0, 1.0).
    fn next_f32(&mut self) -> f32 {
        // 24 bits of mantissa precision, mapped to [-1, 1).
        let bits = (self.next_u64() >> 40) as u32; // 24 bits
        let unit = (bits as f32) / (1u32 << 24) as f32; // [0, 1)
        unit * 2.0 - 1.0
    }
}

fn random_vec(rng: &mut SplitMix64, dim: usize) -> Vec<f32> {
    (0..dim).map(|_| rng.next_f32()).collect()
}

// ---------------------------------------------------------------------------
// High-precision (f64) scalar references — ground truth for the differential
// sweep. These are intentionally independent of the crate's own kernels.
// ---------------------------------------------------------------------------

fn ref_dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(&x, &y)| x as f64 * y as f64).sum()
}

fn ref_sq_mag(a: &[f32]) -> f64 {
    a.iter().map(|&x| (x as f64) * (x as f64)).sum()
}

fn ref_cosine(a: &[f32], b: &[f32]) -> f64 {
    let d = ref_dot(a, b);
    let ma = ref_sq_mag(a).sqrt();
    let mb = ref_sq_mag(b).sqrt();
    if ma == 0.0 || mb == 0.0 {
        0.0
    } else {
        (d / (ma * mb)).clamp(-1.0, 1.0)
    }
}

fn ref_sq_euclidean(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| {
            let diff = x as f64 - y as f64;
            diff * diff
        })
        .sum()
}

/// Assert `got` is within `rel_tol` *relative* tolerance of `want`
/// (with an absolute floor for values near zero).
fn assert_close(got: f32, want: f64, rel_tol: f64, ctx: &str) {
    let got = got as f64;
    let diff = (got - want).abs();
    let scale = want.abs().max(1.0);
    assert!(
        diff <= rel_tol * scale,
        "{ctx}: got={got} want={want} diff={diff} rel_tol={rel_tol}"
    );
}

// ---------------------------------------------------------------------------
// 1. Convention tests (exact-ish, identical across both kernel paths)
// ---------------------------------------------------------------------------

#[test]
fn convention_cosine_identical_is_one() {
    let a = vec![1.0, 2.0, 3.0, 4.0];
    assert!((cosine_similarity(&a, &a).unwrap() - 1.0).abs() < 1e-6);
}

#[test]
fn convention_cosine_orthogonal_is_zero() {
    let a = vec![1.0, 0.0, 0.0];
    let b = vec![0.0, 1.0, 0.0];
    assert!(cosine_similarity(&a, &b).unwrap().abs() < 1e-6);
}

#[test]
fn convention_cosine_opposite_is_minus_one() {
    let a = vec![1.0, 2.0, 3.0];
    let b = vec![-1.0, -2.0, -3.0];
    assert!((cosine_similarity(&a, &b).unwrap() + 1.0).abs() < 1e-6);
}

#[test]
fn convention_euclidean_known() {
    // 3-4-5 right triangle.
    let a = vec![0.0, 0.0];
    let b = vec![3.0, 4.0];
    assert!((euclidean_distance(&a, &b).unwrap() - 5.0).abs() < 1e-6);
    assert!((squared_euclidean_distance(&a, &b).unwrap() - 25.0).abs() < 1e-6);
}

#[test]
fn convention_dot_known() {
    let a = vec![1.0, 2.0, 3.0];
    let b = vec![4.0, 5.0, 6.0];
    // 1*4 + 2*5 + 3*6 = 32
    assert!((dot_product(&a, &b).unwrap() - 32.0).abs() < 1e-6);
}

#[test]
fn convention_magnitude_known() {
    let v = vec![3.0, 4.0];
    assert!((magnitude(&v) - 5.0).abs() < 1e-6);
    assert!((squared_magnitude(&v) - 25.0).abs() < 1e-6);
}

#[test]
fn convention_empty_vectors() {
    let a: Vec<f32> = vec![];
    let b: Vec<f32> = vec![];
    assert_eq!(cosine_similarity(&a, &b).unwrap(), 0.0);
    assert_eq!(cosine_similarity_normalized(&a, &b).unwrap(), 0.0);
    assert_eq!(squared_euclidean_distance(&a, &b).unwrap(), 0.0);
    assert_eq!(euclidean_distance(&a, &b).unwrap(), 0.0);
    assert_eq!(dot_product(&a, &b).unwrap(), 0.0);
    assert_eq!(magnitude(&a), 0.0);
    assert_eq!(squared_magnitude(&a), 0.0);
}

#[test]
fn convention_zero_vector_cosine_is_zero() {
    let a = vec![0.0, 0.0, 0.0];
    let b = vec![1.0, 2.0, 3.0];
    assert_eq!(cosine_similarity(&a, &b).unwrap(), 0.0);
    let z = vec![0.0, 0.0, 0.0];
    assert_eq!(cosine_similarity(&a, &z).unwrap(), 0.0);
}

#[test]
fn convention_nan_propagates_in_cosine() {
    // The public contract (and existing tests) require NaN in an input to
    // surface as NaN, never a silently-swallowed 0.0. This is the reason the
    // simsimd path must fall back to the scalar kernel on `None`.
    let a = vec![f32::NAN, 1.0];
    let b = vec![1.0, 1.0];
    assert!(cosine_similarity(&a, &b).unwrap().is_nan());
}

// ---------------------------------------------------------------------------
// 2. Differential sweep: compiled kernel path vs f64 scalar reference.
// ---------------------------------------------------------------------------

const SWEEP_DIMS: &[usize] = &[1, 3, 7, 17, 33, 128, 384, 768, 1024, 1536, 3072];

#[test]
fn differential_dot_product() {
    let mut rng = SplitMix64::new(0xD07);
    for &dim in SWEEP_DIMS {
        for _ in 0..16 {
            let a = random_vec(&mut rng, dim);
            let b = random_vec(&mut rng, dim);
            let got = dot_product(&a, &b).unwrap();
            let want = ref_dot(&a, &b);
            assert_close(got, want, 1e-4, &format!("dot dim={dim}"));
        }
    }
}

#[test]
fn differential_squared_euclidean() {
    let mut rng = SplitMix64::new(0x5EE);
    for &dim in SWEEP_DIMS {
        for _ in 0..16 {
            let a = random_vec(&mut rng, dim);
            let b = random_vec(&mut rng, dim);
            let got = squared_euclidean_distance(&a, &b).unwrap();
            let want = ref_sq_euclidean(&a, &b);
            assert_close(got, want, 1e-4, &format!("sq_euclidean dim={dim}"));
        }
    }
}

#[test]
fn differential_euclidean() {
    let mut rng = SplitMix64::new(0xE0C);
    for &dim in SWEEP_DIMS {
        for _ in 0..16 {
            let a = random_vec(&mut rng, dim);
            let b = random_vec(&mut rng, dim);
            let got = euclidean_distance(&a, &b).unwrap();
            let want = ref_sq_euclidean(&a, &b).sqrt();
            assert_close(got, want, 1e-4, &format!("euclidean dim={dim}"));
        }
    }
}

#[test]
fn differential_squared_magnitude() {
    let mut rng = SplitMix64::new(0x111);
    for &dim in SWEEP_DIMS {
        for _ in 0..16 {
            let a = random_vec(&mut rng, dim);
            let got = squared_magnitude(&a);
            let want = ref_sq_mag(&a);
            assert_close(got, want, 1e-4, &format!("sq_magnitude dim={dim}"));
        }
    }
}

#[test]
fn differential_cosine_similarity() {
    let mut rng = SplitMix64::new(0xC05);
    for &dim in SWEEP_DIMS {
        for _ in 0..16 {
            let a = random_vec(&mut rng, dim);
            let b = random_vec(&mut rng, dim);
            let got = cosine_similarity(&a, &b).unwrap();
            let want = ref_cosine(&a, &b);
            // Cosine is a ratio in [-1, 1]; an absolute tolerance is the right
            // model here (relative tolerance explodes near zero).
            assert!(
                (got as f64 - want).abs() <= 1e-4,
                "cosine dim={dim}: got={got} want={want}"
            );
        }
    }
}

#[test]
fn differential_cosine_normalized_matches_general() {
    let mut rng = SplitMix64::new(0x2222);
    for &dim in SWEEP_DIMS {
        for _ in 0..16 {
            let a = random_vec(&mut rng, dim);
            let b = random_vec(&mut rng, dim);
            let na = normalize(&a);
            let nb = normalize(&b);
            // Both should approximate the true cosine of the original vectors.
            let want = ref_cosine(&a, &b);
            let got = cosine_similarity_normalized(&na, &nb).unwrap();
            assert!(
                (got as f64 - want).abs() <= 1e-3,
                "cosine_normalized dim={dim}: got={got} want={want}"
            );
        }
    }
}

#[test]
fn differential_normalize_is_unit_length() {
    let mut rng = SplitMix64::new(0x3333);
    for &dim in SWEEP_DIMS {
        for _ in 0..8 {
            let a = random_vec(&mut rng, dim);
            let n = normalize(&a);
            let mag = ref_sq_mag(&n).sqrt();
            // A non-zero random vector normalizes to unit length; a vanishingly
            // small one collapses to the zero vector (mag 0). Both are valid.
            assert!(
                (mag - 1.0).abs() < 1e-4 || mag < 1e-6,
                "normalize dim={dim}: magnitude={mag}"
            );
        }
    }
}

/// Documents *why* the public `cosine_similarity` decomposes into simsimd
/// `dot` reductions (a·b, a·a, b·b) plus the crate's own guard/clamp logic,
/// instead of calling simsimd's fused `cosine` primitive.
///
/// simsimd's fused `f32::cosine` silently *swallows* the very edge cases the
/// public API's contract (and its tests) pin down: it returns cosine distance
/// `0.0` (i.e. similarity `1.0`) for a NaN input, for opposing infinities, and
/// even for two zero vectors — where the contract requires NaN, NaN, and `0.0`
/// respectively. Using it would regress correctness for those inputs, so it is
/// deliberately *not* used. This test captures that behavior so the decision is
/// self-evidencing and any future simsimd change is caught.
#[cfg(feature = "simsimd")]
#[test]
fn simsimd_fused_cosine_would_violate_contract() {
    use simsimd::SpatialSimilarity;

    // NaN input: fused cosine yields distance 0.0 (sim 1.0), NOT NaN.
    let d = f32::cosine(&[f32::NAN, 1.0], &[1.0, 1.0]).unwrap();
    assert_eq!(
        d, 0.0,
        "simsimd swallows NaN (returns 0.0 distance); our cosine must not"
    );
    // Opposing infinities: fused cosine yields 0.0 (sim 1.0), NOT NaN.
    let d = f32::cosine(&[f32::INFINITY, 0.0], &[f32::INFINITY, 0.0]).unwrap();
    assert_eq!(
        d, 0.0,
        "simsimd swallows Inf/Inf; our cosine must yield NaN"
    );
    // Two zero vectors: fused cosine yields 0.0 (sim 1.0), but the contract
    // requires similarity 0.0 for zero vectors.
    let d = f32::cosine(&[0.0, 0.0], &[0.0, 0.0]).unwrap();
    assert_eq!(
        d, 0.0,
        "simsimd returns sim 1.0 for both-zero; contract is 0.0"
    );

    // Meanwhile the crate's public cosine_similarity honors the contract on the
    // exact same inputs (this is what the decomposition buys us):
    assert!(
        cosine_similarity(&[f32::NAN, 1.0], &[1.0, 1.0])
            .unwrap()
            .is_nan()
    );
    assert!(
        cosine_similarity(&[f32::INFINITY, 0.0], &[f32::INFINITY, 0.0])
            .unwrap()
            .is_nan()
    );
    assert_eq!(cosine_similarity(&[0.0, 0.0], &[0.0, 0.0]).unwrap(), 0.0);
}
