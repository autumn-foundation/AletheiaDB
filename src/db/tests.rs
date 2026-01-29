use super::*;
use crate::api::transaction::{ReadOps, WriteOps};
use crate::core::property::PropertyMapBuilder;

#[test]
fn test_create_node() {
    let db = GallifreyDB::new().unwrap();

    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let node_id = db.create_node("Person", props).unwrap();

    assert_eq!(db.node_count(), 1);

    let node = db.get_node(node_id).unwrap();
    assert_eq!(node.id, node_id);
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
}

#[test]
fn test_create_edge() {
    let db = GallifreyDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let edge_id = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .unwrap();

    assert_eq!(db.edge_count(), 1);

    let edge = db.get_edge(edge_id).unwrap();
    assert_eq!(edge.source, alice);
    assert_eq!(edge.target, bob);
}

#[test]
fn test_time_travel_query() {
    let db = GallifreyDB::new().unwrap();

    // Create a node at time T1
    let props_v1 = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let node_id = db.create_node("Person", props_v1).unwrap();

    // Capture timestamp after creation (wallclock time)
    std::thread::sleep(std::time::Duration::from_micros(100));
    let t1 = crate::core::temporal::time::now();

    // In a real implementation, we'd create a second version here with an update_node method
    // For now, just verify we can query at T1

    // Query at time T1 (after node was created)
    let historical_node = db.get_node_at_time(node_id, t1, t1).unwrap();
    assert_eq!(
        historical_node.get_property("age").and_then(|v| v.as_int()),
        Some(30)
    );

    // Query current state
    let current_node = db.get_node(node_id).unwrap();
    assert_eq!(
        current_node.get_property("age").and_then(|v| v.as_int()),
        Some(30)
    );
}

#[test]
fn test_time_travel_after_deletion() {
    let db = GallifreyDB::new().unwrap();

    // Create a node
    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();
    let node_id = db.create_node("Person", props).unwrap();

    // Record timestamp after creation (wallclock time)
    std::thread::sleep(std::time::Duration::from_micros(100));
    let t_after_create = crate::core::temporal::time::now();

    // Delete the node
    std::thread::sleep(std::time::Duration::from_micros(100));
    db.write(|tx| {
        tx.delete_node(node_id)?;
        Ok(())
    })
    .unwrap();

    // Record timestamp after deletion (wallclock time)
    std::thread::sleep(std::time::Duration::from_micros(100));
    let t_after_delete = crate::core::temporal::time::now();

    // Query BEFORE creation - should fail (node didn't exist)
    // Note: We can't easily test this without more control over timestamps

    // Query AFTER deletion - should fail (node was deleted)
    // This is the critical test: time-travel query after deletion should NOT
    // return the deleted node's data
    let result = db.get_node_at_time(node_id, t_after_delete, t_after_delete);
    assert!(
        result.is_err(),
        "Expected NodeNotFound after deletion, but got: {:?}",
        result
    );

    // Query BEFORE deletion - should succeed (node existed)
    let result = db.get_node_at_time(node_id, t_after_create, t_after_create);
    assert!(
        result.is_ok(),
        "Expected to find node before deletion, but got: {:?}",
        result
    );
    let node = result.unwrap();
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
}

/// Test that verifies temporal index + historical storage visibility check interaction.
///
/// This is a critical integration test for Issue #194: the temporal index stores intervals
/// at insertion time, but deletions close the valid_time in historical storage. The query
/// path must verify visibility with historical storage to correctly reject deleted nodes.
#[test]
fn test_temporal_index_deletion_integration() {
    let db = GallifreyDB::new().unwrap();

    // Create a node
    let props = PropertyMapBuilder::new().insert("name", "TestNode").build();
    let node_id = db.create_node("TestLabel", props).unwrap();

    // Record timestamp after creation
    std::thread::sleep(std::time::Duration::from_micros(100));
    let t_after_create = crate::core::temporal::time::now();

    // Verify node is queryable after creation
    let result = db.get_node_at_time(node_id, t_after_create, t_after_create);
    assert!(
        result.is_ok(),
        "Node should be queryable after creation: {:?}",
        result
    );

    // Verify temporal index has the version
    let version_ids =
        db.temporal_indexes
            .find_node_version_at_point(node_id, t_after_create, t_after_create);
    assert!(
        !version_ids.is_empty(),
        "Temporal index should return candidates for existing node"
    );

    // Delete the node
    std::thread::sleep(std::time::Duration::from_micros(100));
    db.write(|tx| {
        tx.delete_node(node_id)?;
        Ok(())
    })
    .unwrap();

    // Record timestamp after deletion
    std::thread::sleep(std::time::Duration::from_micros(100));
    let t_after_delete = crate::core::temporal::time::now();

    // CRITICAL: Temporal index may still return the version (it stores insertion-time intervals)
    // but the query should correctly reject it via historical storage visibility check
    let version_ids_after =
        db.temporal_indexes
            .find_node_version_at_point(node_id, t_after_delete, t_after_delete);
    // Note: Temporal index might still return candidates - that's expected
    // The visibility check in get_node_at_time should filter them out

    // Query after deletion should fail
    let result = db.get_node_at_time(node_id, t_after_delete, t_after_delete);
    assert!(
        result.is_err(),
        "Query after deletion should fail. Temporal index returned {:?} candidates, \
         but historical visibility check should reject them. Got: {:?}",
        version_ids_after.len(),
        result
    );

    // Query at time BEFORE deletion should still work
    let result = db.get_node_at_time(node_id, t_after_create, t_after_create);
    assert!(
        result.is_ok(),
        "Query before deletion should succeed: {:?}",
        result
    );
}

#[test]
fn test_graph_traversal() {
    let db = GallifreyDB::new().unwrap();

    let n0 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    db.create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    let outgoing = db.get_outgoing_edges(n0);
    assert_eq!(outgoing.len(), 2);

    let knows_edges = db.get_outgoing_edges_with_label(n0, "KNOWS");
    assert_eq!(knows_edges.len(), 2);
}

#[test]
fn test_historical_stats() {
    let db = GallifreyDB::new().unwrap();

    db.create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let stats = db.historical_stats().unwrap();
    assert_eq!(stats.total_node_versions, 2);
    assert_eq!(stats.node_anchor_count, 2); // First versions are always anchors
}

// ==================== Transaction API Tests ====================

#[test]
fn test_closure_based_write_api() {
    let db = GallifreyDB::new().unwrap();

    // Use closure-based API for multiple operations
    let (node_id, edge_id) = db
        .write(|tx| {
            let n1 = tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )?;
            let n2 = tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )?;
            let e = tx.create_edge(
                n1,
                n2,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2024i64).build(),
            )?;
            Ok((n1, e))
        })
        .unwrap();

    // Verify changes are visible
    assert_eq!(db.node_count(), 2);
    assert_eq!(db.edge_count(), 1);

    let node = db.get_node(node_id).unwrap();
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Alice")
    );

    let edge = db.get_edge(edge_id).unwrap();
    assert_eq!(edge.source, node_id);
}

#[test]
fn test_closure_based_read_api() {
    let db = GallifreyDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Charlie").build(),
        )
        .unwrap();

    // Use closure-based read API
    let name = db
        .read(|tx| {
            let node = tx.get_node(node_id)?;
            Ok(node
                .get_property("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()))
        })
        .unwrap();

    assert_eq!(name, Some("Charlie".to_string()));
}

#[test]
fn test_explicit_write_transaction() {
    let db = GallifreyDB::new().unwrap();

    let mut tx = db.write_transaction().unwrap();
    let n1 = tx
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "David").build(),
        )
        .unwrap();
    let n2 = tx
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Eve").build(),
        )
        .unwrap();
    tx.create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Changes not visible before commit
    assert_eq!(db.node_count(), 0);

    // Commit
    tx.commit().unwrap();

    // Now visible
    assert_eq!(db.node_count(), 2);
    assert_eq!(db.edge_count(), 1);
}

#[test]
fn test_explicit_read_transaction() {
    let db = GallifreyDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("age", 42i64).build(),
        )
        .unwrap();

    let tx = db.read_transaction().unwrap();
    let node = tx.get_node(node_id).unwrap();
    assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(42));

    // Read transactions don't need commit
}

#[test]
fn test_transaction_atomicity() {
    let db = GallifreyDB::new().unwrap();

    // Create a valid node first
    let valid_node = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Try to create multiple operations, one of which will fail
    let result = db.write(|tx| {
        tx.create_node("Person", PropertyMapBuilder::new().build())?;
        tx.create_node("Person", PropertyMapBuilder::new().build())?;
        // This should fail validation (non-existent target)
        tx.create_edge(
            valid_node,
            NodeId::new(9999).unwrap(),
            "KNOWS",
            PropertyMapBuilder::new().build(),
        )?;
        Ok(())
    });

    // Transaction should fail
    assert!(result.is_err());

    // No partial changes should be visible (atomicity)
    // We started with 1 node, should still have 1 node
    assert_eq!(db.node_count(), 1);
    assert_eq!(db.edge_count(), 0);
}

#[test]
fn test_transaction_rollback_on_error() {
    let db = GallifreyDB::new().unwrap();

    // Closure returns an error - should auto-rollback
    let result: Result<()> = db.write(|tx| {
        tx.create_node("Person", PropertyMapBuilder::new().build())?;
        tx.create_node("Person", PropertyMapBuilder::new().build())?;
        // Manually return an error
        Err(crate::utils::error::Error::Storage(
            crate::utils::error::StorageError::InconsistentState {
                reason: "test error".to_string(),
            },
        ))
    });

    assert!(result.is_err());

    // All changes rolled back
    assert_eq!(db.node_count(), 0);
}

#[test]
fn test_multiple_transactions() {
    let db = GallifreyDB::new().unwrap();

    // Transaction 1
    let n1 = db
        .write(|tx| tx.create_node("Person", PropertyMapBuilder::new().build()))
        .unwrap();

    // Transaction 2
    let n2 = db
        .write(|tx| tx.create_node("Person", PropertyMapBuilder::new().build()))
        .unwrap();

    // Transaction 3
    db.write(|tx| tx.create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build()))
        .unwrap();

    assert_eq!(db.node_count(), 2);
    assert_eq!(db.edge_count(), 1);
}

#[test]
fn test_snapshot_isolation() {
    let db = GallifreyDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("version", 1i64).build(),
        )
        .unwrap();

    // Start a read transaction - captures snapshot
    let tx1 = db.read_transaction().unwrap();
    let node_v1 = tx1.get_node(node_id).unwrap();
    assert_eq!(
        node_v1.get_property("version").and_then(|v| v.as_int()),
        Some(1)
    );

    // Another write commits a change (creates a new node)
    let new_node_id = db
        .write(|tx| {
            tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("version", 2i64).build(),
            )
        })
        .unwrap();

    // Snapshot Isolation: tx1 should NOT see the new node
    // because it was created and committed after tx1's snapshot
    assert!(tx1.get_node(new_node_id).is_err());

    // Verify tx1 still sees the original node
    let node_v1_again = tx1.get_node(node_id).unwrap();
    assert_eq!(
        node_v1_again
            .get_property("version")
            .and_then(|v| v.as_int()),
        Some(1)
    );
}

// ==================== Vector Index API Tests ====================

#[test]
fn test_enable_vector_index() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

    // Enable vector index
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    // Trying to enable again should fail
    let config2 = HnswConfig::new(3, DistanceMetric::Cosine);
    assert!(db.enable_vector_index("embedding", config2).is_err());
}

#[test]
fn test_find_similar_basic() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::{DistanceMetric, HnswConfig};
    use std::sync::Arc;
    use std::thread;

    let db = Arc::new(GallifreyDB::new().unwrap());

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

// ==================== Batch Temporal Query Tests ====================

#[test]
fn test_get_nodes_at_time_basic() {
    let db = GallifreyDB::new().unwrap();

    // Create multiple nodes at time T1
    let props1 = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();
    let props2 = PropertyMapBuilder::new()
        .insert("name", "Bob")
        .insert("age", 25i64)
        .build();
    let props3 = PropertyMapBuilder::new()
        .insert("name", "Charlie")
        .insert("age", 35i64)
        .build();

    let node1 = db.create_node("Person", props1).unwrap();
    let node2 = db.create_node("Person", props2).unwrap();
    let node3 = db.create_node("Person", props3).unwrap();

    let t1 = *db.current_timestamp.lock().unwrap();

    // Query all three nodes at time T1 using batch API
    let node_ids = vec![node1, node2, node3];
    let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

    // Should return all three nodes
    assert_eq!(results.len(), 3);

    // Convert results to HashMap for easier verification
    let results_map: std::collections::HashMap<NodeId, Node> = results
        .into_iter()
        .map(|(id, node_opt)| (id, node_opt.expect("Node should exist")))
        .collect();

    assert_eq!(results_map.len(), 3);

    // Verify node1
    let n1 = results_map.get(&node1).unwrap();
    assert_eq!(n1.id, node1);
    assert_eq!(
        n1.get_property("name").and_then(|v| v.as_str()),
        Some("Alice")
    );

    // Verify node2
    let n2 = results_map.get(&node2).unwrap();
    assert_eq!(n2.id, node2);
    assert_eq!(
        n2.get_property("name").and_then(|v| v.as_str()),
        Some("Bob")
    );

    // Verify node3
    let n3 = results_map.get(&node3).unwrap();
    assert_eq!(n3.id, node3);
    assert_eq!(
        n3.get_property("name").and_then(|v| v.as_str()),
        Some("Charlie")
    );
}

#[test]
fn test_get_nodes_at_time_mixed_results() {
    let db = GallifreyDB::new().unwrap();

    // Create two nodes
    let node1 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    let node2 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();

    let t1 = *db.current_timestamp.lock().unwrap();

    // Query including a non-existent node
    let non_existent = NodeId::new(9999).unwrap();
    let node_ids = vec![node1, non_existent, node2];
    let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

    // Should return 3 results, with one being None
    assert_eq!(results.len(), 3);

    // First result should be Some
    assert!(results[0].1.is_some());
    assert_eq!(results[0].0, node1);

    // Second result should be None (non-existent node)
    assert!(results[1].1.is_none());
    assert_eq!(results[1].0, non_existent);

    // Third result should be Some
    assert!(results[2].1.is_some());
    assert_eq!(results[2].0, node2);
}

#[test]
fn test_get_nodes_at_time_empty_batch() {
    let db = GallifreyDB::new().unwrap();

    let t1 = *db.current_timestamp.lock().unwrap();

    // Query with empty node list
    let results = db.get_nodes_at_time(&[], t1, t1).unwrap();

    // Should return empty results
    assert_eq!(results.len(), 0);
}

#[test]
fn test_get_nodes_at_time_after_deletion() {
    let db = GallifreyDB::new().unwrap();

    // Create nodes
    let node1 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    let node2 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();

    let t_after_create = *db.current_timestamp.lock().unwrap();

    // Delete node1
    db.write(|tx| {
        tx.delete_node(node1)?;
        Ok(())
    })
    .unwrap();

    let t_after_delete = *db.current_timestamp.lock().unwrap();

    // Query at time after deletion - node1 should not be found
    let node_ids = vec![node1, node2];
    let results = db
        .get_nodes_at_time(&node_ids, t_after_delete, t_after_delete)
        .unwrap();

    assert_eq!(results.len(), 2);
    assert!(results[0].1.is_none()); // node1 was deleted
    assert!(results[1].1.is_some()); // node2 still exists

    // Query at time before deletion - both should exist
    let results = db
        .get_nodes_at_time(&node_ids, t_after_create, t_after_create)
        .unwrap();

    assert_eq!(results.len(), 2);
    assert!(results[0].1.is_some()); // node1 existed
    assert!(results[1].1.is_some()); // node2 existed
}

#[test]
fn test_get_edges_at_time_basic() {
    let db = GallifreyDB::new().unwrap();

    // Create nodes
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let charlie = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edges
    let edge1 = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .unwrap();
    let edge2 = db
        .create_edge(
            bob,
            charlie,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2021i64).build(),
        )
        .unwrap();
    let edge3 = db
        .create_edge(
            alice,
            charlie,
            "WORKS_WITH",
            PropertyMapBuilder::new().insert("since", 2022i64).build(),
        )
        .unwrap();

    let t1 = *db.current_timestamp.lock().unwrap();

    // Query all three edges at time T1 using batch API
    let edge_ids = vec![edge1, edge2, edge3];
    let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

    // Should return all three edges
    assert_eq!(results.len(), 3);

    // Convert results to HashMap for easier verification
    let results_map: std::collections::HashMap<EdgeId, Edge> = results
        .into_iter()
        .map(|(id, edge_opt)| (id, edge_opt.expect("Edge should exist")))
        .collect();

    assert_eq!(results_map.len(), 3);

    // Verify edge1
    let e1 = results_map.get(&edge1).unwrap();
    assert_eq!(e1.id, edge1);
    assert_eq!(e1.source, alice);
    assert_eq!(e1.target, bob);
    assert_eq!(
        e1.get_property("since").and_then(|v| v.as_int()),
        Some(2020)
    );

    // Verify edge2
    let e2 = results_map.get(&edge2).unwrap();
    assert_eq!(e2.id, edge2);
    assert_eq!(e2.source, bob);
    assert_eq!(e2.target, charlie);
    assert_eq!(
        e2.get_property("since").and_then(|v| v.as_int()),
        Some(2021)
    );

    // Verify edge3
    let e3 = results_map.get(&edge3).unwrap();
    assert_eq!(e3.id, edge3);
    assert_eq!(e3.source, alice);
    assert_eq!(e3.target, charlie);
    assert_eq!(
        e3.get_property("since").and_then(|v| v.as_int()),
        Some(2022)
    );
}

#[test]
fn test_get_edges_at_time_mixed_results() {
    let db = GallifreyDB::new().unwrap();

    // Create nodes and edges
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let edge1 = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .unwrap();

    let t1 = *db.current_timestamp.lock().unwrap();

    // Query including a non-existent edge
    let non_existent = EdgeId::new(9999).unwrap();
    let edge_ids = vec![edge1, non_existent];
    let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

    // Should return 2 results, with one being None
    assert_eq!(results.len(), 2);

    // First result should be Some
    assert!(results[0].1.is_some());
    assert_eq!(results[0].0, edge1);

    // Second result should be None (non-existent edge)
    assert!(results[1].1.is_none());
    assert_eq!(results[1].0, non_existent);
}

#[test]
fn test_get_edges_at_time_empty_batch() {
    let db = GallifreyDB::new().unwrap();

    let t1 = *db.current_timestamp.lock().unwrap();

    // Query with empty edge list
    let results = db.get_edges_at_time(&[], t1, t1).unwrap();

    // Should return empty results
    assert_eq!(results.len(), 0);
}

#[test]
fn test_get_edges_at_time_after_deletion() {
    let db = GallifreyDB::new().unwrap();

    // Create nodes and edges
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let edge1 = db
        .create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    let edge2 = db
        .create_edge(bob, alice, "WORKS_WITH", PropertyMapBuilder::new().build())
        .unwrap();

    let t_after_create = *db.current_timestamp.lock().unwrap();

    // Delete edge1
    db.write(|tx| {
        tx.delete_edge(edge1)?;
        Ok(())
    })
    .unwrap();

    let t_after_delete = *db.current_timestamp.lock().unwrap();

    // Query at time after deletion - edge1 should not be found
    let edge_ids = vec![edge1, edge2];
    let results = db
        .get_edges_at_time(&edge_ids, t_after_delete, t_after_delete)
        .unwrap();

    assert_eq!(results.len(), 2);
    assert!(results[0].1.is_none()); // edge1 was deleted
    assert!(results[1].1.is_some()); // edge2 still exists

    // Query at time before deletion - both should exist
    let results = db
        .get_edges_at_time(&edge_ids, t_after_create, t_after_create)
        .unwrap();

    assert_eq!(results.len(), 2);
    assert!(results[0].1.is_some()); // edge1 existed
    assert!(results[1].1.is_some()); // edge2 existed
}

#[test]
fn test_get_nodes_at_time_large_batch() {
    let db = GallifreyDB::new().unwrap();

    // Create 100 nodes
    let node_ids: Vec<_> = (0..100)
        .map(|i| {
            db.create_node(
                "Test",
                PropertyMapBuilder::new().insert("index", i as i64).build(),
            )
            .unwrap()
        })
        .collect();

    let t1 = *db.current_timestamp.lock().unwrap();

    // Query all 100 at once
    let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

    // All should exist
    assert_eq!(results.len(), 100);
    assert!(results.iter().all(|(_, node)| node.is_some()));

    // Verify order is preserved
    for (i, (id, _)) in results.iter().enumerate() {
        assert_eq!(*id, node_ids[i]);
    }
}

#[test]
fn test_get_nodes_at_time_duplicate_ids() {
    let db = GallifreyDB::new().unwrap();

    let node1 = db
        .create_node(
            "Test",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();

    let t1 = *db.current_timestamp.lock().unwrap();

    // Query with duplicates
    let node_ids = vec![node1, node1, node1];
    let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

    // Should return 3 results (one per input, even if duplicate)
    assert_eq!(results.len(), 3);
    assert!(
        results
            .iter()
            .all(|(id, node)| { *id == node1 && node.is_some() })
    );
}

#[test]
fn test_get_edges_at_time_large_batch() {
    let db = GallifreyDB::new().unwrap();

    // Create nodes
    let source = db
        .create_node("Node", PropertyMapBuilder::new().build())
        .unwrap();
    let target = db
        .create_node("Node", PropertyMapBuilder::new().build())
        .unwrap();

    // Create 100 edges
    let edge_ids: Vec<_> = (0..100)
        .map(|i| {
            db.create_edge(
                source,
                target,
                "LINK",
                PropertyMapBuilder::new().insert("index", i as i64).build(),
            )
            .unwrap()
        })
        .collect();

    let t1 = *db.current_timestamp.lock().unwrap();

    // Query all 100 at once
    let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

    // All should exist
    assert_eq!(results.len(), 100);
    assert!(results.iter().all(|(_, edge)| edge.is_some()));

    // Verify order is preserved
    for (i, (id, _)) in results.iter().enumerate() {
        assert_eq!(*id, edge_ids[i]);
    }
}

#[test]
fn test_get_edges_at_time_duplicate_ids() {
    let db = GallifreyDB::new().unwrap();

    let source = db
        .create_node("Node", PropertyMapBuilder::new().build())
        .unwrap();
    let target = db
        .create_node("Node", PropertyMapBuilder::new().build())
        .unwrap();

    let edge1 = db
        .create_edge(source, target, "LINK", PropertyMapBuilder::new().build())
        .unwrap();

    let t1 = *db.current_timestamp.lock().unwrap();

    // Query with duplicates
    let edge_ids = vec![edge1, edge1, edge1];
    let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

    // Should return 3 results (one per input, even if duplicate)
    assert_eq!(results.len(), 3);
    assert!(
        results
            .iter()
            .all(|(id, edge)| { *id == edge1 && edge.is_some() })
    );
}

#[test]
fn test_find_similar_with_missing_property() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::DistanceMetric;

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::DistanceMetric;

    let db = GallifreyDB::new().unwrap();

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
    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};

    let db = GallifreyDB::new().unwrap();
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
    use crate::index::vector::DistanceMetric;

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::DistanceMetric;

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::DistanceMetric;

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::DistanceMetric;

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::DistanceMetric;

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};

    let db = GallifreyDB::new().unwrap();
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
    let timestamp = crate::core::temporal::time::now();

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
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};

    let db = GallifreyDB::new().unwrap();
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

    let timestamp = crate::core::temporal::time::now();
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
    use crate::index::vector::DistanceMetric;

    let db = GallifreyDB::new().unwrap();

    // Only enable regular HNSW index, not temporal
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
        .enable()
        .unwrap();

    let timestamp = crate::core::temporal::time::now();
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
    use crate::core::temporal::TimeRange;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};

    let db = GallifreyDB::new().unwrap();
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
    use crate::core::temporal::TimeRange;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};

    let db = GallifreyDB::new().unwrap();
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
    use crate::core::temporal::TimeRange;
    use crate::index::vector::DistanceMetric;

    let db = GallifreyDB::new().unwrap();

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
    use crate::core::temporal::TimeRange;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};

    let db = GallifreyDB::new().unwrap();
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
    use crate::core::temporal::TimeRange;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};

    let db = GallifreyDB::new().unwrap();
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
    use crate::core::temporal::TimeRange;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::temporal::{
        DriftMetric, RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
    };

    let db = GallifreyDB::new().unwrap();
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
    use crate::core::temporal::TimeRange;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::temporal::{
        DriftMetric, RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
    };

    let db = GallifreyDB::new().unwrap();
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
    use crate::core::temporal::TimeRange;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};

    let db = GallifreyDB::new().unwrap();

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
        crate::core::temporal::time::now(),
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
        crate::core::temporal::time::now(),
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
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};

    let db = GallifreyDB::new().unwrap();

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
        crate::core::temporal::time::now(),
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
    use crate::index::vector::temporal::TemporalVectorConfig;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::temporal::TemporalVectorConfig;

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};

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
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
    use crate::index::vector::{DistanceMetric, HnswConfig};
    use std::sync::Arc;
    use std::thread;

    let db = Arc::new(GallifreyDB::new().unwrap());

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

// ==================== Constructor Error Handling Tests ====================
// These tests verify that database constructors return Result and properly
// propagate WAL creation errors (Issue #343)

#[test]
fn test_new_returns_result() {
    // GallifreyDB::new() should return Result<Self> and succeed with default config
    let result = GallifreyDB::new();
    assert!(result.is_ok(), "new() should succeed with default config");
}

#[test]
fn test_with_config_returns_result() {
    // GallifreyDB::with_config() should return Result<Self>
    let result = GallifreyDB::with_config(crate::storage::version::AnchorConfig::default());
    assert!(
        result.is_ok(),
        "with_config() should succeed with default config"
    );
}

#[test]
fn test_with_wal_config_returns_result() {
    // GallifreyDB::with_wal_config() should return Result<Self>
    let wal_config = crate::config::WalConfig::default();
    let result = GallifreyDB::with_wal_config(wal_config);
    assert!(
        result.is_ok(),
        "with_wal_config() should succeed with default config"
    );
}

#[test]
fn test_with_full_config_returns_result() {
    // GallifreyDB::with_full_config() should return Result<Self>
    let result = GallifreyDB::with_full_config(
        crate::storage::version::AnchorConfig::default(),
        crate::config::WalConfig::default(),
    );
    assert!(
        result.is_ok(),
        "with_full_config() should succeed with default config"
    );
}

#[test]
fn test_with_unified_config_returns_result() {
    // GallifreyDB::with_unified_config() should return Result<Self>
    let config = crate::config::GallifreyDBConfig::default();
    let result = GallifreyDB::with_unified_config(config);
    assert!(
        result.is_ok(),
        "with_unified_config() should succeed with default config"
    );
}

#[cfg(unix)]
#[test]
fn test_wal_creation_failure_propagates_error() {
    // When WAL creation fails, the error should be propagated instead of panicking
    use std::path::PathBuf;

    // Use /dev/null/wal - /dev/null is a character device, not a directory,
    // so any attempt to create subdirectories under it will fail
    let invalid_wal_dir = PathBuf::from("/dev/null/wal");

    let wal_config = crate::config::WalConfigBuilder::new()
        .wal_dir(invalid_wal_dir)
        .build();

    let result = GallifreyDB::with_wal_config(wal_config);

    // Should return Err instead of panicking
    assert!(
        result.is_err(),
        "with_wal_config() should return Err when WAL directory cannot be created"
    );

    // Error should mention an I/O issue
    let err = result.err().expect("Expected an error");
    let err_msg = err.to_string().to_lowercase();
    assert!(
        err_msg.contains("i/o")
            || err_msg.contains("directory")
            || err_msg.contains("not a directory"),
        "Error message should indicate I/O issue, got: {}",
        err
    );
}

#[cfg(unix)]
#[test]
fn test_unified_config_wal_failure_propagates_error() {
    // When WAL creation fails in with_unified_config, the error should be propagated
    use std::path::PathBuf;

    // Use /dev/null/wal - /dev/null is a character device, not a directory,
    // so any attempt to create subdirectories under it will fail
    let invalid_wal_dir = PathBuf::from("/dev/null/wal");

    let config = crate::config::GallifreyDBConfigBuilder::new()
        .wal(
            crate::config::WalConfigBuilder::new()
                .wal_dir(invalid_wal_dir)
                .build(),
        )
        .build();

    let result = GallifreyDB::with_unified_config(config);

    // Should return Err instead of panicking
    assert!(
        result.is_err(),
        "with_unified_config() should return Err when WAL directory cannot be created"
    );
}

#[test]
fn test_max_vector_properties_limit() {
    // Test that the maximum number of vector properties is enforced
    use crate::index::vector::{DistanceMetric, HnswConfig};
    use crate::storage::current::DEFAULT_MAX_VECTOR_PROPERTIES;

    let db = crate::GallifreyDB::new().unwrap();

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

// ========================================================================
// Phase 3: Simple Accessor and Getter Tests
// ========================================================================

#[test]
fn test_gallifreydb_is_vector_index_enabled_for() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

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
fn test_gallifreydb_default_durability() {
    let db = GallifreyDB::new().unwrap();

    // Default durability should exist and be valid
    let _durability = db.default_durability();
    // Just verify we can call it without error
    // If it panicked, the test would fail
}

#[test]
fn test_get_edge_source_and_target() {
    let db = GallifreyDB::new().unwrap();

    // Create nodes
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();

    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();

    // Create edge from alice to bob
    let knows_edge = db
        .create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Verify get_edge_source and get_edge_target
    assert_eq!(db.get_edge_source(knows_edge).unwrap(), alice);
    assert_eq!(db.get_edge_target(knows_edge).unwrap(), bob);
}
