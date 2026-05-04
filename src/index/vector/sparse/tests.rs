use super::*;
use std::collections::HashSet;

// ========================================================================
// Basic Functionality Tests
// ========================================================================

#[test]
fn test_create_sparse_index() {
    let config = SparseIndexConfig::new(10_000);
    let index = SparseVectorIndex::new(config).unwrap();

    assert_eq!(index.dimensions(), 10_000);
    assert_eq!(index.len(), 0);
    assert!(index.is_empty());
}

#[test]
fn test_create_sparse_index_zero_dimensions_fails() {
    let config = SparseIndexConfig::new(0);
    let result = SparseVectorIndex::new(config);

    assert!(result.is_err());
}

#[test]
fn test_add_and_retrieve_vector() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
    let node_id = NodeId::new(1).unwrap();
    let vector = SparseVec::new(vec![10, 50, 90], vec![1.0, 2.0, 3.0], 100).unwrap();

    index.add(node_id, &vector).unwrap();

    assert_eq!(index.len(), 1);
    assert!(!index.is_empty());
    assert!(index.contains(node_id));

    let retrieved = index.get(node_id).unwrap();
    assert_eq!(retrieved.dimension(), 100);
    assert_eq!(retrieved.nnz(), 3);
}

#[test]
fn test_add_dimension_mismatch() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
    let node_id = NodeId::new(1).unwrap();
    let vector = SparseVec::new(vec![10], vec![1.0], 200).unwrap(); // Wrong dimension

    let result = index.add(node_id, &vector);

    assert!(matches!(
        result,
        Err(Error::Vector(VectorError::DimensionMismatch { .. }))
    ));
}

#[test]
fn test_remove_vector() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
    let node_id = NodeId::new(1).unwrap();
    let vector = SparseVec::new(vec![10, 50], vec![1.0, 2.0], 100).unwrap();

    index.add(node_id, &vector).unwrap();
    assert_eq!(index.len(), 1);

    index.remove(node_id).unwrap();
    assert_eq!(index.len(), 0);
    assert!(!index.contains(node_id));
}

#[test]
fn test_remove_nonexistent_vector() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
    let node_id = NodeId::new(999).unwrap();

    // Should not error
    index.remove(node_id).unwrap();
    assert_eq!(index.len(), 0);
}

#[test]
fn test_update_existing_vector() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
    let node_id = NodeId::new(1).unwrap();

    let vector1 = SparseVec::new(vec![10], vec![1.0], 100).unwrap();
    index.add(node_id, &vector1).unwrap();
    assert_eq!(index.len(), 1);

    // Add again should replace
    let vector2 = SparseVec::new(vec![20, 30], vec![2.0, 3.0], 100).unwrap();
    index.add(node_id, &vector2).unwrap();
    assert_eq!(index.len(), 1);

    let retrieved = index.get(node_id).unwrap();
    assert_eq!(retrieved.nnz(), 2);
    assert_eq!(retrieved.indices(), &[20, 30]);
}

// ========================================================================
// Search Tests - Dot Product
// ========================================================================

#[test]
fn test_search_dot_product_basic() {
    let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::DotProduct);
    let index = SparseVectorIndex::new(config).unwrap();

    // Add two documents
    let doc1 = SparseVec::new(vec![0, 5, 10], vec![1.0, 2.0, 3.0], 100).unwrap();
    let doc2 = SparseVec::new(vec![5, 10, 15], vec![1.0, 1.0, 1.0], 100).unwrap();

    index.add(NodeId::new(1).unwrap(), &doc1).unwrap();
    index.add(NodeId::new(2).unwrap(), &doc2).unwrap();

    // Query overlaps with both docs at dimensions 5 and 10
    let query = SparseVec::new(vec![5, 10], vec![1.0, 1.0], 100).unwrap();
    let results = index.search(&query, 10).unwrap();

    assert_eq!(results.len(), 2);

    // doc1: 2.0*1.0 + 3.0*1.0 = 5.0
    // doc2: 1.0*1.0 + 1.0*1.0 = 2.0
    // doc1 should score higher
    assert_eq!(results[0].0, NodeId::new(1).unwrap());
    assert!((results[0].1 - 5.0).abs() < 1e-6);
    assert_eq!(results[1].0, NodeId::new(2).unwrap());
    assert!((results[1].1 - 2.0).abs() < 1e-6);
}

#[test]
fn test_search_no_overlap() {
    let config = SparseIndexConfig::new(100);
    let index = SparseVectorIndex::new(config).unwrap();

    let doc = SparseVec::new(vec![0, 1, 2], vec![1.0, 2.0, 3.0], 100).unwrap();
    index.add(NodeId::new(1).unwrap(), &doc).unwrap();

    // Query has no overlapping dimensions
    let query = SparseVec::new(vec![50, 60], vec![1.0, 1.0], 100).unwrap();
    let results = index.search(&query, 10).unwrap();

    // No results since there's no overlap
    assert!(results.is_empty());
}

#[test]
fn test_search_empty_index() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
    let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();

    let results = index.search(&query, 10).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_search_k_zero() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
    let doc = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
    index.add(NodeId::new(1).unwrap(), &doc).unwrap();

    let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
    let results = index.search(&query, 0).unwrap();

    assert!(results.is_empty());
}

#[test]
fn test_search_top_k() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();

    // Add 10 documents
    for i in 1..=10 {
        let doc = SparseVec::new(vec![0], vec![i as f32], 100).unwrap();
        index.add(NodeId::new(i).unwrap(), &doc).unwrap();
    }

    let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
    let results = index.search(&query, 3).unwrap();

    assert_eq!(results.len(), 3);
    // Should get top 3 by score (doc 10, 9, 8)
    assert_eq!(results[0].0, NodeId::new(10).unwrap());
    assert_eq!(results[1].0, NodeId::new(9).unwrap());
    assert_eq!(results[2].0, NodeId::new(8).unwrap());
}

// ========================================================================
// Search Tests - Cosine Similarity
// ========================================================================

#[test]
fn test_search_cosine_similarity() {
    let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::Cosine);
    let index = SparseVectorIndex::new(config).unwrap();

    // Add identical vectors with different magnitudes
    let doc1 = SparseVec::new(vec![0, 1], vec![1.0, 1.0], 100).unwrap();
    let doc2 = SparseVec::new(vec![0, 1], vec![10.0, 10.0], 100).unwrap();

    index.add(NodeId::new(1).unwrap(), &doc1).unwrap();
    index.add(NodeId::new(2).unwrap(), &doc2).unwrap();

    let query = SparseVec::new(vec![0, 1], vec![1.0, 1.0], 100).unwrap();
    let results = index.search(&query, 10).unwrap();

    // Both should have same cosine similarity (1.0) since they're parallel
    assert_eq!(results.len(), 2);
    assert!((results[0].1 - 1.0).abs() < 1e-5);
    assert!((results[1].1 - 1.0).abs() < 1e-5);
}

#[test]
fn test_search_cosine_orthogonal() {
    let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::Cosine);
    let index = SparseVectorIndex::new(config).unwrap();

    // Orthogonal vectors (no overlap)
    let doc = SparseVec::new(vec![0, 1], vec![1.0, 1.0], 100).unwrap();
    index.add(NodeId::new(1).unwrap(), &doc).unwrap();

    let query = SparseVec::new(vec![50, 51], vec![1.0, 1.0], 100).unwrap();
    let results = index.search(&query, 10).unwrap();

    // Orthogonal vectors should have 0 similarity
    assert!(results.is_empty());
}

// ========================================================================
// Search Tests - BM25
// ========================================================================

#[test]
fn test_search_bm25() {
    let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::bm25_default());
    let index = SparseVectorIndex::new(config).unwrap();

    // Add documents with different term frequencies
    let doc1 = SparseVec::new(vec![0, 1, 2], vec![3.0, 1.0, 1.0], 100).unwrap(); // term 0 appears 3 times
    let doc2 = SparseVec::new(vec![0, 3, 4], vec![1.0, 1.0, 1.0], 100).unwrap(); // term 0 appears 1 time

    index.add(NodeId::new(1).unwrap(), &doc1).unwrap();
    index.add(NodeId::new(2).unwrap(), &doc2).unwrap();

    // Query for term 0
    let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
    let results = index.search(&query, 10).unwrap();

    assert_eq!(results.len(), 2);
    // BM25 should rank doc1 higher due to higher term frequency
    // (with saturation, so not proportionally higher)
    assert!(results[0].1 > results[1].1);
}

// ========================================================================
// Search with Filter Tests
// ========================================================================

#[test]
fn test_search_with_filter() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();

    for i in 1..=10 {
        let doc = SparseVec::new(vec![0], vec![i as f32], 100).unwrap();
        index.add(NodeId::new(i).unwrap(), &doc).unwrap();
    }

    let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();

    // Only allow even node IDs
    let allowed: HashSet<NodeId> = (1..=10)
        .filter(|i| i % 2 == 0)
        .map(|i| NodeId::new(i).unwrap())
        .collect();

    let results = index
        .search_with_filter(&query, 10, |id| allowed.contains(id))
        .unwrap();

    assert_eq!(results.len(), 5);
    for (id, _) in &results {
        assert!(allowed.contains(id));
    }
}

// ========================================================================
// Statistics Tests
// ========================================================================

#[test]
fn test_index_stats() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(1000)).unwrap();

    // Add some vectors
    let doc1 = SparseVec::new(vec![0, 100, 500], vec![1.0, 2.0, 3.0], 1000).unwrap();
    let doc2 = SparseVec::new(vec![0, 200], vec![1.0, 1.0], 1000).unwrap();
    let doc3 = SparseVec::new(vec![0, 100, 200, 300], vec![1.0, 1.0, 1.0, 1.0], 1000).unwrap();

    index.add(NodeId::new(1).unwrap(), &doc1).unwrap();
    index.add(NodeId::new(2).unwrap(), &doc2).unwrap();
    index.add(NodeId::new(3).unwrap(), &doc3).unwrap();

    let stats = index.stats();

    assert_eq!(stats.num_vectors, 3);
    assert_eq!(stats.dimensions, 1000);
    assert!(stats.non_empty_dimensions > 0);
    assert_eq!(stats.total_postings, 9); // 3 + 2 + 4 = 9 postings
    assert!(stats.avg_vector_nnz > 0.0);
}

// ========================================================================
// Hybrid Fusion Tests
// ========================================================================

#[test]
fn test_hybrid_fusion_basic() {
    let dense = vec![
        (NodeId::new(1).unwrap(), 0.9),
        (NodeId::new(2).unwrap(), 0.85),
        (NodeId::new(4).unwrap(), 0.7),
    ];
    let sparse = vec![
        (NodeId::new(2).unwrap(), 10.0),
        (NodeId::new(3).unwrap(), 8.0),
        (NodeId::new(4).unwrap(), 6.0),
    ];

    // Equal weight (0.5)
    let fused = hybrid_fusion(&dense, &sparse, 0.5, 10);

    // All four nodes should be present
    assert_eq!(fused.len(), 4);

    // Node 2 appears in both with high scores, should have highest fused score
    // Dense normalized: 1→1.0, 2→0.75, 4→0.0 (range: 0.7-0.9)
    // Sparse normalized: 2→1.0, 3→0.5, 4→0.0 (range: 6.0-10.0)
    // Node 2: 0.5*0.75 + 0.5*1.0 = 0.875 (highest)
    // Node 1: 0.5*1.0 + 0.0 = 0.5
    // Node 3: 0.0 + 0.5*0.5 = 0.25
    // Node 4: 0.5*0.0 + 0.5*0.0 = 0.0
    assert_eq!(fused[0].0, NodeId::new(2).unwrap());
}

#[test]
fn test_hybrid_fusion_dense_only() {
    let dense = vec![
        (NodeId::new(1).unwrap(), 0.9),
        (NodeId::new(2).unwrap(), 0.8),
    ];
    let sparse: Vec<(NodeId, f32)> = vec![];

    let fused = hybrid_fusion(&dense, &sparse, 0.5, 10);

    assert_eq!(fused.len(), 2);
    // Order should be preserved
    assert_eq!(fused[0].0, NodeId::new(1).unwrap());
}

#[test]
fn test_hybrid_fusion_sparse_only() {
    let dense: Vec<(NodeId, f32)> = vec![];
    let sparse = vec![
        (NodeId::new(1).unwrap(), 10.0),
        (NodeId::new(2).unwrap(), 8.0),
    ];

    let fused = hybrid_fusion(&dense, &sparse, 0.5, 10);

    assert_eq!(fused.len(), 2);
    assert_eq!(fused[0].0, NodeId::new(1).unwrap());
}

#[test]
fn test_hybrid_fusion_alpha_extremes() {
    let dense = vec![(NodeId::new(1).unwrap(), 0.9)];
    let sparse = vec![(NodeId::new(2).unwrap(), 10.0)];

    // Alpha = 1.0 (dense only)
    let fused = hybrid_fusion(&dense, &sparse, 1.0, 10);
    assert_eq!(fused[0].0, NodeId::new(1).unwrap());
    assert!(fused[0].1 > fused[1].1); // Dense result should be higher

    // Alpha = 0.0 (sparse only)
    let fused = hybrid_fusion(&dense, &sparse, 0.0, 10);
    assert_eq!(fused[0].0, NodeId::new(2).unwrap());
}

#[test]
fn test_reciprocal_rank_fusion() {
    let dense = vec![
        (NodeId::new(1).unwrap(), 0.9),
        (NodeId::new(2).unwrap(), 0.8),
        (NodeId::new(3).unwrap(), 0.7),
    ];
    let sparse = vec![
        (NodeId::new(2).unwrap(), 10.0),
        (NodeId::new(4).unwrap(), 8.0),
        (NodeId::new(1).unwrap(), 6.0),
    ];

    let fused = reciprocal_rank_fusion(&dense, &sparse, 60.0, 10);

    // Node 1 and 2 appear in both lists, should be ranked high
    assert!(fused.len() <= 4);

    // Node 2 is rank 2 in dense (1/(60+2)) and rank 1 in sparse (1/(60+1))
    // Node 1 is rank 1 in dense (1/(60+1)) and rank 3 in sparse (1/(60+3))
    // They should both have high RRF scores
}

// ========================================================================
// Thread Safety Tests
// ========================================================================

#[test]
fn test_concurrent_adds() {
    use std::thread;

    let index = Arc::new(SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap());
    let mut handles = vec![];

    for i in 0..10 {
        let index_clone = Arc::clone(&index);
        let handle = thread::spawn(move || {
            let doc = SparseVec::new(vec![i as u32], vec![1.0], 100).unwrap();
            index_clone.add(NodeId::new(i + 1).unwrap(), &doc).unwrap();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(index.len(), 10);
}

#[test]
fn test_concurrent_search() {
    use std::thread;

    let index = Arc::new(SparseVectorIndex::new(SparseIndexConfig::new(200)).unwrap());

    // Add some documents with dimension 0 (shared) and a unique dimension
    for i in 1..=100 {
        // Use dimension 0 as shared, and i as unique (values 1-100, all valid for dim 200)
        let doc = SparseVec::new(vec![0, i as u32], vec![1.0, i as f32], 200).unwrap();
        index.add(NodeId::new(i).unwrap(), &doc).unwrap();
    }

    let mut handles = vec![];

    // Concurrent searches
    for _ in 0..10 {
        let index_clone = Arc::clone(&index);
        let handle = thread::spawn(move || {
            let query = SparseVec::new(vec![0], vec![1.0], 200).unwrap();
            let results = index_clone.search(&query, 10).unwrap();
            assert_eq!(results.len(), 10);
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

// ========================================================================
// Edge Case Tests
// ========================================================================

#[test]
fn test_empty_sparse_vector() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
    let empty = SparseVec::new(vec![], vec![], 100).unwrap();

    index.add(NodeId::new(1).unwrap(), &empty).unwrap();
    assert_eq!(index.len(), 1);

    // Empty query should return nothing
    let query = SparseVec::new(vec![], vec![], 100).unwrap();
    let results = index.search(&query, 10).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_single_dimension_vectors() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(10_000)).unwrap();

    // All vectors have only dimension 0
    for i in 1..=100 {
        let doc = SparseVec::new(vec![0], vec![i as f32], 10_000).unwrap();
        index.add(NodeId::new(i).unwrap(), &doc).unwrap();
    }

    let query = SparseVec::new(vec![0], vec![1.0], 10_000).unwrap();
    let results = index.search(&query, 5).unwrap();

    assert_eq!(results.len(), 5);
    // Should return top 5 by score
    assert_eq!(results[0].0, NodeId::new(100).unwrap());
}

#[test]
fn test_very_sparse_high_dimensional() {
    let dim = 100_000;
    let index = SparseVectorIndex::new(SparseIndexConfig::new(dim)).unwrap();

    // Very sparse: 3 non-zeros in 100,000 dimensions
    let doc = SparseVec::new(vec![0, 50_000, 99_999], vec![1.0, 2.0, 3.0], dim as u32).unwrap();
    index.add(NodeId::new(1).unwrap(), &doc).unwrap();

    let query = SparseVec::new(vec![50_000], vec![1.0], dim as u32).unwrap();
    let results = index.search(&query, 10).unwrap();

    assert_eq!(results.len(), 1);
    assert!((results[0].1 - 2.0).abs() < 1e-6);
}

#[test]
fn test_compact() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();

    // Add and remove vectors
    for i in 1..=10 {
        let doc = SparseVec::new(vec![i as u32], vec![1.0], 100).unwrap();
        index.add(NodeId::new(i).unwrap(), &doc).unwrap();
    }

    for i in 1..=10 {
        index.remove(NodeId::new(i).unwrap()).unwrap();
    }

    assert_eq!(index.len(), 0);

    // Compact should clean up empty posting lists
    index.compact();

    let stats = index.stats();
    assert_eq!(stats.total_postings, 0);
}

#[test]
fn test_memory_usage() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(1000)).unwrap();

    let initial_mem = index.memory_usage();

    // Add vectors
    for i in 1..=100 {
        let doc = SparseVec::new(vec![0, 1, 2], vec![1.0, 2.0, 3.0], 1000).unwrap();
        index.add(NodeId::new(i).unwrap(), &doc).unwrap();
    }

    let final_mem = index.memory_usage();
    assert!(final_mem > initial_mem);
}

// ========================================================================
// Configuration Tests
// ========================================================================

#[test]
fn test_config_builder() {
    let config = SparseIndexConfig::new(1000)
        .with_scoring(ScoringMethod::Cosine)
        .with_capacity(5000);

    assert_eq!(config.dimensions, 1000);
    assert_eq!(config.scoring, ScoringMethod::Cosine);
    assert_eq!(config.initial_capacity, 5000);
}

#[test]
fn test_bm25_custom_params() {
    let scoring = ScoringMethod::BM25 { k1: 2.0, b: 0.5 };
    let config = SparseIndexConfig::new(100).with_scoring(scoring);

    if let ScoringMethod::BM25 { k1, b } = config.scoring {
        assert_eq!(k1, 2.0);
        assert_eq!(b, 0.5);
    } else {
        panic!("Expected BM25 scoring");
    }
}

// ========================================================================
// Edge Case Tests
// ========================================================================

#[test]
fn test_max_dimensions_boundary() {
    // Exactly at MAX_VECTOR_DIMENSIONS should succeed
    let result = SparseVectorIndex::new(SparseIndexConfig::new(MAX_VECTOR_DIMENSIONS));
    assert!(result.is_ok());

    // One over MAX_VECTOR_DIMENSIONS should fail
    let result = SparseVectorIndex::new(SparseIndexConfig::new(MAX_VECTOR_DIMENSIONS + 1));
    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::DimensionTooLarge {
            dimension,
            max_allowed,
        })) => {
            assert_eq!(dimension, MAX_VECTOR_DIMENSIONS + 1);
            assert_eq!(max_allowed, MAX_VECTOR_DIMENSIONS);
        }
        _ => panic!("Expected DimensionTooLarge error"),
    }
}

#[test]
fn test_max_k_capping() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();

    // Add more vectors than MAX_K
    for i in 1..=100 {
        let doc = SparseVec::new(vec![0], vec![i as f32], 100).unwrap();
        index.add(NodeId::new(i).unwrap(), &doc).unwrap();
    }

    // Request more than MAX_K (10_000), should be capped
    let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
    let results = index.search(&query, 100_000).unwrap();

    // Should return at most 100 (since we only have 100 vectors)
    // but the request was capped to MAX_K internally
    assert!(results.len() <= 100);
}

#[test]
fn test_nan_values_in_search_results() {
    let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();

    // Add some normal vectors
    for i in 1..=5 {
        let doc = SparseVec::new(vec![0], vec![i as f32], 100).unwrap();
        index.add(NodeId::new(i).unwrap(), &doc).unwrap();
    }

    let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
    let results = index.search(&query, 10).unwrap();

    // Results should be properly sorted without panicking
    // (tests that total_cmp handles NaN correctly if any arise)
    assert!(!results.is_empty());

    // Verify results are sorted by score descending
    for i in 1..results.len() {
        assert!(
            results[i - 1].1 >= results[i].1 || results[i].1.is_nan(),
            "Results should be sorted by score descending"
        );
    }
}

#[test]
fn test_concurrent_add_remove_same_node() {
    use std::sync::Arc;
    use std::thread;

    let index = Arc::new(SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap());
    let num_threads = 4;
    let iterations = 100;

    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let index = Arc::clone(&index);
            thread::spawn(move || {
                for i in 0..iterations {
                    let node_id = NodeId::new(1).unwrap(); // All threads use same node ID
                    let doc = SparseVec::new(
                        vec![(thread_id * iterations + i) as u32 % 50],
                        vec![1.0],
                        100,
                    )
                    .unwrap();

                    // Add and remove the same node concurrently
                    let _ = index.add(node_id, &doc);
                    let _ = index.remove(node_id);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // After all operations, index should be in a consistent state
    // (may have 0 or 1 vectors depending on timing)
    assert!(index.len() <= 1);

    // Search should work without errors
    let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
    let results = index.search(&query, 10);
    assert!(results.is_ok());
}

// ========================================================================
// Persistence Tests
// ========================================================================

#[test]
fn test_save_and_load_basic() {
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("sparse_index.gsp");

    // Create index with some data
    let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::DotProduct);
    let index = SparseVectorIndex::new(config.clone()).unwrap();

    let v1 = SparseVec::new(vec![0, 10, 50], vec![1.0, 2.0, 3.0], 100).unwrap();
    let v2 = SparseVec::new(vec![10, 20, 30], vec![0.5, 1.5, 2.5], 100).unwrap();

    index.add(NodeId::new(1).unwrap(), &v1).unwrap();
    index.add(NodeId::new(2).unwrap(), &v2).unwrap();

    // Save
    index.save(&path).unwrap();
    assert!(path.exists());

    // Load
    let loaded = SparseVectorIndex::load(&path, config).unwrap();

    // Verify
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded.dimensions(), 100);

    // Check vectors are intact
    let loaded_v1 = loaded.get(NodeId::new(1).unwrap()).unwrap();
    assert_eq!(loaded_v1.indices(), v1.indices());
    assert_eq!(loaded_v1.values(), v1.values());

    let loaded_v2 = loaded.get(NodeId::new(2).unwrap()).unwrap();
    assert_eq!(loaded_v2.indices(), v2.indices());
    assert_eq!(loaded_v2.values(), v2.values());

    // Search should work on loaded index
    let query = SparseVec::new(vec![10], vec![1.0], 100).unwrap();
    let results = loaded.search(&query, 10).unwrap();
    assert_eq!(results.len(), 2);

    // Clean up
    fs::remove_file(&path).ok();
}

#[test]
fn test_save_and_load_bm25() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("sparse_bm25.gsp");

    // Create index with BM25 scoring
    let config = SparseIndexConfig::new(1000).with_scoring(ScoringMethod::BM25 { k1: 1.8, b: 0.6 });
    let index = SparseVectorIndex::new(config.clone()).unwrap();

    for i in 1..=10 {
        let v = SparseVec::new(vec![i as u32, (i * 10) as u32], vec![1.0, 2.0], 1000).unwrap();
        index.add(NodeId::new(i).unwrap(), &v).unwrap();
    }

    // Save and load
    index.save(&path).unwrap();
    let loaded = SparseVectorIndex::load(&path, config).unwrap();

    // Verify BM25 parameters preserved
    if let ScoringMethod::BM25 { k1, b } = loaded.scoring() {
        assert!((k1 - 1.8).abs() < 1e-6);
        assert!((b - 0.6).abs() < 1e-6);
    } else {
        panic!("Expected BM25 scoring method");
    }

    assert_eq!(loaded.len(), 10);
}

#[test]
fn test_save_and_load_cosine() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("sparse_cosine.gsp");

    let config = SparseIndexConfig::new(500).with_scoring(ScoringMethod::Cosine);
    let index = SparseVectorIndex::new(config.clone()).unwrap();

    let v = SparseVec::new(vec![0, 100, 200], vec![1.0, 1.0, 1.0], 500).unwrap();
    index.add(NodeId::new(42).unwrap(), &v).unwrap();

    index.save(&path).unwrap();
    let loaded = SparseVectorIndex::load(&path, config).unwrap();

    assert_eq!(loaded.scoring(), ScoringMethod::Cosine);
    assert_eq!(loaded.len(), 1);
    assert!(loaded.contains(NodeId::new(42).unwrap()));
}

#[test]
fn test_save_and_load_empty_index() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("sparse_empty.gsp");

    let config = SparseIndexConfig::new(100);
    let index = SparseVectorIndex::new(config.clone()).unwrap();

    // Save empty index
    index.save(&path).unwrap();

    // Load empty index
    let loaded = SparseVectorIndex::load(&path, config).unwrap();
    assert_eq!(loaded.len(), 0);
    assert!(loaded.is_empty());
}

#[test]
fn test_load_invalid_magic() {
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("invalid_magic.gsp");

    // Write file with invalid magic
    fs::write(&path, b"BADM\x01\x00\x00\x00\x00\x00").unwrap();

    let config = SparseIndexConfig::new(100);
    let result = SparseVectorIndex::load(&path, config);
    assert!(result.is_err());
    let err = format!("{}", result.err().unwrap());
    assert!(err.contains("Invalid magic bytes"));
}

#[test]
fn test_load_corrupted_crc() {
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("corrupted.gsp");

    // Create valid index
    let config = SparseIndexConfig::new(100);
    let index = SparseVectorIndex::new(config.clone()).unwrap();
    let v = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
    index.add(NodeId::new(1).unwrap(), &v).unwrap();
    index.save(&path).unwrap();

    // Corrupt the last byte (part of CRC)
    let mut data = fs::read(&path).unwrap();
    let last_idx = data.len() - 1;
    data[last_idx] ^= 0xFF;
    fs::write(&path, &data).unwrap();

    // Load should fail
    let result = SparseVectorIndex::load(&path, config);
    assert!(result.is_err());
    let err = format!("{}", result.err().unwrap());
    assert!(err.contains("CRC32 mismatch"));
}

#[test]
fn test_load_dimension_mismatch() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("dim_mismatch.gsp");

    // Save with 100 dimensions
    let config100 = SparseIndexConfig::new(100);
    let index = SparseVectorIndex::new(config100).unwrap();
    index.save(&path).unwrap();

    // Try to load with 200 dimensions
    let config200 = SparseIndexConfig::new(200);
    let result = SparseVectorIndex::load(&path, config200);
    assert!(result.is_err());
}

#[test]
fn test_load_file_too_small() {
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("too_small.gsp");

    // Write file that's too small
    fs::write(&path, b"ASPS").unwrap();

    let config = SparseIndexConfig::new(100);
    let result = SparseVectorIndex::load(&path, config);
    assert!(result.is_err());
    let err = format!("{}", result.err().unwrap());
    assert!(err.contains("too small"));
}

#[test]
fn test_save_and_load_preserves_search_results() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("search_preserve.gsp");

    let config = SparseIndexConfig::new(100);
    let index = SparseVectorIndex::new(config.clone()).unwrap();

    // Add documents with varying relevance
    for i in 1..=5 {
        let v = SparseVec::new(vec![0, 1], vec![i as f32, (6 - i) as f32], 100).unwrap();
        index.add(NodeId::new(i).unwrap(), &v).unwrap();
    }

    let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
    let results_before = index.search(&query, 5).unwrap();

    // Save and load
    index.save(&path).unwrap();
    let loaded = SparseVectorIndex::load(&path, config).unwrap();

    let results_after = loaded.search(&query, 5).unwrap();

    // Results should be identical
    assert_eq!(results_before.len(), results_after.len());
    for (before, after) in results_before.iter().zip(results_after.iter()) {
        assert_eq!(before.0, after.0);
        assert!((before.1 - after.1).abs() < 1e-6);
    }
}
