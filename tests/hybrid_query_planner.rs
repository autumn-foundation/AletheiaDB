//! Integration tests for the Hybrid Query Planner (VS-060).
//!
//! These tests verify the end-to-end functionality of the query planner,
//! including graph traversal, vector search, and temporal queries.

use gallifreydb::{
    DistanceMetric, GallifreyDB, HnswConfig, NodeId, PropertyMapBuilder, WriteOps,
    query::{QueryBuilder, QueryPlanner},
    storage::version::AnchorConfig,
};
use std::sync::Arc;

/// Helper to create a test database with vector indexing enabled.
fn create_test_db() -> GallifreyDB {
    let db = GallifreyDB::new();
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");
    db
}

/// Helper to create a social graph for testing.
/// Returns (alice_id, bob_id, carol_id, dave_id)
fn create_social_graph(db: &GallifreyDB) -> (NodeId, NodeId, NodeId, NodeId) {
    // Create nodes with embeddings
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
                .insert_vector("embedding", &[0.8f32, 0.2, 0.0, 0.0]) // Somewhat similar to Alice
                .build(),
        )
        .expect("Failed to create Dave");

    // Create relationships: Alice -> Bob, Alice -> Carol, Bob -> Dave
    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create Alice->Bob edge");
    db.create_edge(alice, carol, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create Alice->Carol edge");
    db.create_edge(bob, dave, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create Bob->Dave edge");

    (alice, bob, carol, dave)
}

// =============================================================================
// Query Builder Tests
// =============================================================================

#[test]
fn test_query_builder_basic() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Build a simple node lookup query
    let query = QueryBuilder::new().start(alice).build();

    // Verify by planning - if planning succeeds, the query is valid
    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    assert!(planner.plan(query).is_ok());
    println!("✓ Query builder creates valid query");
}

#[test]
fn test_query_builder_with_filter() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Build query with filter
    let query = QueryBuilder::new()
        .start(alice)
        .filter(gallifreydb::query::Predicate::eq("name", "Alice"))
        .build();

    // Verify by planning
    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    assert!(planner.plan(query).is_ok());
    println!("✓ Query builder with filter works");
}

#[test]
fn test_query_builder_traverse() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Build traversal query
    let query = QueryBuilder::new().start(alice).traverse("KNOWS").build();

    // Verify by planning
    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    assert!(planner.plan(query).is_ok());
    println!("✓ Query builder with traversal works");
}

#[test]
fn test_query_builder_vector_search() {
    let _db = create_test_db();

    // Build vector search query
    let embedding = [1.0f32, 0.0, 0.0, 0.0];
    let query = QueryBuilder::new().find_similar(&embedding, 5).build();

    // Verify by planning
    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    assert!(planner.plan(query).is_ok());
    println!("✓ Query builder for vector search works");
}

#[test]
fn test_query_builder_hybrid() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Build hybrid query: traverse then rank by similarity
    let embedding = [1.0f32, 0.0, 0.0, 0.0];
    let query = QueryBuilder::new()
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .build();

    // Verify by planning
    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    assert!(planner.plan(query).is_ok());
    println!("✓ Query builder for hybrid query works");
}

#[test]
fn test_query_builder_temporal() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Build temporal query
    let query = QueryBuilder::new().as_of(1000, 1000).start(alice).build();

    assert!(query.is_temporal());
    println!("✓ Query builder with temporal context works");
}

// =============================================================================
// Query Planner Tests
// =============================================================================

#[test]
fn test_planner_node_lookup() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    let query = QueryBuilder::new().start(alice).build();

    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    let plan = planner.plan(query).expect("Planning failed");

    // Should produce a NodeLookup physical op
    assert!(matches!(
        plan.root,
        gallifreydb::query::planner::physical::PhysicalOp::NodeLookup { .. }
    ));
    println!("✓ Planner produces NodeLookup for start query");
}

#[test]
fn test_planner_traversal() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    let query = QueryBuilder::new().start(alice).traverse("KNOWS").build();

    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    let plan = planner.plan(query).expect("Planning failed");

    // Should produce an IndexedTraversal
    assert!(matches!(
        plan.root,
        gallifreydb::query::planner::physical::PhysicalOp::IndexedTraversal { .. }
    ));
    println!("✓ Planner produces IndexedTraversal for traverse query");
}

#[test]
fn test_planner_vector_search() {
    let _db = create_test_db();

    let embedding = [1.0f32, 0.0, 0.0, 0.0];
    let query = QueryBuilder::new().find_similar(&embedding, 5).build();

    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    let plan = planner.plan(query).expect("Planning failed");

    // Should produce an HnswSearch
    assert!(matches!(
        plan.root,
        gallifreydb::query::planner::physical::PhysicalOp::HnswSearch { .. }
    ));
    println!("✓ Planner produces HnswSearch for vector query");
}

#[test]
fn test_planner_hybrid_graph_vector() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    let embedding = [1.0f32, 0.0, 0.0, 0.0];
    let query = QueryBuilder::new()
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .build();

    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    let plan = planner.plan(query).expect("Planning failed");

    // Should produce VectorRerank(IndexedTraversal(NodeLookup))
    assert!(matches!(
        plan.root,
        gallifreydb::query::planner::physical::PhysicalOp::VectorRerank { .. }
    ));
    println!("✓ Planner produces VectorRerank for hybrid graph+vector query");
}

#[test]
fn test_planner_temporal_lookup() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    let query = QueryBuilder::new().as_of(1000, 1000).start(alice).build();

    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    let plan = planner.plan(query).expect("Planning failed");

    // Should produce TemporalNodeLookup
    assert!(matches!(
        plan.root,
        gallifreydb::query::planner::physical::PhysicalOp::TemporalNodeLookup { .. }
    ));
    println!("✓ Planner produces TemporalNodeLookup for temporal query");
}

// =============================================================================
// Convenience Method Tests
// =============================================================================

#[test]
fn test_db_query_method() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Use the db.query() convenience method
    let query = db.query().start(alice).traverse("KNOWS").build();

    assert!(query.operation_count() >= 2);
    println!("✓ db.query() convenience method works");
}

// =============================================================================
// Plan Explain Tests
// =============================================================================

#[test]
fn test_plan_explain() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    let embedding = [1.0f32, 0.0, 0.0, 0.0];
    let query = QueryBuilder::new()
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .limit(5)
        .build();

    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    let plan = planner.plan(query).expect("Planning failed");

    // Get explain output from the root operation
    let explain = plan.root.explain();
    assert!(!explain.is_empty());
    println!("Plan explanation:\n{}", explain);
    println!("✓ Plan explain works");
}

#[test]
fn test_plan_depth() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Simple query - shallow plan
    let simple = QueryBuilder::new().start(alice).build();
    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats.clone());
    let simple_plan = planner.plan(simple).expect("Planning failed");
    assert_eq!(simple_plan.root.depth(), 1);

    // Complex query - deeper plan
    let embedding = [1.0f32, 0.0, 0.0, 0.0];
    let complex = QueryBuilder::new()
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .build();
    let complex_plan = planner.plan(complex).expect("Planning failed");
    assert!(complex_plan.root.depth() >= 2);

    println!("✓ Plan depth calculation works");
}

// =============================================================================
// Edge Case Tests
// =============================================================================

#[test]
fn test_minimal_query() {
    let _db = create_test_db();

    // Create a minimal valid query - just looking up a non-existent node
    // The planner should still produce a valid plan
    let fake_id = NodeId::new(99999).expect("valid id");
    let query = QueryBuilder::new().start(fake_id).build();

    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);

    // Planning should succeed (execution would return empty results)
    let result = planner.plan(query);
    assert!(result.is_ok());
    println!("✓ Minimal query plans successfully");
}

#[test]
fn test_multi_hop_traversal_query() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Multi-hop traversal: Alice -> Bob -> Dave (2 hops)
    let query = QueryBuilder::new()
        .start(alice)
        .traverse_n("KNOWS", 2)
        .build();

    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    let plan = planner.plan(query).expect("Planning failed");

    assert!(matches!(
        plan.root,
        gallifreydb::query::planner::physical::PhysicalOp::IndexedTraversal { .. }
    ));
    println!("✓ Multi-hop traversal query works");
}

#[test]
fn test_scan_all_nodes() {
    let db = create_test_db();
    let (_alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Scan all nodes with label
    let query = QueryBuilder::new().scan(Some("Person")).build();

    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    let plan = planner.plan(query).expect("Planning failed");

    assert!(matches!(
        plan.root,
        gallifreydb::query::planner::physical::PhysicalOp::NodeScan { .. }
    ));
    println!("✓ Node scan query works");
}

// =============================================================================
// Temporal Integration Tests
// =============================================================================

#[test]
fn test_temporal_context_preserved() {
    let db = GallifreyDB::with_config(AnchorConfig {
        anchor_interval: 3,
        max_delta_chain: 10,
    });
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");

    // Create a node
    let node_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Original")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create node");

    // Record the timestamp after creation
    let create_time = gallifreydb::core::temporal::time::now();

    // Update the node
    db.write(|tx| {
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert("title", "Updated")
                .insert_vector("embedding", &[0.0f32, 1.0, 0.0, 0.0])
                .build(),
        )?;
        Ok(())
    })
    .expect("Failed to update node");

    // Build a temporal query for the original state
    let query = QueryBuilder::new()
        .as_of(create_time, create_time)
        .start(node_id)
        .build();

    // Verify temporal context is set (can only check via is_temporal())
    assert!(query.is_temporal());
    // Verify we have operations
    assert!(query.operation_count() >= 1);

    println!("✓ Temporal context is preserved in query");
}

// =============================================================================
// Full Hybrid Query Tests
// =============================================================================

#[test]
fn test_full_hybrid_temporal_graph_vector() {
    let db = GallifreyDB::with_config(AnchorConfig {
        anchor_interval: 3,
        max_delta_chain: 10,
    });
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");

    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Full hybrid: temporal + graph + vector
    let embedding = [1.0f32, 0.0, 0.0, 0.0];
    let timestamp = gallifreydb::core::temporal::time::now();

    let query = QueryBuilder::new()
        .as_of(timestamp, timestamp)
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .build();

    // Verify the query has all components:
    // - Temporal context (as_of)
    // - Node start + traverse + rank = at least 3 operations
    assert!(query.is_temporal());
    assert!(query.operation_count() >= 3);

    // Plan should work
    let stats = Arc::new(gallifreydb::query::planner::Statistics::default());
    let planner = QueryPlanner::new(stats);
    let plan = planner.plan(query).expect("Planning failed");

    // Should be a VectorRerank over something
    assert!(matches!(
        plan.root,
        gallifreydb::query::planner::physical::PhysicalOp::VectorRerank { .. }
    ));

    println!("✓ Full hybrid query (temporal + graph + vector) works");
}
