use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::error::VectorError;
use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::db::AletheiaDB;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::query::hybrid::*;
use aletheiadb::query::traverse_and_rank;

/// Helper to create a test database with vector indexing enabled.
fn create_test_db() -> AletheiaDB {
    let db = AletheiaDB::new().unwrap();
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");
    db
}

/// Helper to create a simple social graph:
/// Alice -> Bob (embedding [0.9, 0.1, 0.0, 0.0] - similar to Alice)
/// Alice -> Carol (embedding [0.0, 1.0, 0.0, 0.0] - dissimilar to Alice)
/// Alice -> Dave (embedding [0.8, 0.2, 0.0, 0.0] - somewhat similar to Alice)
/// Returns (alice_id, bob_id, carol_id, dave_id)
fn create_social_graph(db: &AletheiaDB) -> (NodeId, NodeId, NodeId, NodeId) {
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Alice");

    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0, 0.0]) // Similar to Alice
                .build(),
        )
        .expect("Failed to create Bob");

    let carol = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Carol")
                .insert_vector("embedding", &[0.0f32, 1.0, 0.0, 0.0]) // Different
                .build(),
        )
        .expect("Failed to create Carol");

    let dave = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Dave")
                .insert_vector("embedding", &[0.8f32, 0.2, 0.0, 0.0]) // Somewhat similar
                .build(),
        )
        .expect("Failed to create Dave");

    // Create relationships: Alice -> Bob, Alice -> Carol, Alice -> Dave
    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create Alice->Bob edge");
    db.create_edge(alice, carol, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create Alice->Carol edge");
    db.create_edge(alice, dave, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create Alice->Dave edge");

    (alice, bob, carol, dave)
}

#[test]
fn test_traverse_and_rank_basic() {
    let db = create_test_db();
    let (alice, bob, carol, dave) = create_social_graph(&db);

    // Query: Find people Alice knows, ranked by similarity to Alice's embedding
    let alice_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results =
        traverse_and_rank(&db, alice, "KNOWS", &alice_embedding, 10).expect("Query failed");

    // Should return all 3 neighbors (Bob, Carol, Dave) ranked by similarity
    assert_eq!(results.len(), 3, "Should return all 3 neighbors");

    // Verify ordering: Bob (0.9,0.1,0,0) should be most similar, then Dave (0.8,0.2,0,0), then Carol (0,1,0,0)
    assert_eq!(results[0].0, bob, "Bob should be most similar");
    assert_eq!(results[1].0, dave, "Dave should be second most similar");
    assert_eq!(results[2].0, carol, "Carol should be least similar");

    // Verify similarity scores are in descending order
    assert!(
        results[0].1 > results[1].1,
        "Scores should be in descending order"
    );
    assert!(
        results[1].1 > results[2].1,
        "Scores should be in descending order"
    );

    // Verify similarity scores are in valid range [-1, 1]
    for (_, score) in &results {
        assert!(
            *score >= -1.0 && *score <= 1.0,
            "Cosine similarity should be in [-1, 1]"
        );
    }
}

#[test]
fn test_traverse_and_rank_respects_k_limit() {
    let db = create_test_db();
    let (alice, bob, _carol, _dave) = create_social_graph(&db);

    // Query with k=1 should only return top result
    let alice_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results =
        traverse_and_rank(&db, alice, "KNOWS", &alice_embedding, 1).expect("Query failed");

    assert_eq!(results.len(), 1, "Should respect k=1 limit");
    assert_eq!(results[0].0, bob, "Should return most similar (Bob)");
}

#[test]
fn test_traverse_and_rank_no_neighbors() {
    let db = create_test_db();

    // Create isolated node with no outgoing edges
    let isolated = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Isolated")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create isolated node");

    let query_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results = traverse_and_rank(&db, isolated, "KNOWS", &query_embedding, 10)
        .expect("Query should succeed");

    assert_eq!(
        results.len(),
        0,
        "Should return empty results for isolated node"
    );
}

#[test]
fn test_traverse_and_rank_node_not_found() {
    let db = create_test_db();

    let fake_id = NodeId::new(99999).expect("valid id");
    let query_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let result = traverse_and_rank(&db, fake_id, "KNOWS", &query_embedding, 10);

    assert!(
        result.is_err(),
        "Should return error for non-existent start node"
    );
    assert!(
        matches!(
            result.unwrap_err(),
            aletheiadb::core::error::Error::Storage(
                aletheiadb::core::error::StorageError::NodeNotFound(_)
            )
        ),
        "Should return NodeNotFound error"
    );
}

#[test]
fn test_traverse_and_rank_invalid_embedding() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Test with NaN
    let nan_embedding = [f32::NAN, 0.0, 0.0, 0.0];
    let result = traverse_and_rank(&db, alice, "KNOWS", &nan_embedding, 10);
    assert!(result.is_err(), "Should reject NaN embedding");
    assert!(
        matches!(
            result.unwrap_err(),
            aletheiadb::core::error::Error::Vector(VectorError::ContainsNaN { .. })
        ),
        "Should return ContainsNaN error"
    );

    // Test with Infinity
    let inf_embedding = [f32::INFINITY, 0.0, 0.0, 0.0];
    let result = traverse_and_rank(&db, alice, "KNOWS", &inf_embedding, 10);
    assert!(result.is_err(), "Should reject Infinity embedding");
    assert!(
        matches!(
            result.unwrap_err(),
            aletheiadb::core::error::Error::Vector(VectorError::ContainsInfinity { .. })
        ),
        "Should return ContainsInfinity error"
    );
}

#[test]
fn test_traverse_and_rank_handles_cycles() {
    let db = create_test_db();

    // Create a cycle: A -> B -> C -> A
    let node_a = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "A")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create A");

    let node_b = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "B")
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create B");

    let node_c = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "C")
                .insert_vector("embedding", &[0.8f32, 0.2, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create C");

    // Create cycle: A -> B -> C -> A
    db.create_edge(node_a, node_b, "NEXT", PropertyMapBuilder::new().build())
        .expect("Failed to create A->B edge");
    db.create_edge(node_b, node_c, "NEXT", PropertyMapBuilder::new().build())
        .expect("Failed to create B->C edge");
    db.create_edge(node_c, node_a, "NEXT", PropertyMapBuilder::new().build())
        .expect("Failed to create C->A edge");

    // Query from A - should only return B (direct neighbor), not revisit A through cycle
    let query_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results = traverse_and_rank(&db, node_a, "NEXT", &query_embedding, 10)
        .expect("Query should handle cycles");

    // Should only return immediate neighbor (B), not traverse the full cycle
    assert_eq!(
        results.len(),
        1,
        "Should return only direct neighbors, not cycle back"
    );
    assert_eq!(results[0].0, node_b, "Should return node B");
}

#[test]
fn test_traverse_and_rank_nodes_without_embeddings() {
    let db = create_test_db();

    // Create nodes: Alice has embedding, Bob has NO embedding, Carol has embedding
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Alice");

    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                // No embedding!
                .build(),
        )
        .expect("Failed to create Bob");

    let carol = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Carol")
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Carol");

    // Alice knows both Bob and Carol
    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create Alice->Bob edge");
    db.create_edge(alice, carol, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create Alice->Carol edge");

    // Query should gracefully skip Bob (no embedding) and return only Carol
    let query_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results = traverse_and_rank(&db, alice, "KNOWS", &query_embedding, 10)
        .expect("Query should handle missing embeddings");

    assert_eq!(
        results.len(),
        1,
        "Should skip node without embedding and return only Carol"
    );
    assert_eq!(results[0].0, carol, "Should return Carol (has embedding)");
}

#[test]
fn test_traverse_and_rank_respects_edge_label() {
    let db = create_test_db();

    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Alice");

    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Bob");

    let carol = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Carol")
                .insert_vector("embedding", &[0.8f32, 0.2, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Carol");

    // Alice KNOWS Bob, Alice WORKS_WITH Carol
    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create Alice->Bob KNOWS edge");
    db.create_edge(
        alice,
        carol,
        "WORKS_WITH",
        PropertyMapBuilder::new().build(),
    )
    .expect("Failed to create Alice->Carol WORKS_WITH edge");

    // Query only KNOWS edges - should only return Bob
    let query_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results =
        traverse_and_rank(&db, alice, "KNOWS", &query_embedding, 10).expect("Query failed");

    assert_eq!(
        results.len(),
        1,
        "Should only traverse KNOWS edges, not WORKS_WITH"
    );
    assert_eq!(results[0].0, bob, "Should return Bob (KNOWS edge)");
}

#[test]
fn test_traverse_and_rank_empty_database() {
    let db = create_test_db();

    // Create a single node with no edges
    let lonely = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Lonely")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create lonely node");

    let query_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results = traverse_and_rank(&db, lonely, "KNOWS", &query_embedding, 10)
        .expect("Query should succeed on node with no edges");

    assert_eq!(
        results.len(),
        0,
        "Should return empty results when node has no outgoing edges"
    );
}

#[test]
fn test_traverse_and_rank_self_loop() {
    let db = create_test_db();

    // Create a node with a self-loop
    let narcissist = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Narcissist")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create narcissist node");

    // Create self-loop
    db.create_edge(
        narcissist,
        narcissist,
        "LIKES",
        PropertyMapBuilder::new().build(),
    )
    .expect("Failed to create self-loop");

    // Query should include the self-loop result
    let query_embedding = [0.9f32, 0.1, 0.0, 0.0];
    let results = traverse_and_rank(&db, narcissist, "LIKES", &query_embedding, 10)
        .expect("Query should handle self-loop");

    assert_eq!(
        results.len(),
        1,
        "Should return self as neighbor (self-loop)"
    );
    assert_eq!(
        results[0].0, narcissist,
        "Should return self as result of self-loop"
    );
}
// Tests for find_similar_as_of

/// Helper to create a test database with temporal vector indexing enabled.
fn create_temporal_test_db() -> AletheiaDB {
    use aletheiadb::index::vector::temporal::{
        RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
    };

    let db = AletheiaDB::new().unwrap();
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
    let temporal_config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 5,
        hnsw_config: Some(hnsw_config),
    };
    db.enable_temporal_vector_index("embedding", temporal_config)
        .expect("Failed to enable temporal vector index");
    db
}

#[test]
fn test_find_similar_as_of_basic() {
    let db = create_temporal_test_db();

    // Create initial nodes
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Alice");

    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Bob");

    let carol = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Carol")
                .insert_vector("embedding", &[0.0f32, 1.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Carol");

    // Get current timestamp
    use aletheiadb::core::temporal::time;
    let timestamp = time::now();

    // Query: Find similar nodes to Alice's embedding
    let alice_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results =
        find_similar_as_of(&db, &alice_embedding, 10, timestamp).expect("Query should succeed");

    // Should return all 3 nodes
    assert_eq!(results.len(), 3, "Should return all nodes");

    // Verify ordering: Alice (exact match), Bob (similar), Carol (different)
    assert_eq!(results[0].0, alice, "Alice should be most similar");
    assert_eq!(results[1].0, bob, "Bob should be second most similar");
    assert_eq!(results[2].0, carol, "Carol should be least similar");

    // Verify similarity scores are in valid range and descending
    assert!(
        results[0].1 >= results[1].1,
        "Scores should be in descending order"
    );
    assert!(
        results[1].1 >= results[2].1,
        "Scores should be in descending order"
    );
}

#[test]
fn test_find_similar_as_of_respects_k_limit() {
    let db = create_temporal_test_db();

    // Create multiple nodes
    for i in 0..5 {
        let vector = [i as f32 / 5.0, 1.0 - i as f32 / 5.0, 0.0, 0.0];
        db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", format!("Person{}", i))
                .insert_vector("embedding", &vector)
                .build(),
        )
        .expect("Failed to create node");
    }

    use aletheiadb::core::temporal::time;
    let timestamp = time::now();

    // Query with k=2
    let query_embedding = [0.5f32, 0.5, 0.0, 0.0];
    let results =
        find_similar_as_of(&db, &query_embedding, 2, timestamp).expect("Query should succeed");

    assert_eq!(results.len(), 2, "Should respect k=2 limit");
}

#[test]
fn test_find_similar_as_of_temporal_consistency() {
    let db = create_temporal_test_db();

    // Create node with initial embedding
    let node = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Doc")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create node");

    use aletheiadb::core::temporal::time;
    let timestamp_before = time::now();

    // Wait a bit to ensure timestamp difference
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Update node with different embedding using write transaction
    db.write(|tx| {
        tx.update_node(
            node,
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.0f32, 1.0, 0.0, 0.0])
                .build(),
        )
    })
    .expect("Failed to update node");
    let timestamp_after = time::now();

    // Query at old timestamp - should find old embedding
    let old_query = [1.0f32, 0.0, 0.0, 0.0];
    let results_old = find_similar_as_of(&db, &old_query, 10, timestamp_before)
        .expect("Query at old timestamp should succeed");

    assert_eq!(
        results_old.len(),
        1,
        "Should find one node at old timestamp"
    );
    assert_eq!(results_old[0].0, node, "Should find the same node");
    assert!(
        results_old[0].1 > 0.99,
        "Old embedding should be very similar to old query"
    );

    // Query at new timestamp - should find new embedding
    let new_query = [0.0f32, 1.0, 0.0, 0.0];
    let results_new = find_similar_as_of(&db, &new_query, 10, timestamp_after)
        .expect("Query at new timestamp should succeed");

    assert_eq!(
        results_new.len(),
        1,
        "Should find one node at new timestamp"
    );
    assert_eq!(results_new[0].0, node, "Should find the same node");
    assert!(
        results_new[0].1 > 0.99,
        "New embedding should be very similar to new query"
    );
}

#[test]
fn test_find_similar_as_of_invalid_embedding() {
    let db = create_temporal_test_db();

    use aletheiadb::core::temporal::time;
    let timestamp = time::now();

    // Test with NaN
    let nan_embedding = [f32::NAN, 0.0, 0.0, 0.0];
    let result = find_similar_as_of(&db, &nan_embedding, 10, timestamp);
    assert!(result.is_err(), "Should reject NaN embedding");
    assert!(
        matches!(
            result.unwrap_err(),
            aletheiadb::core::error::Error::Vector(VectorError::ContainsNaN { .. })
        ),
        "Should return ContainsNaN error"
    );

    // Test with Infinity
    let inf_embedding = [f32::INFINITY, 0.0, 0.0, 0.0];
    let result = find_similar_as_of(&db, &inf_embedding, 10, timestamp);
    assert!(result.is_err(), "Should reject Infinity embedding");
    assert!(
        matches!(
            result.unwrap_err(),
            aletheiadb::core::error::Error::Vector(VectorError::ContainsInfinity { .. })
        ),
        "Should return ContainsInfinity error"
    );
}

#[test]
fn test_find_similar_as_of_no_temporal_index() {
    // Create DB without temporal index
    let db = create_test_db(); // Uses regular vector index only

    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
            .build(),
    )
    .expect("Failed to create node");

    use aletheiadb::core::temporal::time;
    let timestamp = time::now();

    let query_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let result = find_similar_as_of(&db, &query_embedding, 10, timestamp);

    assert!(
        result.is_err(),
        "Should return error when temporal index not enabled"
    );
    assert!(
        matches!(
            result.unwrap_err(),
            aletheiadb::core::error::Error::Vector(VectorError::IndexError(_))
        ),
        "Should return IndexError"
    );
}

#[test]
fn test_find_similar_as_of_empty_database() {
    let db = create_temporal_test_db();

    use aletheiadb::core::temporal::time;
    let timestamp = time::now();

    let query_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results = find_similar_as_of(&db, &query_embedding, 10, timestamp)
        .expect("Query on empty database should succeed");

    assert_eq!(
        results.len(),
        0,
        "Should return empty results for empty database"
    );
}
