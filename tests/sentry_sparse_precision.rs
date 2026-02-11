use aletheiadb::core::vector::{SparseVec, sparse_squared_euclidean_distance};

#[test]
fn test_sparse_euclidean_precision_vs_expanded_formula() {
    // Demonstration of precision loss with the expanded formula ||a||^2 + ||b||^2 - 2ab
    // vs the stable direct formula sum((a-b)^2).

    let val = 1000.0f32;
    let epsilon = 1e-4f32;

    // a = [1000.0 + epsilon]
    // b = [1000.0]
    // Note: 1000.0 + 0.0001 is represented as 1000.00012207... in f32
    // So the actual difference is ~1.22e-4, and squared difference is ~1.49e-8.
    let val_plus_eps = val + epsilon;
    let actual_diff = val_plus_eps - val;
    let expected_sq_diff = actual_diff * actual_diff;

    let a = SparseVec::new(vec![0], vec![val_plus_eps], 10).unwrap();
    let b = SparseVec::new(vec![0], vec![val], 10).unwrap();

    let dist_sq = sparse_squared_euclidean_distance(&a, &b).unwrap();

    println!("Computed squared distance: {}", dist_sq);
    println!("Expected (based on f32 rep): {}", expected_sq_diff);

    // With expanded formula, this is 0.0.
    // With direct formula, this should match expected_sq_diff exactly.

    assert!(
        dist_sq > 0.0,
        "Squared distance should be positive (precision loss detected!)"
    );
    assert!(
        (dist_sq - expected_sq_diff).abs() < 1e-12,
        "Should match exact f32 difference squared"
    );
}
