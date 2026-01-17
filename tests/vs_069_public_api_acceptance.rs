//! VS-069: Public API for Hybrid Queries - Acceptance Tests
//!
//! This test file validates all acceptance criteria for issue #82 (VS-069):
//! - [ ] Add `query()` method returning QueryBuilder
//! - [ ] Add shortcut methods for common patterns
//! - [ ] Update lib.rs re-exports
//! - [ ] Integration tests using public API
//! - [ ] Document API in rustdoc

use gallifreydb::{
    DistanceMetric,
    GallifreyDB,
    HnswConfig,
    NodeId,
    PropertyMapBuilder,
    // VS-069 Acceptance Criterion #3: Verify lib.rs re-exports work
    Query,
    QueryBuilder,
    QueryExecutor,
    QueryPlanner,
    QueryResults,
};

/// Helper to create a test database with vector indexing enabled.
fn create_test_db() -> GallifreyDB {
    let db = GallifreyDB::new().unwrap();
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");
    db
}

/// Helper to create a social graph for testing.
/// Returns (alice_id, bob_id, carol_id)
fn create_social_graph(db: &GallifreyDB) -> (NodeId, NodeId, NodeId) {
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

    // Create edges
    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create Alice-KNOWS->Bob edge");

    db.create_edge(alice, carol, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create Alice-KNOWS->Carol edge");

    (alice, bob, carol)
}

// ============================================================================
// VS-069 Acceptance Criterion #1: Add `query()` method returning QueryBuilder
// ============================================================================

#[test]
fn test_query_method_exists_and_returns_query_builder() {
    let db = create_test_db();
    let (alice, _bob, _carol) = create_social_graph(&db);

    // ACCEPTANCE: db.query() must exist and return a QueryBuilder
    let builder = db.query();

    // Verify we can build a minimal valid query
    let query = builder.start(alice).build();

    // Verify the query can be executed
    assert!(db.execute_query(query).is_ok());
}

#[test]
fn test_query_builder_fluent_api() {
    let db = create_test_db();
    let (alice, _bob, _carol) = create_social_graph(&db);

    // ACCEPTANCE: QueryBuilder must support fluent API for hybrid queries
    let query = db.query().start(alice).traverse("KNOWS").build();

    let results = db.execute_query(query).expect("Query should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    assert_eq!(rows.len(), 2, "Alice knows 2 people");
}

// ============================================================================
// VS-069 Acceptance Criterion #2: Add shortcut methods for common patterns
// ============================================================================

#[test]
fn test_shortcut_find_similar() {
    let db = create_test_db();
    let (alice, bob, _carol) = create_social_graph(&db);

    // ACCEPTANCE: find_similar() shortcut must exist and work
    let similar = db.find_similar(alice, 2).expect("find_similar should work");

    assert_eq!(similar.len(), 2, "Should find 2 similar nodes");
    assert_eq!(similar[0].0, bob, "Bob should be most similar to Alice");
    // Verify similarity score is reasonable (Bob's embedding [0.9, 0.1, 0, 0] vs Alice's [1, 0, 0, 0])
    assert!(
        similar[0].1 > 0.8,
        "Similarity should be high (>0.8), got {}",
        similar[0].1
    );
}

#[test]
fn test_shortcut_find_similar_with_label() {
    let db = create_test_db();
    let (alice, bob, _carol) = create_social_graph(&db);

    // ACCEPTANCE: find_similar_with_label() shortcut must exist
    let similar = db
        .find_similar_with_label(alice, "Person", 5)
        .expect("find_similar_with_label should work");

    assert_eq!(similar.len(), 2, "Should find 2 similar Person nodes");
    assert_eq!(
        similar[0].0, bob,
        "Bob should be most similar Person to Alice"
    );
}

#[test]
fn test_shortcut_find_similar_by_embedding() {
    let db = create_test_db();
    let (alice, bob, _carol) = create_social_graph(&db);

    // ACCEPTANCE: find_similar_by_embedding() shortcut must exist
    let embedding = [1.0f32, 0.0, 0.0, 0.0]; // Alice's embedding
    let similar = db
        .find_similar_by_embedding(&embedding, 2)
        .expect("find_similar_by_embedding should work");

    assert_eq!(similar.len(), 2, "Should find 2 nodes");
    assert_eq!(similar[0].0, alice, "Alice should be most similar");
    assert_eq!(similar[1].0, bob, "Bob should be second most similar");
}

#[test]
fn test_shortcut_traverse_and_rank() {
    let db = create_test_db();
    let (alice, bob, carol) = create_social_graph(&db);

    // ACCEPTANCE: traverse_and_rank() shortcut must exist
    let bob_embedding = [0.9f32, 0.1, 0.0, 0.0]; // Bob's embedding
    let results = db
        .traverse_and_rank(alice, "KNOWS", &bob_embedding, 10)
        .expect("traverse_and_rank should work");

    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");
    assert_eq!(rows.len(), 2, "Should find both of Alice's neighbors");
    let first = rows[0].entity.as_node().expect("Expected a node");
    let second = rows[1].entity.as_node().expect("Expected a node");
    assert_eq!(
        first.id, bob,
        "Bob should be ranked first (most similar to query embedding)"
    );
    assert_eq!(second.id, carol);
}

// ============================================================================
// VS-069 Acceptance Criterion #3: Update lib.rs re-exports
// ============================================================================

#[test]
fn test_lib_rs_reexports_query_types() {
    // ACCEPTANCE: These types must be re-exported from gallifreydb crate root
    // This test compiles only if the re-exports exist

    let _builder: Option<QueryBuilder<gallifreydb::query::builder::state::Initial>> = None;
    let _query: Option<Query> = None;
    let _executor: Option<QueryExecutor> = None;
    let _planner: Option<QueryPlanner> = None;
    let _results: Option<QueryResults> = None;

    // If this test compiles, the re-exports are working
}

// ============================================================================
// VS-069 Acceptance Criterion #4: Integration tests using public API
// ============================================================================

#[test]
fn test_public_api_graph_only_query() {
    let db = create_test_db();
    let (alice, _bob, _carol) = create_social_graph(&db);

    // ACCEPTANCE: Public API must support graph-only queries
    let query = db.query().start(alice).traverse("KNOWS").build();

    let results = db.execute_query(query).expect("Query should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    assert_eq!(rows.len(), 2, "Alice knows 2 people");
}

#[test]
fn test_public_api_vector_only_query() {
    let db = create_test_db();
    let (alice, bob, _carol) = create_social_graph(&db);

    // ACCEPTANCE: Public API must support vector-only queries
    let embedding = [1.0f32, 0.0, 0.0, 0.0]; // Alice's embedding
    let query = db.query().find_similar(&embedding, 2).build();

    let results = db.execute_query(query).expect("Query should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].entity.as_node().expect("Expected a node").id, alice);
    assert_eq!(rows[1].entity.as_node().expect("Expected a node").id, bob);
}

#[test]
fn test_public_api_hybrid_graph_vector_query() {
    let db = create_test_db();
    let (alice, bob, carol) = create_social_graph(&db);

    // ACCEPTANCE: Public API must support hybrid Graph+Vector queries
    let embedding = [0.9f32, 0.1, 0.0, 0.0]; // Bob's embedding
    let query = db
        .query()
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .build();

    let results = db.execute_query(query).expect("Query should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].entity.as_node().expect("Expected a node").id, bob);
    assert_eq!(rows[1].entity.as_node().expect("Expected a node").id, carol);
}

#[test]
fn test_public_api_temporal_graph_query() {
    let db = create_test_db();
    let (alice, _bob, _carol) = create_social_graph(&db);

    // ACCEPTANCE: Public API must support temporal queries
    let timestamp = gallifreydb::core::temporal::time::now();
    let query = db
        .query()
        .as_of(timestamp, timestamp)
        .start(alice)
        .traverse("KNOWS")
        .build();

    let results = db.execute_query(query).expect("Query should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    assert_eq!(rows.len(), 2, "Should find both of Alice's neighbors");
}

#[test]
fn test_public_api_full_hybrid_query() {
    let db = create_test_db();
    let (alice, bob, carol) = create_social_graph(&db);

    // ACCEPTANCE: Public API must support full hybrid (Temporal+Graph+Vector)
    let timestamp = gallifreydb::core::temporal::time::now();
    let embedding = [0.9f32, 0.1, 0.0, 0.0]; // Bob's embedding

    let query = db
        .query()
        .as_of(timestamp, timestamp)
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .build();

    let results = db.execute_query(query).expect("Query should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].entity.as_node().expect("Expected a node").id, bob);
    assert_eq!(rows[1].entity.as_node().expect("Expected a node").id, carol);
}

// ============================================================================
// VS-069 Acceptance Criterion #5: Document API in rustdoc
// ============================================================================

// NOTE: Rustdoc documentation is validated by:
// 1. Running `cargo doc --no-deps --open` manually
// 2. Checking that all public methods have /// doc comments
// 3. Verifying examples compile with `cargo test --doc`
//
// This test validates that key API patterns compile correctly.
//
// To verify rustdoc examples themselves, run: `cargo test --doc`

/// This test validates that key API patterns compile correctly
#[test]
fn test_rustdoc_examples_compile() {
    // If this test compiles, the basic API patterns work
    let db = create_test_db();
    let (alice, _bob, _carol) = create_social_graph(&db);

    // Example pattern from query() rustdoc
    let _query1 = db.query().start(alice).traverse("KNOWS").build();

    // Example pattern from traverse_and_rank() rustdoc
    let embedding = [0.9f32, 0.1, 0.0, 0.0];
    let _query2 = db
        .query()
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .build();

    // Example pattern from find_similar() rustdoc
    let _similar = db.find_similar(alice, 10);
}

// ============================================================================
// Comprehensive End-to-End Test
// ============================================================================

#[test]
fn test_vs_069_complete_acceptance() {
    // This test validates ALL acceptance criteria together

    let db = create_test_db();
    let (alice, bob, carol) = create_social_graph(&db);

    // 1. query() method exists ✓
    let builder = db.query();

    // 2. Shortcut methods exist ✓
    let _similar1 = db.find_similar(alice, 5).expect("find_similar works");
    let _similar2 = db
        .find_similar_with_label(alice, "Person", 5)
        .expect("find_similar_with_label works");

    let embedding = [0.9f32, 0.1, 0.0, 0.0];
    let _similar3 = db
        .find_similar_by_embedding(&embedding, 5)
        .expect("find_similar_by_embedding works");

    let _traverse_rank = db
        .traverse_and_rank(alice, "KNOWS", &embedding, 10)
        .expect("traverse_and_rank works");

    // 3. lib.rs re-exports work (imports at top of file) ✓

    // 4. Integration tests using public API ✓
    let timestamp = gallifreydb::core::temporal::time::now();
    let query = builder
        .as_of(timestamp, timestamp)
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .build();

    let results = db.execute_query(query).expect("Full hybrid query works");
    let rows: Vec<_> = results.collect_all().expect("Can collect results");

    assert_eq!(rows.len(), 2, "VS-069: All acceptance criteria validated");
    assert_eq!(rows[0].entity.as_node().expect("Expected a node").id, bob);
    assert_eq!(rows[1].entity.as_node().expect("Expected a node").id, carol);
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_find_similar_with_k_zero() {
    let db = create_test_db();
    let (alice, _bob, _carol) = create_social_graph(&db);

    // Edge case: k=0 should return empty results
    let similar = db.find_similar(alice, 0).expect("Should handle k=0");
    assert_eq!(similar.len(), 0, "k=0 should return no results");
}

#[test]
fn test_find_similar_nonexistent_node() {
    use gallifreydb::NodeId;

    let db = create_test_db();
    let _graph = create_social_graph(&db);

    // Edge case: Invalid node ID should error
    let invalid_id = NodeId::new(99999).expect("Should create NodeId");
    let result = db.find_similar(invalid_id, 10);

    assert!(result.is_err(), "Should error on non-existent node");
}

#[test]
fn test_find_similar_node_without_embedding() {
    let db = create_test_db();

    // Create a node without an embedding
    let no_embedding_node = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "NoEmbedding")
                .build(),
        )
        .expect("Should create node");

    // Edge case: Node without embedding should error
    let result = db.find_similar(no_embedding_node, 10);
    assert!(
        result.is_err(),
        "Should error when node lacks vector property"
    );
}

#[test]
fn test_find_similar_with_large_k() {
    let db = create_test_db();
    let (alice, _bob, _carol) = create_social_graph(&db);

    // Edge case: k > total nodes should return all nodes
    let similar = db
        .find_similar(alice, 1000)
        .expect("Should handle k > total nodes");

    // Should return all nodes with embeddings (alice, bob, carol = 3 total, but alice not included in results typically)
    assert!(
        similar.len() <= 1000,
        "Should not exceed requested k, got {}",
        similar.len()
    );
    assert!(similar.len() >= 2, "Should return at least bob and carol");
}

#[test]
fn test_traverse_and_rank_no_edges() {
    let db = create_test_db();

    // Create an isolated node with no edges
    let isolated = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Isolated")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Should create isolated node");

    let embedding = [0.9f32, 0.1, 0.0, 0.0];
    let results = db
        .traverse_and_rank(isolated, "KNOWS", &embedding, 10)
        .expect("Should handle nodes with no edges");

    let rows: Vec<_> = results.collect_all().expect("Should collect results");
    assert_eq!(rows.len(), 0, "Should return empty for isolated node");
}

#[test]
fn test_query_with_invalid_embedding_dimensions() {
    let db = create_test_db();
    let (_alice, _bob, _carol) = create_social_graph(&db);

    // Edge case: Wrong embedding dimensions
    let wrong_dims = [1.0f32, 0.0]; // Only 2 dimensions, expected 4
    let query = db.query().find_similar(&wrong_dims, 10).build();

    let result = db.execute_query(query);
    assert!(
        result.is_err(),
        "Should error on mismatched embedding dimensions"
    );
}
