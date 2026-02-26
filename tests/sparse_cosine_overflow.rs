use aletheiadb::core::id::NodeId;
use aletheiadb::core::vector::SparseVec;
use aletheiadb::index::vector::sparse::{ScoringMethod, SparseIndexConfig, SparseVectorIndex};

#[test]
fn test_sparse_cosine_overflow() {
    // This test uses values known to cause dot/magnitude > 1.0 due to precision issues
    // Values taken from repro_panic.rs:
    // a = [-8.161245e-22]
    // b = [-125.53673]

    // Config with Cosine scoring
    let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::Cosine);
    let index = SparseVectorIndex::new(config).unwrap();

    let val_a = -8.161245e-22f32;
    let val_b = -125.53673f32;

    // Construct sparse vectors
    // Note: val_a is subnormal but non-zero, so SparseVec should accept it.
    let vec_a = SparseVec::new(vec![0], vec![val_a], 100).unwrap();
    let vec_b = SparseVec::new(vec![0], vec![val_b], 100).unwrap();

    let id_a = NodeId::new(1).unwrap();

    index.add(id_a, &vec_a).unwrap();

    // Search with vec_b
    let results = index.search(&vec_b, 1).unwrap();

    assert_eq!(results.len(), 1);
    let score = results[0].1;

    println!("Score: {}", score);

    // Check if score is within [-1.0, 1.0]
    // Without clamping, this is expected to be ~1.0003322
    assert!(score <= 1.0, "Score {} exceeds 1.0", score);
    assert!(score >= -1.0, "Score {} is below -1.0", score);
}
