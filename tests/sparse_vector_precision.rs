use aletheiadb::core::vector::sparse_squared_euclidean_distance;
use aletheiadb::core::vector::SparseVec;

#[test]
fn test_sparse_precision_loss() {
    // 🧪 Strategy: Create sparse vectors that cause catastrophic cancellation
    // using the expanded form ||a||^2 + ||b||^2 - 2ab.
    // 💣 Risk: If the implementation uses the expanded form, it will return 0.0 instead of correct small distance.

    // a = [1000.0]
    // b = [1000.01]
    // Expected squared distance = (0.01)^2 = 0.0001

    let a = SparseVec::new(vec![0], vec![1000.0], 100).unwrap();
    let b = SparseVec::new(vec![0], vec![1000.01], 100).unwrap();

    let dist_sq = sparse_squared_euclidean_distance(&a, &b).unwrap();

    // Direct computation gives ~0.0001
    // Expanded form gives 0.0 (due to precision loss)

    println!("Computed distance: {}", dist_sq);

    // We expect it to be close to 0.0001.
    // If it's 0.0, it failed.
    assert!(dist_sq > 1e-5, "Distance should be non-zero (got {})", dist_sq);
    assert!((dist_sq - 0.0001).abs() < 1e-5, "Distance should be approx 0.0001 (got {})", dist_sq);
}
