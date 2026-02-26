use aletheiadb::core::id::NodeId;
use aletheiadb::core::vector::SparseVec;
use aletheiadb::index::vector::sparse::{ScoringMethod, SparseIndexConfig, SparseVectorIndex};

#[test]
fn test_sparse_cosine_overflow() {
    let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::Cosine);
    let index = SparseVectorIndex::new(config).unwrap();

    // Values from repro_panic.rs that caused > 1.0 result
    let val_a = -8.161245e-22f32;
    let val_b = -125.53673f32;

    let doc = SparseVec::new(vec![0], vec![val_b], 100).unwrap();
    let query = SparseVec::new(vec![0], vec![val_a], 100).unwrap();

    index.add(NodeId::new(1).unwrap(), &doc).unwrap();

    let results = index.search(&query, 10).unwrap();

    assert_eq!(results.len(), 1);
    let score = results[0].1;

    println!("Score: {}", score);

    // Check if score is within valid range [-1.0, 1.0]
    // We allow a very small epsilon for floating point representation, but strictly it should be clamped.
    // However, if the code doesn't clamp, this will likely be > 1.0.

    assert!(score <= 1.0, "Score {} exceeds 1.0", score);
    assert!(score >= -1.0, "Score {} is less than -1.0", score);
}
