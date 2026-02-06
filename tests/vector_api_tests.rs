use aletheiadb::AletheiaDB;
use aletheiadb::WriteOps;
use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::index::vector::temporal::{
    DriftMetric, RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
};
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use std::sync::Arc;
use std::thread;

// ==================== Vector Index API Tests ====================

#[test]
fn test_enable_vector_index() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Trying to enable again should fail
    let config2 = HnswConfig::new(3, DistanceMetric::Cosine);
    assert!(db.enable_vector_index("embedding", config2).is_err());
}

#[test]
fn test_find_similar_basic() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Create nodes with vector embeddings
    let doc1 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Rust Programming")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let doc2 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Rust Advanced")
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                .build(),
        )
        .unwrap();

    let doc3 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Python Basics")
                .insert_vector("embedding", &[0.0f32, 1.0, 0.0])
                .build(),
        )
        .unwrap();

    // Find similar to doc1
    let similar = db.find_similar(doc1, 2).unwrap();

    // Should return 2 results (excluding doc1 itself)
    assert_eq!(similar.len(), 2);

    // doc2 should be most similar (both about Rust)
    assert_eq!(similar[0].0, doc2);
    assert!(similar[0].1 > 0.9); // High similarity

    // doc3 should be less similar
    assert_eq!(similar[1].0, doc3);
    assert!(similar[1].1 < 0.5); // Lower similarity
}

#[test]
fn test_find_similar_with_label() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Create Document nodes
    let doc1 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let doc2 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                .build(),
        )
        .unwrap();

    // Create Person nodes with similar embeddings
    let _person1 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.95f32, 0.05, 0.0])
                .build(),
        )
        .unwrap();

    // Find similar Documents only (should exclude Person nodes)
    let similar = db.find_similar_with_label(doc1, "Document", 5).unwrap();

    // Should only return doc2 (not person1)
    assert_eq!(similar.len(), 1);
    assert_eq!(similar[0].0, doc2);
}

#[test]
fn test_vector_index_not_enabled() {
    let db = AletheiaDB::new().unwrap();

    // Create node with vector
    let node_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    // Try to search without enabling index - should fail
    assert!(db.find_similar(node_id, 5).is_err());
}

#[test]
fn test_vector_index_with_euclidean_distance() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index with Euclidean distance
    let config = HnswConfig::new(3, DistanceMetric::Euclidean).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Create nodes with vector embeddings
    let doc1 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let doc2 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let doc3 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[10.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    // Find similar to doc1
    let similar = db.find_similar(doc1, 2).unwrap();

    assert_eq!(similar.len(), 2);

    // With Euclidean distance, doc2 (distance 1.0) should be closer than doc3 (distance 10.0)
    assert_eq!(similar[0].0, doc2);
    assert_eq!(similar[1].0, doc3);
}

#[test]
fn test_vector_index_with_large_k() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Create 5 nodes
    let mut node_ids = Vec::new();
    for i in 0..5 {
        let node_id = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[i as f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();
        node_ids.push(node_id);
    }

    // Request k=10 (more than available)
    let similar = db.find_similar(node_ids[0], 10).unwrap();

    // Should return at most 4 results (5 total - 1 query node)
    assert!(similar.len() <= 4);
}

/// Regression test for VS-030 bug: nodes created via write transactions
/// must be indexed for vector search.
///
/// Prior to fix: insert_node_direct() only called indexes.insert_node(),
/// skipping try_index_vector(). This meant all transaction-created nodes
/// were missing from the HNSW index, causing find_similar to return empty results.
#[test]
fn test_transaction_nodes_are_indexed() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Create nodes via write transaction (not convenience method)
    let (doc1, doc2, _doc3) = db
        .write(|tx| {
            let d1 = tx.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )?;
            let d2 = tx.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                    .build(),
            )?;
            let d3 = tx.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0f32, 1.0, 0.0])
                    .build(),
            )?;
            Ok((d1, d2, d3))
        })
        .unwrap();

    // CRITICAL: These nodes were created via transaction, not db.create_node()
    // Before the fix, insert_node_direct() didn't index vectors, so this would fail
    let similar = db.find_similar(doc1, 2).unwrap();

    // Should find doc2 and doc3
    assert_eq!(similar.len(), 2);
    assert_eq!(similar[0].0, doc2); // Most similar
    assert!(similar[0].1 > 0.9); // High similarity
}

#[test]
fn test_find_similar_by_embedding() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Create nodes with vector embeddings
    let doc1 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Rust Programming")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let doc2 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Rust Advanced")
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                .build(),
        )
        .unwrap();

    let _doc3 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Python Basics")
                .insert_vector("embedding", &[0.0f32, 1.0, 0.0])
                .build(),
        )
        .unwrap();

    // Search with an external query embedding (similar to doc1)
    let query_embedding = [0.95f32, 0.05, 0.0];
    let similar = db.find_similar_by_embedding(&query_embedding, 2).unwrap();

    // Should return doc1 first (most similar to query), then doc2
    assert_eq!(similar.len(), 2);
    assert_eq!(similar[0].0, doc1); // Most similar
    assert!(similar[0].1 > 0.99); // Very high similarity
    assert_eq!(similar[1].0, doc2);
}

#[test]
fn test_find_similar_by_embedding_with_label() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Create Document nodes
    let doc1 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let doc2 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                .build(),
        )
        .unwrap();

    // Create Person nodes with similar embeddings
    let _person1 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.95f32, 0.05, 0.0])
                .build(),
        )
        .unwrap();

    // Search for Documents only with query embedding
    let query_embedding = [1.0f32, 0.0, 0.0];
    let similar = db
        .find_similar_by_embedding_with_label(&query_embedding, "Document", 5)
        .unwrap();

    // Should only return Documents (doc1 and doc2), not person1
    assert_eq!(similar.len(), 2);
    assert!(similar.iter().any(|(id, _)| *id == doc1));
    assert!(similar.iter().any(|(id, _)| *id == doc2));
}

#[test]
fn test_find_similar_by_embedding_dimension_mismatch() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index with 3 dimensions
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Create a node
    db.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
            .build(),
    )
    .unwrap();

    // Try to search with wrong dimensions (4 instead of 3)
    let wrong_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let result = db.find_similar_by_embedding(&wrong_embedding, 5);

    // Should fail with dimension mismatch error
    assert!(result.is_err());
}

#[test]
fn test_find_similar_empty_database() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index but don't add any nodes
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Search should return empty results, not error
    let query_embedding = [1.0f32, 0.0, 0.0];
    let results = db.find_similar_by_embedding(&query_embedding, 10).unwrap();

    assert_eq!(results.len(), 0);
}

#[test]
fn test_find_similar_k_zero() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index and add some nodes
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    db.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
            .build(),
    )
    .unwrap();

    // Search with k=0 should return empty results, not error
    let query_embedding = [1.0f32, 0.0, 0.0];
    let results = db.find_similar_by_embedding(&query_embedding, 0).unwrap();

    assert_eq!(results.len(), 0);
}

#[test]
fn test_concurrent_vector_indexing() {
    let db = Arc::new(AletheiaDB::new().unwrap());

    // Enable vector index
    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(1000);
    db.enable_vector_index("embedding", config).unwrap();

    // Spawn multiple threads that create nodes with vectors concurrently
    let mut handles = vec![];
    for i in 0..10 {
        let db_clone = Arc::clone(&db);
        let handle = thread::spawn(move || {
            // Use non-zero vectors to avoid issues with cosine similarity
            let base = (i as f32 + 1.0) / 10.0;
            db_clone
                .create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[base, base, base, base])
                        .build(),
                )
                .unwrap()
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    let node_ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // Verify all nodes were indexed - search should return results
    // (Note: find_similar excludes the query node, so we check for OTHER nodes)
    for node_id in &node_ids {
        let results = db.find_similar(*node_id, 5).unwrap();
        // With 10 nodes and k=5, we should get at least 4 results (excluding query node)
        // HNSW is approximate, so we allow for slight variation
        assert!(
            results.len() >= 4,
            "Expected >=4 results for node {:?}, got {}",
            node_id,
            results.len()
        );
        // Verify results don't include the query node (it's excluded by design)
        assert!(
            results.iter().all(|(id, _)| *id != *node_id),
            "Query node {:?} should not appear in its own results",
            node_id
        );
        // Verify similarity scores are reasonable (between 0 and 1)
        for (_, score) in &results {
            assert!(
                (0.0..=1.0).contains(score),
                "Similarity score {} out of range",
                score
            );
        }
    }

    // Verify total count
    assert_eq!(db.node_count(), 10);
}

#[test]
fn test_find_similar_with_missing_property() {
    let db = AletheiaDB::new().unwrap();

    // Enable vector index on "embedding" property
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Create some nodes with the indexed property
    let doc1 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let _doc2 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                .build(),
        )
        .unwrap();

    // Create a node WITHOUT the indexed property (should be ignored in searches)
    let _doc_no_vector = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "No embedding")
                .build(),
        )
        .unwrap();

    // Search should only find nodes with the property
    let results = db.find_similar(doc1, 5).unwrap();

    // Should find doc2 but not doc_no_vector
    assert_eq!(results.len(), 1); // Only doc2 (doc1 is excluded as query node)
}

// ========================================================================
// Tests for Issue #389: VectorIndexBuilder pattern
// ========================================================================

/// Test basic builder pattern for enabling vector index.
#[test]
fn test_vector_index_builder_basic() {
    let db = AletheiaDB::new().unwrap();

    // Builder pattern API
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable()
        .expect("Should enable vector index via builder");

    // Verify it was enabled
    assert!(db.has_vector_index("embedding"));
}

/// Test builder pattern with multiple properties.
#[test]
fn test_vector_index_builder_multiple_properties() {
    let db = AletheiaDB::new().unwrap();

    // Enable two indexes via builder
    db.vector_index("title_embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable()
        .unwrap();

    db.vector_index("body_embedding")
        .hnsw(HnswConfig::new(8, DistanceMetric::Euclidean).with_capacity(100))
        .enable()
        .unwrap();

    // Both should be enabled
    assert!(db.has_vector_index("title_embedding"));
    assert!(db.has_vector_index("body_embedding"));

    // Verify different configs
    let indexes = db.list_vector_indexes();
    assert_eq!(indexes.len(), 2);
}

/// Test builder pattern without calling hnsw() fails.
#[test]
fn test_vector_index_builder_missing_hnsw_fails() {
    let db = AletheiaDB::new().unwrap();

    // Calling enable() without hnsw() should fail
    let result = db.vector_index("embedding").enable();
    assert!(result.is_err(), "Should fail without HNSW config");
}

/// Test builder with temporal config auto-enables current index.
///
/// This is the key DX improvement from #386: users shouldn't need to call
/// both enable_vector_index() and enable_temporal_vector_index().
#[test]
fn test_vector_index_builder_with_temporal() {
    let db = AletheiaDB::new().unwrap();
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

    // Single call enables both current and temporal indexing
    db.vector_index("embedding")
        .hnsw(hnsw_config.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config), // Will be overwritten by builder, but needed for struct completeness
        })
        .enable()
        .expect("Should enable both current and temporal index");

    // Current index should be auto-enabled
    assert!(db.has_vector_index("embedding"));

    // Temporal index should also be enabled
    assert!(db.is_temporal_vector_index_enabled());
}

/// Test builder creates working index that can be searched.
#[test]
fn test_vector_index_builder_functional() {
    let db = AletheiaDB::new().unwrap();

    db.vector_index("embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable()
        .unwrap();

    // Create nodes with embeddings
    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let v2 = vec![0.9f32, 0.1, 0.0, 0.0];

    let node1 = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v1)
                .build(),
        )
        .unwrap();
    let _node2 = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v2)
                .build(),
        )
        .unwrap();

    // Search should work
    let results = db.find_similar(node1, 1).unwrap();
    assert_eq!(results.len(), 1);
}

/// Test re-enabling same property via builder fails.
#[test]
fn test_vector_index_builder_same_property_twice_fails() {
    let db = AletheiaDB::new().unwrap();

    db.vector_index("embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable()
        .unwrap();

    // Second enable on same property should fail
    let result = db
        .vector_index("embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable();

    assert!(
        result.is_err(),
        "Should not allow re-enabling same property"
    );
}

// ========================================================================
// Tests for Issue #389: Query API with explicit property specification
// ========================================================================

/// Test find_similar_in() with explicit property specification.
#[test]
fn test_find_similar_in_explicit_property() {
    let db = AletheiaDB::new().unwrap();

    // Enable two different indexes
    db.vector_index("title_embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable()
        .unwrap();

    db.vector_index("body_embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable()
        .unwrap();

    // Create nodes with different embeddings for each property
    let title_v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let title_v2 = vec![0.9f32, 0.1, 0.0, 0.0];
    let body_v1 = vec![0.0f32, 1.0, 0.0, 0.0];
    let body_v2 = vec![0.0f32, 0.9, 0.1, 0.0];

    let node1 = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &title_v1)
                .insert_vector("body_embedding", &body_v1)
                .build(),
        )
        .unwrap();

    let _node2 = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &title_v2)
                .insert_vector("body_embedding", &body_v2)
                .build(),
        )
        .unwrap();

    // Search by title embedding
    let title_results = db.find_similar_in("title_embedding", node1, 1).unwrap();
    assert_eq!(title_results.len(), 1);

    // Search by body embedding
    let body_results = db.find_similar_in("body_embedding", node1, 1).unwrap();
    assert_eq!(body_results.len(), 1);
}

/// Test search_vectors_in() with explicit property specification.
#[test]
fn test_search_vectors_in_explicit_property() {
    let db = AletheiaDB::new().unwrap();

    db.vector_index("embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable()
        .unwrap();

    // Create nodes
    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let v2 = vec![0.9f32, 0.1, 0.0, 0.0];

    db.create_node(
        "Doc",
        PropertyMapBuilder::new()
            .insert_vector("embedding", &v1)
            .build(),
    )
    .unwrap();

    db.create_node(
        "Doc",
        PropertyMapBuilder::new()
            .insert_vector("embedding", &v2)
            .build(),
    )
    .unwrap();

    // Search with raw embedding (not tied to a node)
    let query = vec![1.0f32, 0.0, 0.0, 0.0];
    let results = db.search_vectors_in("embedding", &query, 2).unwrap();
    assert_eq!(results.len(), 2);
}

/// Test find_similar_in() with non-existent property fails.
#[test]
fn test_find_similar_in_nonexistent_property_fails() {
    let db = AletheiaDB::new().unwrap();

    db.vector_index("embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable()
        .unwrap();

    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let node1 = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v1)
                .build(),
        )
        .unwrap();

    // Search with wrong property name should fail
    let result = db.find_similar_in("nonexistent", node1, 1);
    assert!(result.is_err());
}

// ========================================================================
// Tests for Issue #389: Temporal property-specific queries
// ========================================================================

/// Test find_similar_as_of_in() with explicit property specification.
#[test]
fn test_find_similar_as_of_in_explicit_property() {
    let db = AletheiaDB::new().unwrap();
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

    // Enable temporal vector index for a specific property
    db.vector_index("content_embedding")
        .hnsw(hnsw_config.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1), // Snapshot every tx
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        })
        .enable()
        .expect("Should enable temporal index");

    // Create a node with an embedding
    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let _node1 = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("content_embedding", &v1)
                .build(),
        )
        .unwrap();

    // Get a timestamp after the node was created
    let timestamp = aletheiadb::core::temporal::time::now();

    // Query with property-specific temporal search
    let query = vec![0.9f32, 0.1, 0.0, 0.0];
    let results = db
        .find_similar_as_of_in("content_embedding", &query, 10, timestamp)
        .unwrap();

    // Should find the node
    assert!(!results.is_empty());
}

/// Test find_similar_as_of_in() with wrong property fails.
#[test]
fn test_find_similar_as_of_in_wrong_property_fails() {
    let db = AletheiaDB::new().unwrap();
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

    // Enable temporal index for "embedding" property
    db.vector_index("embedding")
        .hnsw(hnsw_config.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        })
        .enable()
        .expect("Should enable temporal index");

    let timestamp = aletheiadb::core::temporal::time::now();
    let query = vec![1.0f32, 0.0, 0.0, 0.0];

    // Query with WRONG property name should fail
    let result = db.find_similar_as_of_in("wrong_property", &query, 10, timestamp);
    assert!(
        result.is_err(),
        "Should fail when property doesn't match temporal index"
    );
}

/// Test find_similar_as_of_in() when temporal index not enabled.
#[test]
fn test_find_similar_as_of_in_no_temporal_index() {
    let db = AletheiaDB::new().unwrap();

    // Only enable regular HNSW index, not temporal
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable()
        .unwrap();

    let timestamp = aletheiadb::core::temporal::time::now();
    let query = vec![1.0f32, 0.0, 0.0, 0.0];

    // Temporal query should fail when temporal index not enabled
    let result = db.find_similar_as_of_in("embedding", &query, 10, timestamp);
    assert!(
        result.is_err(),
        "Should fail when temporal index not enabled"
    );
}

/// Test track_drift_in() with explicit property name.
#[test]
fn test_track_drift_in_explicit_property() {
    use aletheiadb::core::temporal::TimeRange;

    let db = AletheiaDB::new().unwrap();
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

    // Enable temporal vector index for a specific property
    db.vector_index("content_embedding")
        .hnsw(hnsw_config.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1), // Snapshot every tx
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        })
        .enable()
        .expect("Should enable temporal index");

    // Create a node with an embedding
    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let node_id = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("content_embedding", &v1)
                .build(),
        )
        .unwrap();

    // Track drift over time using property-specific method
    // Even with just one snapshot, the API should work (may return empty or single result)
    let reference = vec![1.0f32, 0.0, 0.0, 0.0];
    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
    let result = db.track_drift_in("content_embedding", node_id, &reference, time_range);

    // Should succeed (not error) - method exists and property validation passes
    assert!(
        result.is_ok(),
        "track_drift_in should succeed with correct property"
    );
}

/// Test track_drift_in() with wrong property fails.
#[test]
fn test_track_drift_in_wrong_property_fails() {
    use aletheiadb::core::temporal::TimeRange;

    let db = AletheiaDB::new().unwrap();
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

    // Enable temporal index for "embedding" property
    db.vector_index("embedding")
        .hnsw(hnsw_config.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        })
        .enable()
        .expect("Should enable temporal index");

    let node_id = NodeId::new(1).unwrap();
    let reference = vec![1.0f32, 0.0, 0.0, 0.0];
    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

    // Query with WRONG property name should fail
    let result = db.track_drift_in("wrong_property", node_id, &reference, time_range);
    assert!(
        result.is_err(),
        "Should fail when property doesn't match temporal index"
    );
}

/// Test track_drift_in() when temporal index not enabled.
#[test]
fn test_track_drift_in_no_temporal_index() {
    use aletheiadb::core::temporal::TimeRange;

    let db = AletheiaDB::new().unwrap();

    // Only enable regular HNSW index, not temporal
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable()
        .unwrap();

    let node_id = NodeId::new(1).unwrap();
    let reference = vec![1.0f32, 0.0, 0.0, 0.0];
    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

    // Temporal query should fail when temporal index not enabled
    let result = db.track_drift_in("embedding", node_id, &reference, time_range);
    assert!(
        result.is_err(),
        "Should fail when temporal index not enabled"
    );
}

/// Test semantic_evolution_in() with explicit property name.
#[test]
fn test_semantic_evolution_in_explicit_property() {
    use aletheiadb::core::temporal::TimeRange;

    let db = AletheiaDB::new().unwrap();
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

    db.vector_index("content_embedding")
        .hnsw(hnsw_config.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        })
        .enable()
        .expect("Should enable temporal index");

    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let node_id = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("content_embedding", &v1)
                .build(),
        )
        .unwrap();

    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
    let result = db.semantic_evolution_in("content_embedding", node_id, time_range);

    assert!(result.is_ok(), "semantic_evolution_in should succeed");
}

/// Test semantic_evolution_in() with wrong property fails.
#[test]
fn test_semantic_evolution_in_wrong_property_fails() {
    use aletheiadb::core::temporal::TimeRange;

    let db = AletheiaDB::new().unwrap();
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

    db.vector_index("embedding")
        .hnsw(hnsw_config.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        })
        .enable()
        .expect("Should enable temporal index");

    let node_id = NodeId::new(1).unwrap();
    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

    let result = db.semantic_evolution_in("wrong_property", node_id, time_range);
    assert!(
        result.is_err(),
        "Should fail when property doesn't match temporal index"
    );
}

/// Test find_drift_in() with explicit property name.
#[test]
fn test_find_drift_in_explicit_property() {
    use aletheiadb::core::temporal::TimeRange;

    let db = AletheiaDB::new().unwrap();
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

    db.vector_index("content_embedding")
        .hnsw(hnsw_config.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        })
        .enable()
        .expect("Should enable temporal index");

    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let _node_id = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("content_embedding", &v1)
                .build(),
        )
        .unwrap();

    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
    let result = db.find_drift_in("content_embedding", 0.1, time_range, DriftMetric::Cosine);

    assert!(result.is_ok(), "find_drift_in should succeed");
}

/// Test find_drift_in() with wrong property fails.
#[test]
fn test_find_drift_in_wrong_property_fails() {
    use aletheiadb::core::temporal::TimeRange;

    let db = AletheiaDB::new().unwrap();
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

    db.vector_index("embedding")
        .hnsw(hnsw_config.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        })
        .enable()
        .expect("Should enable temporal index");

    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

    let result = db.find_drift_in("wrong_property", 0.1, time_range, DriftMetric::Cosine);
    assert!(
        result.is_err(),
        "Should fail when property doesn't match temporal index"
    );
}

// ========================================================================
// Multi-Property Temporal Index Tests (Issue #389 - Critical Fix)
// ========================================================================

/// Test that multiple temporal vector indexes can be enabled for different properties.
/// This is the critical test for multi-property temporal support.
#[test]
fn test_multi_property_temporal_indexes() {
    use aletheiadb::core::temporal::TimeRange;

    let db = AletheiaDB::new().unwrap();

    // Enable temporal index for FIRST property
    let hnsw_config1 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    db.vector_index("title_embedding")
        .hnsw(hnsw_config1.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config1),
        })
        .enable()
        .expect("Should enable first temporal index");

    // Enable temporal index for SECOND property - should NOT overwrite first!
    let hnsw_config2 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    db.vector_index("content_embedding")
        .hnsw(hnsw_config2.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config2),
        })
        .enable()
        .expect("Should enable second temporal index");

    // Create a node with both embeddings
    let title_emb = vec![1.0f32, 0.0, 0.0, 0.0];
    let content_emb = vec![0.0f32, 1.0, 0.0, 0.0];
    let node_id = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &title_emb)
                .insert_vector("content_embedding", &content_emb)
                .build(),
        )
        .unwrap();

    // Both temporal indexes should work independently
    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
    let query = vec![0.9f32, 0.1, 0.0, 0.0];

    // Query first property's temporal index
    let result1 = db.find_similar_as_of_in(
        "title_embedding",
        &query,
        10,
        aletheiadb::core::temporal::time::now(),
    );
    assert!(
        result1.is_ok(),
        "First temporal index should still work: {:?}",
        result1.err()
    );

    // Query second property's temporal index
    let result2 = db.find_similar_as_of_in(
        "content_embedding",
        &query,
        10,
        aletheiadb::core::temporal::time::now(),
    );
    assert!(
        result2.is_ok(),
        "Second temporal index should work: {:?}",
        result2.err()
    );

    // Both track_drift_in should work
    let reference = vec![1.0f32, 0.0, 0.0, 0.0];
    let drift1 = db.track_drift_in("title_embedding", node_id, &reference, time_range);
    assert!(
        drift1.is_ok(),
        "track_drift_in for first property should work"
    );

    let drift2 = db.track_drift_in("content_embedding", node_id, &reference, time_range);
    assert!(
        drift2.is_ok(),
        "track_drift_in for second property should work"
    );
}

/// Test that temporal queries on non-existent property fail gracefully.
#[test]
fn test_temporal_query_nonexistent_property() {
    let db = AletheiaDB::new().unwrap();

    // Enable temporal index for one property
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    db.vector_index("embedding")
        .hnsw(hnsw_config.clone())
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        })
        .enable()
        .expect("Should enable temporal index");

    // Query a property that has NO temporal index enabled
    let query = vec![1.0f32, 0.0, 0.0, 0.0];
    let result = db.find_similar_as_of_in(
        "nonexistent_property",
        &query,
        10,
        aletheiadb::core::temporal::time::now(),
    );

    assert!(
        result.is_err(),
        "Should fail for property without temporal index"
    );
}

// ==================== Code Review Fix Tests ====================

#[test]
fn test_temporal_config_without_hnsw_config() {
    // Test that TemporalVectorConfig can be created without hnsw_config
    // when a vector index already exists
    let db = AletheiaDB::new().unwrap();

    // Enable vector index first
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", hnsw_config).unwrap();

    // Now enable temporal vector index WITHOUT providing hnsw_config
    // This should succeed because vector index already exists
    let temporal_config = TemporalVectorConfig::default_temporal_only();
    let result = db.enable_temporal_vector_index("embedding", temporal_config);
    assert!(
        result.is_ok(),
        "Should succeed when vector index exists and hnsw_config is None"
    );
}

#[test]
fn test_temporal_config_without_hnsw_config_requires_existing_index() {
    // Test that TemporalVectorConfig without hnsw_config fails
    // when no vector index exists
    let db = AletheiaDB::new().unwrap();

    // Try to enable temporal vector index WITHOUT providing hnsw_config
    // AND without an existing vector index - this should fail
    let temporal_config = TemporalVectorConfig::default_temporal_only();
    let result = db.enable_temporal_vector_index("embedding", temporal_config);
    assert!(
        result.is_err(),
        "Should fail when no vector index exists and hnsw_config is None"
    );
}

#[test]
fn test_temporal_config_default_temporal_only() {
    // Test the default_temporal_only constructor
    let config = TemporalVectorConfig::default_temporal_only();

    assert!(config.hnsw_config.is_none(), "hnsw_config should be None");
    assert_eq!(
        config.snapshot_strategy,
        SnapshotStrategy::TransactionInterval(10)
    );
    assert_eq!(config.retention_policy, RetentionPolicy::KeepN(100));
    assert_eq!(config.max_snapshots, 100);
    assert_eq!(config.full_snapshot_interval, 10);
}

#[test]
fn test_list_temporal_vector_indexes() {
    // Test the list_temporal_vector_indexes method
    let db = AletheiaDB::new().unwrap();

    // Initially empty
    assert!(db.list_temporal_vector_indexes().is_empty());

    // Enable temporal index for first property
    let hnsw_config1 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    db.vector_index("embedding1")
        .hnsw(hnsw_config1)
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: None, // Will be resolved from builder
        })
        .enable()
        .expect("Should enable first temporal index");

    // Should have one index
    let indexes = db.list_temporal_vector_indexes();
    assert_eq!(indexes.len(), 1);
    assert!(indexes.contains(&"embedding1".to_string()));

    // Enable temporal index for second property
    let hnsw_config2 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    db.vector_index("embedding2")
        .hnsw(hnsw_config2)
        .temporal(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: None,
        })
        .enable()
        .expect("Should enable second temporal index");

    // Should have two indexes
    let indexes = db.list_temporal_vector_indexes();
    assert_eq!(indexes.len(), 2);
    assert!(indexes.contains(&"embedding1".to_string()));
    assert!(indexes.contains(&"embedding2".to_string()));
}

#[test]
fn test_concurrent_vector_operations_with_multiple_properties() {
    // Test concurrent operations across multiple vector properties
    let db = Arc::new(AletheiaDB::new().unwrap());

    // Enable two vector indexes for different properties
    let hnsw_config1 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    let hnsw_config2 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

    db.enable_vector_index("title_embedding", hnsw_config1)
        .unwrap();
    db.enable_vector_index("content_embedding", hnsw_config2)
        .unwrap();

    // Spawn multiple threads that create nodes with different embeddings concurrently
    let mut handles = vec![];
    for i in 0..10 {
        let db_clone = Arc::clone(&db);
        let handle = thread::spawn(move || {
            let base = (i as f32 + 1.0) / 10.0;
            let title_emb = vec![base, 0.0, 0.0, 0.0];
            let content_emb = vec![0.0, base, 0.0, 0.0];

            db_clone
                .create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert("id", i as i64)
                        .insert_vector("title_embedding", &title_emb)
                        .insert_vector("content_embedding", &content_emb)
                        .build(),
                )
                .unwrap()
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    let node_ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // Verify all nodes were created
    assert_eq!(db.node_count(), 10);

    // Verify both indexes are functioning
    // Search by title_embedding
    let title_query = vec![0.5f32, 0.0, 0.0, 0.0];
    let title_results = db
        .search_vectors_in("title_embedding", &title_query, 5)
        .unwrap();
    assert!(
        !title_results.is_empty(),
        "Should find results in title_embedding index"
    );

    // Search by content_embedding
    let content_query = vec![0.0f32, 0.5, 0.0, 0.0];
    let content_results = db
        .search_vectors_in("content_embedding", &content_query, 5)
        .unwrap();
    assert!(
        !content_results.is_empty(),
        "Should find results in content_embedding index"
    );

    // Verify both indexes have correct data (all nodes indexed in both)
    // Each node should be findable via both indexes
    let similar_by_title = db
        .search_vectors_in("title_embedding", &[0.5, 0.0, 0.0, 0.0], 10)
        .unwrap();
    let similar_by_content = db
        .search_vectors_in("content_embedding", &[0.0, 0.5, 0.0, 0.0], 10)
        .unwrap();

    // Both should have results from all nodes
    assert!(
        !similar_by_title.is_empty(),
        "Title index should have results"
    );
    assert!(
        !similar_by_content.is_empty(),
        "Content index should have results"
    );

    // Results should be distinct - title and content queries target different directions
    // So the top results from each should have different orderings
    let _ = node_ids; // Use node_ids to suppress unused warning
}

#[test]
fn test_aletheiadb_is_vector_index_enabled_for() {
    let db = AletheiaDB::new().unwrap();

    // Initially no index should be enabled
    assert!(!db.is_vector_index_enabled());
    assert!(!db.is_vector_index_enabled_for("embedding"));
    assert!(!db.is_vector_index_enabled_for("vector"));

    // Enable index for "embedding"
    let config = HnswConfig::new(128, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config).unwrap();

    // Now should be enabled
    assert!(db.is_vector_index_enabled());
    assert!(db.is_vector_index_enabled_for("embedding"));
    assert!(!db.is_vector_index_enabled_for("vector")); // Still false for other property

    // Enable another index
    let config2 = HnswConfig::new(256, DistanceMetric::Euclidean);
    db.enable_vector_index("vector", config2).unwrap();

    assert!(db.is_vector_index_enabled_for("vector"));
}

#[test]
fn test_max_vector_properties_limit() {
    // Test that the maximum number of vector properties is enforced
    use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
    use aletheiadb::storage::DEFAULT_MAX_VECTOR_PROPERTIES;

    let db = AletheiaDB::new().unwrap();

    // Enable indexes up to the limit
    for i in 0..DEFAULT_MAX_VECTOR_PROPERTIES {
        let config = HnswConfig::new(4, DistanceMetric::Cosine);
        let result = db
            .vector_index(&format!("property_{}", i))
            .hnsw(config)
            .enable();
        assert!(
            result.is_ok(),
            "Should be able to enable index {} (limit is {})",
            i,
            DEFAULT_MAX_VECTOR_PROPERTIES
        );
    }

    // Verify we have exactly the limit number of indexes
    let indexes = db.list_vector_indexes();
    assert_eq!(indexes.len(), DEFAULT_MAX_VECTOR_PROPERTIES);

    // Attempting to add one more should fail
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    let result = db.vector_index("one_too_many").hnsw(config).enable();
    assert!(
        result.is_err(),
        "Should not be able to exceed the maximum vector property limit"
    );

    // Verify the error message is helpful
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("Maximum") || err_msg.contains("maximum"),
        "Error message should mention the maximum limit"
    );
}
