use super::*;

#[test]
fn test_negative_distance_regression() {
    // 🛡️ Sentry Regression Test: Seed 34 triggered negative distance with unstable formula.
    // This test ensures the stable formula (sum of squared differences) is used.

    let seed = 34;
    let size = 1000;
    let indices: Vec<u32> = (0..size).map(|i| i as u32).collect();
    // Deterministic generation that caused the issue
    let values: Vec<f32> = (0..size)
        .map(|i| {
            let mut x = i as f32 * (seed as f32 + 1.0);
            x = x % 1000.0 + 0.1;
            x
        })
        .collect();

    let vec = SparseVec::new(indices, values, size as u32).unwrap();
    let dist = sparse_squared_euclidean_distance(&vec, &vec).unwrap();

    // Should be exactly 0.0 for identical vectors, but definitely non-negative
    assert!(
        dist >= 0.0,
        "Distance should be non-negative, got {:.20}",
        dist
    );

    // Ideally it should be very close to 0.0
    assert!(
        dist < 1e-6,
        "Self distance should be close to 0, got {:.20}",
        dist
    );
}

#[test]
fn test_sparse_euclidean_distance_nan_regression() {
    // 🛡️ Sentry: Ensure sparse_euclidean_distance handles close vectors without returning NaN
    // If squared distance is negative, sqrt() returns NaN.

    let seed = 34;
    let size = 1000;
    let indices: Vec<u32> = (0..size).map(|i| i as u32).collect();
    let values: Vec<f32> = (0..size)
        .map(|i| {
            let mut x = i as f32 * (seed as f32 + 1.0);
            x = x % 1000.0 + 0.1;
            x
        })
        .collect();

    let vec = SparseVec::new(indices, values, size as u32).unwrap();
    let dist = sparse_euclidean_distance(&vec, &vec).unwrap();

    assert!(!dist.is_nan(), "Euclidean distance should not be NaN");
    assert!(dist >= 0.0);
}

#[test]
fn test_sparse_squared_euclidean_distance_correctness() {
    // Verify correctness against manual calculation for a simple case
    let a = SparseVec::new(vec![0, 2], vec![1.0, 3.0], 5).unwrap();
    let b = SparseVec::new(vec![0, 3], vec![2.0, 4.0], 5).unwrap();

    // a = [1, 0, 3, 0, 0]
    // b = [2, 0, 0, 4, 0]
    // diff = [-1, 0, 3, -4, 0]
    // sq_diff = [1, 0, 9, 16, 0]
    // sum = 1 + 9 + 16 = 26

    let dist = sparse_squared_euclidean_distance(&a, &b).unwrap();
    assert!((dist - 26.0).abs() < 1e-6, "Expected 26.0, got {}", dist);
}

#[test]
fn test_sparse_cosine_similarity_threshold_behavior() {
    // 🛡️ Sentry: Verify that vectors with squared magnitude below SQUARED_MAGNITUDE_THRESHOLD
    // are treated as zero vectors (similarity 0.0), even if their linear magnitude is above the threshold.
    //
    // Threshold is 1e-14.
    // Construct vector with squared magnitude 0.5e-14.
    // Value = sqrt(0.5e-14) ≈ 0.707e-7.
    // Magnitude = 0.707e-7.
    //
    // If logic uses magnitude < 1e-14, then 0.707e-7 < 1e-14 is FALSE.
    // So it treats it as valid -> returns 1.0 (self-similarity).
    //
    // If logic uses squared_magnitude < 1e-14, then 0.5e-14 < 1e-14 is TRUE.
    // So it treats it as zero -> returns 0.0.

    let val = (0.5 * 1e-14f32).sqrt();
    let a = SparseVec::new(vec![0], vec![val], 1).unwrap();

    // We expect it to be treated as zero vector because it's below the squared magnitude threshold
    // Current implementation fails this because it compares magnitude vs threshold
    let sim = sparse_cosine_similarity(&a, &a).unwrap();

    assert_eq!(
        sim, 0.0,
        "Vector with squared magnitude < threshold should be treated as zero"
    );
}
