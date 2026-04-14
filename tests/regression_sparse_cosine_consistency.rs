use aletheiadb::core::vector::{SparseVec, cosine_similarity, sparse_cosine_similarity};

#[test]
fn test_sparse_vs_dense_cosine_consistency_small_vectors() {
    // Vector with squared magnitude < 1e-30
    // 1e-16 squared is 1e-32, which is < 1e-30
    let v_dense = vec![1e-16];
    let v_sparse = SparseVec::new(vec![0], vec![1e-16], 1).unwrap();

    // Dense cosine similarity should treat this as zero vector and return 0.0
    let dense_sim = cosine_similarity(&v_dense, &v_dense).unwrap();
    assert_eq!(
        dense_sim, 0.0,
        "Dense cosine similarity should be 0.0 for near-zero vector"
    );

    // Sparse cosine similarity should ALSO treat this as zero vector and return 0.0
    // But due to bug, it likely returns 1.0 because threshold check is wrong
    let sparse_sim = sparse_cosine_similarity(&v_sparse, &v_sparse).unwrap();

    // This assertion is expected to fail if the bug exists
    assert_eq!(
        sparse_sim, 0.0,
        "Sparse cosine similarity should be 0.0 for near-zero vector, but got {}",
        sparse_sim
    );
}
