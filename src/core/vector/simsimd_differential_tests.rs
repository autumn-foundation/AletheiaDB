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

/// `euclidean_distance(a, a)` must be exactly `0.0` — never a tiny negative
/// squared sum that `sqrt`s to `NaN`. This guards against an l2sq kernel whose
/// FMA/accumulation ordering could let cancellation drive the sum below zero,
/// including for large-magnitude and near-equal (`a ≈ b`) inputs where the
/// cancellation risk is highest. Runs under both kernel configurations.
#[test]
fn euclidean_self_distance_is_zero_no_nan() {
    let cases: &[Vec<f32>] = &[
        vec![0.0, 0.0, 0.0],
        vec![1.0, 2.0, 3.0, 4.0],
        vec![1e18, -1e18, 1e18],    // large magnitude
        vec![1e-20, -1e-20, 1e-20], // tiny magnitude
        vec![123456.0; 384],        // large, common embedding dim
        (0..768).map(|i| i as f32).collect(),
    ];
    for a in cases {
        let d = euclidean_distance(a, a).unwrap();
        assert_eq!(d, 0.0, "euclidean_distance(a, a) must be 0.0, got {d}");
        assert!(!d.is_nan(), "euclidean_distance(a, a) must not be NaN");
        // squared form must not go negative (which would NaN under sqrt).
        let sq = squared_euclidean_distance(a, a).unwrap();
        assert!(
            sq >= 0.0,
            "squared_euclidean_distance(a, a) must be >= 0, got {sq}"
        );
    }
}

/// Near-equal `a ≈ b` pairs: distance is small and non-negative, and `sqrt`
/// never produces `NaN` from a negative squared sum.
#[test]
fn euclidean_near_equal_is_nonnegative_no_nan() {
    let mut rng = SplitMix64::new(0xAE9);
    for &dim in &[3usize, 17, 384, 1536] {
        let a = random_vec(&mut rng, dim);
        // b = a + tiny perturbation
        let b: Vec<f32> = a.iter().map(|&x| x + 1e-7).collect();
        let sq = squared_euclidean_distance(&a, &b).unwrap();
        assert!(sq >= 0.0, "sq_euclidean(a, a≈b) must be >= 0, got {sq}");
        let d = euclidean_distance(&a, &b).unwrap();
        assert!(
            !d.is_nan() && d >= 0.0,
            "euclidean(a, a≈b) must be finite >= 0, got {d}"
        );
    }
}

/// NaN in an input propagates through `dot_product` (previously only cosine had
/// a NaN test). Runs under both kernel configurations.
#[test]
fn dot_product_propagates_nan() {
    let a = vec![f32::NAN, 1.0, 2.0];
    let b = vec![1.0, 1.0, 1.0];
    assert!(dot_product(&a, &b).unwrap().is_nan());
    assert!(dot_product(&b, &a).unwrap().is_nan());
}

/// Inf in an input propagates through `dot_product`.
#[test]
fn dot_product_propagates_inf() {
    let a = vec![f32::INFINITY, 1.0, 2.0];
    let b = vec![1.0, 1.0, 1.0];
    assert!(dot_product(&a, &b).unwrap().is_infinite());
}

/// NaN in an input propagates through `squared_euclidean_distance`.
#[test]
fn squared_euclidean_propagates_nan() {
    let a = vec![f32::NAN, 1.0, 2.0];
    let b = vec![1.0, 1.0, 1.0];
    assert!(squared_euclidean_distance(&a, &b).unwrap().is_nan());
    assert!(squared_euclidean_distance(&b, &a).unwrap().is_nan());
}

/// Inf in an input propagates through `squared_euclidean_distance`.
#[test]
fn squared_euclidean_propagates_inf() {
    let a = vec![f32::INFINITY, 0.0, 0.0];
    let b = vec![0.0, 0.0, 0.0];
    assert!(squared_euclidean_distance(&a, &b).unwrap().is_infinite());
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

    // What we assert about simsimd's *internal* fused cosine is intentionally
    // loose, so a future simsimd bump does not turn into a false CI failure.
    //
    // Version pin (observed as of simsimd 6.5.x): `f32::cosine` returns
    // `Some(0.0)` cosine *distance* (i.e. similarity 1.0) for a NaN input, for
    // opposing infinities, and for two zero vectors — silently swallowing the
    // very edge cases our public contract pins to NaN, NaN, and 0.0. Any of the
    // following outcomes still demonstrates that the fused primitive cannot be
    // trusted to honor our contract, so we accept all three without panicking:
    //   * `Some(0.0)`  — swallowed to "identical" (the currently-observed bug),
    //   * `Some(NaN)`  — a future version that surfaces NaN but which we still
    //                     would not rely on for the zero-vector case, or
    //   * `None`       — a future version that rejects the input outright.
    // The load-bearing guarantee lives in the `cosine_similarity` assertions
    // below: those must hold regardless of what the fused primitive does.
    fn fused_cannot_be_trusted(d: Option<f64>) -> bool {
        match d {
            None => true,
            Some(v) => v == 0.0 || v.is_nan(),
        }
    }

    // NaN input: contract requires NaN; fused cosine does not deliver it.
    assert!(
        fused_cannot_be_trusted(f32::cosine(&[f32::NAN, 1.0], &[1.0, 1.0])),
        "fused cosine on NaN input should be untrustworthy (0.0/NaN/None); \
         our cosine must yield NaN"
    );
    // Opposing infinities: contract requires NaN; fused cosine does not.
    assert!(
        fused_cannot_be_trusted(f32::cosine(&[f32::INFINITY, 0.0], &[f32::INFINITY, 0.0])),
        "fused cosine on Inf/Inf should be untrustworthy (0.0/NaN/None); \
         our cosine must yield NaN"
    );
    // Two zero vectors: contract requires similarity 0.0; fused cosine returns
    // 0.0 *distance* (similarity 1.0), i.e. the wrong answer.
    assert!(
        fused_cannot_be_trusted(f32::cosine(&[0.0, 0.0], &[0.0, 0.0])),
        "fused cosine on both-zero should be untrustworthy (0.0/NaN/None); \
         contract is similarity 0.0"
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

// ---------------------------------------------------------------------------
// 3. Cross-path guard (simsimd feature only).
//
// The differential sweep above routes the *public* op through whichever kernel
// is compiled: simsimd when the feature is on, scalar when it is off. But the
// scalar kernels remain compiled even under the `simsimd` feature (they are the
// `None`-fallback the simsimd helpers call), yet the default CI test job — which
// always has `simsimd` on — never *executes* them, so a numeric regression in a
// scalar kernel would slip through CI silently. This test closes that gap: it
// invokes the simsimd dispatch AND the scalar kernel *directly, in the same
// binary*, and asserts they agree — so `cargo test` (default features) now
// guards the scalar numerics too, not just the simsimd path.
// ---------------------------------------------------------------------------

/// Relative agreement check between two same-precision (`f32`) kernel results,
/// with an absolute floor of 1.0 for values near zero.
#[cfg(feature = "simsimd")]
fn assert_agree(simsimd: f32, scalar: f32, rel_tol: f32, ctx: &str) {
    let diff = (simsimd - scalar).abs();
    let scale = simsimd.abs().max(scalar.abs()).max(1.0);
    assert!(
        diff <= rel_tol * scale,
        "{ctx}: simsimd={simsimd} scalar={scalar} diff={diff} rel_tol={rel_tol}"
    );
}

/// For many random vectors across a range of dimensions (including odd tail
/// lengths and the common embedding sizes), assert the simsimd kernel and the
/// scalar kernel agree within 1e-4 relative tolerance for dot_product,
/// squared_euclidean, cosine (via the fused dot/magnitudes tuple), and
/// magnitude. This runs in the default (`simsimd`) `cargo test` binary and is
/// the guard that keeps the scalar fallback numerics honest in CI.
#[cfg(feature = "simsimd")]
#[test]
fn cross_path_simsimd_agrees_with_scalar() {
    use super::simd::{
        dot_and_magnitudes, dot_and_magnitudes_scalar, dot_product_scalar, dot_product_sum,
        squared_diff_sum, squared_diff_sum_scalar, squared_magnitude as squared_magnitude_simsimd,
        squared_magnitude_scalar,
    };

    const REL_TOL: f32 = 1e-4;
    let mut rng = SplitMix64::new(0x426_426);
    for &dim in &[1usize, 3, 7, 17, 384, 768, 1536, 3072] {
        for _ in 0..16 {
            let a = random_vec(&mut rng, dim);
            let b = random_vec(&mut rng, dim);

            // dot_product
            assert_agree(
                dot_product_sum(&a, &b),
                dot_product_scalar(&a, &b),
                REL_TOL,
                &format!("dot dim={dim}"),
            );

            // squared_euclidean
            assert_agree(
                squared_diff_sum(&a, &b),
                squared_diff_sum_scalar(&a, &b),
                REL_TOL,
                &format!("sq_euclidean dim={dim}"),
            );

            // magnitude (squared magnitude of a and of b)
            assert_agree(
                squared_magnitude_simsimd(&a),
                squared_magnitude_scalar(&a),
                REL_TOL,
                &format!("sq_magnitude(a) dim={dim}"),
            );
            assert_agree(
                squared_magnitude_simsimd(&b),
                squared_magnitude_scalar(&b),
                REL_TOL,
                &format!("sq_magnitude(b) dim={dim}"),
            );

            // cosine kernel: the fused (dot, |a|², |b|²) tuple that
            // cosine_similarity is built on. Compare each component, then the
            // derived cosine value.
            let (d_s, ma_s, mb_s) = dot_and_magnitudes(&a, &b);
            let (d_r, ma_r, mb_r) = dot_and_magnitudes_scalar(&a, &b);
            assert_agree(d_s, d_r, REL_TOL, &format!("cosine.dot dim={dim}"));
            assert_agree(ma_s, ma_r, REL_TOL, &format!("cosine.mag_a dim={dim}"));
            assert_agree(mb_s, mb_r, REL_TOL, &format!("cosine.mag_b dim={dim}"));

            let cos_s = d_s / (ma_s.sqrt() * mb_s.sqrt());
            let cos_r = d_r / (ma_r.sqrt() * mb_r.sqrt());
            assert_agree(cos_s, cos_r, REL_TOL, &format!("cosine dim={dim}"));
        }
    }
}
