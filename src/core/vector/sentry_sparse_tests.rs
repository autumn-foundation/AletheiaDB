use super::*;
use crate::utils::error::{Error, VectorError};

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
            let mut x = (i as f32 * (seed as f32 + 1.0));
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
            let mut x = (i as f32 * (seed as f32 + 1.0));
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
