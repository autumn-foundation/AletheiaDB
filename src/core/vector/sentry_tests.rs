use super::simd;

// ============================================================================
// Ground Truth Implementations (Scalar)
// ============================================================================

fn scalar_dot_and_magnitudes(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    let mut dot = 0.0;
    let mut mag_a = 0.0;
    let mut mag_b = 0.0;

    for (&ai, &bi) in a.iter().zip(b.iter()) {
        dot += ai * bi;
        mag_a += ai * ai;
        mag_b += bi * bi;
    }
    (dot, mag_a, mag_b)
}

fn scalar_squared_diff_sum(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(&ai, &bi)| {
            let diff = ai - bi;
            diff * diff
        })
        .sum()
}

fn scalar_scale_in_place(v: &mut [f32], scalar: f32) {
    for x in v.iter_mut() {
        *x *= scalar;
    }
}

// ============================================================================
// Correctness Tests
// ============================================================================

#[test]
fn test_dot_and_magnitudes_correctness_all_lengths() {
    // 💣 Risk: SIMD implementations (AVX2/SSE2) often process data in chunks (8 or 4).
    // Edge cases occur when vector length is not a multiple of the chunk size.
    // Incorrect handling of the remainder loop can lead to wrong results or out-of-bounds reads.

    // Test every length from 1 to 128 to cover all remainder possibilities (0..7) multiple times.
    for len in 1..=128 {
        // Generate deterministic test vectors
        let a: Vec<f32> = (0..len).map(|i| (i as f32 * 0.1).sin()).collect();
        let b: Vec<f32> = (0..len).map(|i| (i as f32 * 0.1).cos()).collect();

        // 1. Compute Ground Truth
        let (dot_expected, mag_a_expected, mag_b_expected) = scalar_dot_and_magnitudes(&a, &b);

        // 2. Compute SIMD (which dispatches to AVX2/SSE2/Scalar based on CPU)
        let (dot_actual, mag_a_actual, mag_b_actual) = simd::dot_and_magnitudes(&a, &b);

        // 3. Verify
        // Use 1e-4 tolerance because FMA (Fused Multiply-Add) in AVX2 has higher precision
        // than scalar multiply-then-add, leading to slight divergence.
        let tolerance = 1e-4;

        assert!(
            (dot_actual - dot_expected).abs() < tolerance,
            "Len {}: dot mismatch. Expected {}, got {}", len, dot_expected, dot_actual
        );
        assert!(
            (mag_a_actual - mag_a_expected).abs() < tolerance,
            "Len {}: mag_a mismatch. Expected {}, got {}", len, mag_a_expected, mag_a_actual
        );
        assert!(
            (mag_b_actual - mag_b_expected).abs() < tolerance,
            "Len {}: mag_b mismatch. Expected {}, got {}", len, mag_b_expected, mag_b_actual
        );
    }
}

#[test]
fn test_squared_diff_sum_correctness_all_lengths() {
    // 💣 Risk: SIMD accumulation of differences.

    for len in 1..=128 {
        let a: Vec<f32> = (0..len).map(|i| (i as f32 * 0.2).sin() * 10.0).collect();
        let b: Vec<f32> = (0..len).map(|i| (i as f32 * 0.2).cos() * 10.0).collect();

        let expected = scalar_squared_diff_sum(&a, &b);
        let actual = simd::squared_diff_sum(&a, &b);

        // Use a relative tolerance for larger values, or a larger absolute tolerance.
        // For f32, machine epsilon is ~1.19e-7.
        // For a value ~3000, error can be ~3000 * 1.2e-7 ≈ 3.6e-4.
        // SIMD FMA accumulation can differ from scalar sum.
        let abs_diff = (actual - expected).abs();
        let max_val = actual.abs().max(expected.abs());
        let rel_err = if max_val > 1e-6 { abs_diff / max_val } else { 0.0 };

        // Allow up to 2e-6 relative error (approx 20 epsilons accumulated)
        // or 1e-3 absolute error (whichever passes).
        assert!(
            abs_diff < 1e-3 || rel_err < 2e-6,
            "Len {}: squared_diff_sum mismatch. Expected {}, got {}, abs_diff {}, rel_err {}",
            len, expected, actual, abs_diff, rel_err
        );
    }
}

#[test]
fn test_scale_in_place_correctness_all_lengths() {
    // 💣 Risk: In-place modification with SIMD.

    for len in 1..=128 {
        let original: Vec<f32> = (0..len).map(|i| i as f32).collect();
        let scale = 2.5;

        // Ground Truth
        let mut expected = original.clone();
        scalar_scale_in_place(&mut expected, scale);

        // SIMD
        let mut actual = original.clone();
        simd::scale_in_place(&mut actual, scale);

        // Verify element-wise
        actual.iter().zip(expected.iter()).enumerate().for_each(|(i, (act, exp))| {
            assert!(
                (act - exp).abs() < 1e-6,
                "Len {}: mismatch at index {}. Expected {}, got {}", len, i, exp, act
            );
        });
    }
}

#[test]
fn test_simd_alignment_agnostic() {
    // 💣 Risk: unsafe SIMD loads often require aligned memory.
    // Rust Vecs are aligned, but slices into them might not be (if we slice from odd offsets).
    // The implementation uses _mm_loadu_ps (unaligned load), so it should work.

    // Create a buffer larger than needed
    let buffer: Vec<f32> = (0..200).map(|i| i as f32).collect();

    // Test various unaligned offsets
    for offset in 1..8 {
        let len = 100;
        let slice = &buffer[offset..offset+len];

        // Use the slice as input. slice.as_ptr() will likely be unaligned relative to 32-byte (AVX) boundary.
        // e.g. f32 is 4 bytes. If buffer is aligned to 32, buffer[1] is at offset 4 (not 32-aligned).

        let expected_mag = scalar_dot_and_magnitudes(slice, slice).1; // mag_a
        let actual_mag = simd::dot_and_magnitudes(slice, slice).1;

        assert!(
            (actual_mag - expected_mag).abs() < 1e-4,
            "Offset {}: magnitude mismatch with unaligned slice", offset
        );
    }
}
