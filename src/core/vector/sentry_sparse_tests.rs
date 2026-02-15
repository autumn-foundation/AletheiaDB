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
    // 🛡️ Sentry: Verify that vectors with squared magnitude < SQUARED_MAGNITUDE_THRESHOLD (1e-14)
    // are treated as zero vectors (similarity 0.0), even if their linear magnitude is > 1e-14.
    //
    // The previous implementation used `magnitude() < SQUARED_MAGNITUDE_THRESHOLD`, which
    // effectively compared `sqrt(sq_mag) < 1e-14`. This meant vectors with squared magnitude
    // between 1e-28 and 1e-14 were NOT treated as zero, potentially leading to instability.
    //
    // We construct a vector with squared magnitude = 0.9 * 1e-14 = 9e-15.
    // Its linear magnitude is sqrt(9e-15) ≈ 9.48e-8.
    //
    // If threshold check is correct (squared vs squared): 9e-15 < 1e-14 -> true -> return 0.0.
    // If threshold check is incorrect (linear vs squared): 9.48e-8 < 1e-14 -> false -> return 1.0 (self-similarity).

    use crate::core::vector::constants::SQUARED_MAGNITUDE_THRESHOLD;

    // Create a vector with a single element such that val^2 = 0.9 * threshold
    let target_sq_mag = 0.9 * SQUARED_MAGNITUDE_THRESHOLD;
    let val = target_sq_mag.sqrt();

    let vec = SparseVec::new(vec![0], vec![val], 10).unwrap();

    // Verify our construction
    let actual_sq_mag = vec.squared_magnitude();
    assert!((actual_sq_mag - target_sq_mag).abs() < 1e-20);
    assert!(actual_sq_mag < SQUARED_MAGNITUDE_THRESHOLD);

    // Compute similarity with itself
    // Should be 0.0 because it's considered a zero vector
    let similarity = sparse_cosine_similarity(&vec, &vec).unwrap();

    assert_eq!(
        similarity, 0.0,
        "Vector with squared magnitude < threshold should be treated as zero vector"
    );
}
