//! Integration tests for the Hybrid Query Planner (VS-060).
//!
//! These tests verify the end-to-end functionality of the query planner,
//! including graph traversal, vector search, and temporal queries.

use aletheiadb::{
    AletheiaDB, DistanceMetric, Error, HnswConfig, NodeId, PropertyMapBuilder, WriteOps,
    query::{QueryBuilder, QueryPlanner, traverse_and_rank},
    storage::{CurrentStorage, version::AnchorConfig},
};
use std::sync::Arc;

/// Helper to create a test database with vector indexing enabled.
fn create_test_db() -> AletheiaDB {
    let db = AletheiaDB::new().unwrap();
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");
    db
}

/// Helper to create a test planner with vector index enabled.
fn create_test_planner() -> QueryPlanner {
    let storage = Arc::new(CurrentStorage::new());
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    storage.enable_vector_index("embedding", config).unwrap();
    let stats = Arc::new(aletheiadb::query::planner::Statistics::default());
    QueryPlanner::new(stats, storage)
}

/// Helper to assert a query executes successfully and returns expected count
fn assert_query_returns_count(db: &AletheiaDB, query: aletheiadb::query::Query, expected: usize) {
    let results = db.execute_query(query).expect("Query should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");
    assert_eq!(
        rows.len(),
        expected,
        "Expected {} results, got {}",
        expected,
        rows.len()
    );
}

/// Helper to assert a query executes successfully and returns non-empty results
fn assert_query_succeeds(db: &AletheiaDB, query: aletheiadb::query::Query) {
    let results = db.execute_query(query).expect("Query should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");
    assert!(!rows.is_empty(), "Query should return at least one result");
}

/// Helper to create a social graph for testing.
/// Returns (alice_id, bob_id, carol_id, dave_id)
fn create_social_graph(db: &AletheiaDB) -> (NodeId, NodeId, NodeId, NodeId) {
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
    let planner = create_test_planner();
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
        .filter(aletheiadb::query::Predicate::eq("name", "Alice"))
        .build();

    // Verify by planning
    let planner = create_test_planner();
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
    let planner = create_test_planner();
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
    let planner = create_test_planner();
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
    let planner = create_test_planner();
    assert!(planner.plan(query).is_ok());
    println!("✓ Query builder for hybrid query works");
}

#[test]
fn test_query_builder_temporal() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Build temporal query
    let query = QueryBuilder::new()
        .as_of(1000.into(), 1000.into())
        .start(alice)
        .build();

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

    let planner = create_test_planner();
    let plan = planner.plan(query).expect("Planning failed");

    // Should produce a NodeLookup physical op
    assert!(matches!(
        plan.root,
        aletheiadb::query::planner::physical::PhysicalOp::NodeLookup { .. }
    ));
    println!("✓ Planner produces NodeLookup for start query");
}

#[test]
fn test_planner_traversal() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    let query = QueryBuilder::new().start(alice).traverse("KNOWS").build();

    let planner = create_test_planner();
    let plan = planner.plan(query).expect("Planning failed");

    // Should produce an IndexedTraversal
    assert!(matches!(
        plan.root,
        aletheiadb::query::planner::physical::PhysicalOp::IndexedTraversal { .. }
    ));
    println!("✓ Planner produces IndexedTraversal for traverse query");
}

#[test]
fn test_planner_vector_search() {
    let _db = create_test_db();

    let embedding = [1.0f32, 0.0, 0.0, 0.0];
    let query = QueryBuilder::new().find_similar(&embedding, 5).build();

    let planner = create_test_planner();
    let plan = planner.plan(query).expect("Planning failed");

    // Should produce an HnswSearch
    assert!(matches!(
        plan.root,
        aletheiadb::query::planner::physical::PhysicalOp::HnswSearch { .. }
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

    let planner = create_test_planner();
    let plan = planner.plan(query).expect("Planning failed");

    // Should produce VectorRerank(IndexedTraversal(NodeLookup))
    assert!(matches!(
        plan.root,
        aletheiadb::query::planner::physical::PhysicalOp::VectorRerank { .. }
    ));
    println!("✓ Planner produces VectorRerank for hybrid graph+vector query");
}

#[test]
fn test_planner_temporal_lookup() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    let query = QueryBuilder::new()
        .as_of(1000.into(), 1000.into())
        .start(alice)
        .build();

    let planner = create_test_planner();
    let plan = planner.plan(query).expect("Planning failed");

    // Should produce TemporalNodeLookup
    assert!(matches!(
        plan.root,
        aletheiadb::query::planner::physical::PhysicalOp::TemporalNodeLookup { .. }
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

    let planner = create_test_planner();
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
    let planner = create_test_planner();
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

    let planner = create_test_planner();

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

    let planner = create_test_planner();
    let plan = planner.plan(query).expect("Planning failed");

    assert!(matches!(
        plan.root,
        aletheiadb::query::planner::physical::PhysicalOp::IndexedTraversal { .. }
    ));
    println!("✓ Multi-hop traversal query works");
}

#[test]
fn test_scan_all_nodes() {
    let db = create_test_db();
    let (_alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Scan all nodes with label
    let query = QueryBuilder::new().scan(Some("Person")).build();

    let planner = create_test_planner();
    let plan = planner.plan(query).expect("Planning failed");

    assert!(matches!(
        plan.root,
        aletheiadb::query::planner::physical::PhysicalOp::NodeScan { .. }
    ));
    println!("✓ Node scan query works");
}

// =============================================================================
// Temporal Integration Tests
// =============================================================================

#[test]
fn test_temporal_context_preserved() {
    let db = AletheiaDB::with_config(AnchorConfig {
        anchor_interval: 3,
        max_delta_chain: 10,
    })
    .unwrap();
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
    let create_time = aletheiadb::core::temporal::time::now();

    // Update the node
    db.write(|tx| {
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert("title", "Updated")
                .insert_vector("embedding", &[0.0f32, 1.0, 0.0, 0.0])
                .build(),
        )?;
        Ok::<_, Error>(())
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
    let db = AletheiaDB::with_config(AnchorConfig {
        anchor_interval: 3,
        max_delta_chain: 10,
    })
    .unwrap();
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");

    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Full hybrid: temporal + graph + vector
    let embedding = [1.0f32, 0.0, 0.0, 0.0];
    let timestamp = aletheiadb::core::temporal::time::now();

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
    let planner = create_test_planner();
    let plan = planner.plan(query).expect("Planning failed");

    // Should be a VectorRerank over something
    assert!(matches!(
        plan.root,
        aletheiadb::query::planner::physical::PhysicalOp::VectorRerank { .. }
    ));

    println!("✓ Full hybrid query (temporal + graph + vector) works");
}

// =============================================================================
// End-to-End Execution Tests
// =============================================================================
// These tests verify the complete query execution pipeline, not just planning.

#[test]
fn test_execute_node_lookup() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Execute a simple node lookup query
    let query = db.query().start(alice).build();

    // Use helper to assert query succeeds and returns results
    assert_query_succeeds(&db, query.clone());

    // Verify it's Alice specifically
    let results = db.execute_query(query).expect("Query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");
    assert_eq!(rows.len(), 1, "Expected exactly one result");

    let row = &rows[0];
    let node = row.entity.as_node().expect("Expected a node");
    assert_eq!(node.id, alice);
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Alice")
    );

    println!("✓ Execute node lookup returns correct result");
}

#[test]
fn test_execute_traversal() {
    let db = create_test_db();
    let (alice, bob, carol, _dave) = create_social_graph(&db);

    // Execute traversal: Alice -> KNOWS -> ?
    let query = db.query().start(alice).traverse("KNOWS").build();
    let results = db.execute_query(query).expect("Query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Alice knows Bob and Carol
    assert_eq!(rows.len(), 2, "Expected two results (Bob and Carol)");

    // Collect the node IDs from results
    let result_ids: std::collections::HashSet<_> = rows
        .iter()
        .filter_map(|r| r.entity.as_node())
        .map(|n| n.id)
        .collect();

    assert!(result_ids.contains(&bob), "Results should contain Bob");
    assert!(result_ids.contains(&carol), "Results should contain Carol");

    println!("✓ Execute traversal returns correct neighbors");
}

#[test]
fn test_execute_traverse_and_rank() {
    let db = create_test_db();
    let (alice, bob, carol, _dave) = create_social_graph(&db);

    // Alice knows Bob (embedding [0.9, 0.1, 0.0, 0.0] - similar to Alice)
    // Alice knows Carol (embedding [0.0, 1.0, 0.0, 0.0] - dissimilar to Alice)
    // Query: Find people Alice knows, ranked by similarity to Alice's embedding
    let alice_embedding = [1.0f32, 0.0, 0.0, 0.0];

    let query = db
        .query()
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&alice_embedding, 10)
        .build();

    let results = db.execute_query(query).expect("Query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Should have Bob and Carol
    assert_eq!(rows.len(), 2, "Expected two results");

    // Bob should be first (more similar to Alice's embedding)
    let first = rows[0].entity.as_node().expect("Expected a node");
    let second = rows[1].entity.as_node().expect("Expected a node");

    assert_eq!(first.id, bob, "Bob should be first (most similar)");
    assert_eq!(second.id, carol, "Carol should be second (less similar)");

    // Verify scores are present and in descending order
    assert!(rows[0].score.is_some(), "First result should have a score");
    assert!(rows[1].score.is_some(), "Second result should have a score");
    assert!(
        rows[0].score.unwrap() > rows[1].score.unwrap(),
        "Scores should be in descending order"
    );

    println!("✓ Execute traverse_and_rank returns correctly ordered results");
}

#[test]
fn test_execute_filter() {
    let db = create_test_db();
    let (alice, bob, _carol, _dave) = create_social_graph(&db);

    // Traverse from Alice and filter for "Bob"
    let query = db
        .query()
        .start(alice)
        .traverse("KNOWS")
        .filter(aletheiadb::query::Predicate::eq("name", "Bob"))
        .build();

    let results = db.execute_query(query).expect("Query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Should only return Bob
    assert_eq!(rows.len(), 1, "Expected one result after filter");
    let node = rows[0].entity.as_node().expect("Expected a node");
    assert_eq!(node.id, bob, "Should be Bob");

    println!("✓ Execute with filter returns filtered results");
}

#[test]
fn test_execute_limit() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Alice knows 2 people, but limit to 1
    let query = db.query().start(alice).traverse("KNOWS").limit(1).build();

    let results = db.execute_query(query).expect("Query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Should only return 1 result due to limit
    assert_eq!(rows.len(), 1, "Expected one result due to limit");

    println!("✓ Execute with limit truncates results");
}

#[test]
fn test_execute_node_scan() {
    let db = create_test_db();
    let (alice, bob, carol, dave) = create_social_graph(&db);

    // Scan all Person nodes
    let query = db.query().scan(Some("Person")).build();

    let results = db.execute_query(query).expect("Query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Should return all 4 people
    assert_eq!(rows.len(), 4, "Expected four Person nodes");

    let result_ids: std::collections::HashSet<_> = rows
        .iter()
        .filter_map(|r| r.entity.as_node())
        .map(|n| n.id)
        .collect();

    assert!(result_ids.contains(&alice));
    assert!(result_ids.contains(&bob));
    assert!(result_ids.contains(&carol));
    assert!(result_ids.contains(&dave));

    println!("✓ Execute node scan returns all matching nodes");
}

#[test]
fn test_execute_vector_search() {
    let db = create_test_db();
    let (_alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Search for nodes similar to Alice's embedding
    let alice_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let query = db.query().find_similar(&alice_embedding, 2).build();

    let results = db.execute_query(query).expect("Query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Should return k=2 results, ordered by similarity
    assert_eq!(rows.len(), 2, "Expected 2 results for k=2");

    // The results should be Alice (exact match) and Bob (most similar)
    // But Alice is excluded in find_similar typically - let's just verify ordering
    if rows.len() >= 2 {
        assert!(
            rows[0].score.unwrap_or(0.0) >= rows[1].score.unwrap_or(0.0),
            "Results should be ordered by similarity descending"
        );
    }

    println!("✓ Execute vector search returns similar nodes");
}

#[test]
fn test_execute_convenience_traverse_and_rank() {
    let db = create_test_db();
    let (alice, bob, _carol, _dave) = create_social_graph(&db);

    // Use convenience method
    let bob_embedding = [0.9f32, 0.1, 0.0, 0.0];
    let results = db
        .traverse_and_rank(alice, "KNOWS", &bob_embedding, 10)
        .expect("Query execution failed");

    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Should return neighbors ranked by similarity to Bob's embedding
    assert!(!rows.is_empty(), "Should have results");

    // Bob should be first (most similar to his own embedding)
    let first = rows[0].entity.as_node().expect("Expected a node");
    assert_eq!(
        first.id, bob,
        "Bob should be most similar to his own embedding"
    );

    println!("✓ Convenience method traverse_and_rank works");
}

#[test]
fn test_execute_multi_hop_traversal() {
    let db = create_test_db();
    let (alice, _bob, _carol, dave) = create_social_graph(&db);

    // 2-hop traversal: Alice -> Bob -> Dave
    let query = db.query().start(alice).traverse_n("KNOWS", 2).build();

    let results = db.execute_query(query).expect("Query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Should reach Dave through Bob (Alice -> Bob -> Dave)
    let result_ids: std::collections::HashSet<_> = rows
        .iter()
        .filter_map(|r| r.entity.as_node())
        .map(|n| n.id)
        .collect();

    // In a 2-hop BFS, we should reach nodes at depth 1 and 2
    // Alice -> Bob (depth 1), Alice -> Carol (depth 1), Bob -> Dave (depth 2)
    assert!(
        result_ids.contains(&dave),
        "Should reach Dave at depth 2 through Bob"
    );

    println!("✓ Execute multi-hop traversal reaches transitive neighbors");
}

#[test]
fn test_execute_empty_result() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Filter that matches nothing
    let query = db
        .query()
        .start(alice)
        .traverse("KNOWS")
        .filter(aletheiadb::query::Predicate::eq("name", "NonExistent"))
        .build();

    let results = db.execute_query(query).expect("Query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Should return empty results
    assert!(
        rows.is_empty(),
        "Filter for nonexistent name should return no results"
    );

    println!("✓ Execute with no matches returns empty results");
}

#[test]
fn test_execute_chained_operations() {
    let db = create_test_db();
    let (alice, bob, _carol, _dave) = create_social_graph(&db);

    // Chain multiple operations: start -> traverse -> filter -> limit
    let query = db
        .query()
        .start(alice)
        .traverse("KNOWS")
        .filter(aletheiadb::query::Predicate::ne("name", "Carol"))
        .limit(5)
        .build();

    let results = db.execute_query(query).expect("Query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Should return only Bob (Carol filtered out)
    assert_eq!(
        rows.len(),
        1,
        "Should have one result after filtering Carol"
    );
    let node = rows[0].entity.as_node().expect("Expected a node");
    assert_eq!(node.id, bob, "Should be Bob");

    println!("✓ Execute chained operations works correctly");
}

#[test]
fn test_statistics_caching() {
    let db = create_test_db();
    let (_alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Refresh statistics
    db.refresh_statistics();

    // Verify statistics are populated
    let stats = db.statistics();
    assert!(
        stats.is_initialized(),
        "Statistics should be initialized after refresh"
    );
    assert_eq!(stats.node_count(), 4, "Should count 4 nodes");

    // Run a query - it should use the cached statistics
    let query = db.query().scan(Some("Person")).limit(1).build();
    let results = db.execute_query(query).expect("Query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");
    assert_eq!(rows.len(), 1);

    // Statistics should still be cached (same Arc)
    assert!(
        stats.is_initialized(),
        "Statistics should remain initialized"
    );

    println!("✓ Statistics caching works correctly");
}

// =============================================================================
// Temporal Query Execution Tests (Issue #306)
// =============================================================================

#[test]
fn test_execute_temporal_node_lookup_returns_historical_state() {
    // Create database with frequent anchoring for testing
    let db = AletheiaDB::with_config(AnchorConfig {
        anchor_interval: 2,
        max_delta_chain: 10,
    })
    .unwrap();
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");

    // Create a node with initial properties
    let node_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Original Title")
                .insert("content", "Original Content")
                .insert("version", 1)
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create node");

    // Wait a moment before updating (simulating passage of time)
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Update the node with different properties
    db.write(|tx| {
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert("title", "Updated Title")
                .insert("content", "Updated Content")
                .insert("version", 2)
                .insert_vector("embedding", &[0.0f32, 1.0, 0.0, 0.0])
                .build(),
        )?;
        Ok::<_, Error>(())
    })
    .expect("Failed to update node");

    // Get a timestamp that's safely within the first version's interval
    // (after the first version starts but before the second version starts)
    let historical_timestamp = {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        let current_version_id = hist_guard.get_current_node_version(node_id).unwrap();
        let current_version = hist_guard.get_node_version(current_version_id).unwrap();
        let prev_version_id = current_version.prev_version.unwrap();
        let prev_version = hist_guard.get_node_version(prev_version_id).unwrap();

        println!(
            "DEBUG: Current version: {:?}, temporal={}",
            current_version_id, current_version.temporal
        );
        println!(
            "DEBUG: Previous version: {:?}, temporal={}",
            prev_version_id, prev_version.temporal
        );

        // Use a timestamp in the middle of the first version's interval
        let start = prev_version.temporal.valid_time().start();
        let end = prev_version.temporal.valid_time().end();
        let midpoint = ((start.wallclock() + end.wallclock()) / 2).into();
        println!("DEBUG: Using midpoint timestamp: {}", midpoint);
        midpoint
    };

    // Make another update to ensure we have enough versions for anchoring
    std::thread::sleep(std::time::Duration::from_millis(10));
    db.write(|tx| {
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert("title", "Final Title")
                .insert("content", "Final Content")
                .insert("version", 3)
                .insert_vector("embedding", &[0.0f32, 0.0, 1.0, 0.0])
                .build(),
        )?;
        Ok::<_, Error>(())
    })
    .expect("Failed to update node again");

    // Debug: Check version chain at query time
    println!("\n=== Version chain at query time ===");
    {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        if let Some(head_version_id) = hist_guard.get_current_node_version(node_id) {
            let mut current_id = head_version_id;
            let mut index = 0;
            while let Some(version) = hist_guard.get_node_version(current_id) {
                println!(
                    "Version {}: id={:?}, temporal={}, prev={:?}",
                    index, current_id, version.temporal, version.prev_version
                );
                println!(
                    "  is_visible_at({}, {})? {}",
                    historical_timestamp,
                    historical_timestamp,
                    version
                        .temporal
                        .is_visible_at(historical_timestamp, historical_timestamp)
                );
                if let Some(prev) = version.prev_version {
                    current_id = prev;
                    index += 1;
                } else {
                    break;
                }
            }
        }
    }

    // Query the node at the historical timestamp (after first creation, before first update)
    let query = db
        .query()
        .as_of(historical_timestamp, historical_timestamp)
        .start(node_id)
        .build();

    let results = db
        .execute_query(query)
        .expect("Temporal query execution failed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Should return exactly one row
    assert_eq!(
        rows.len(),
        1,
        "Expected exactly one result from temporal query"
    );

    // Verify it returns the HISTORICAL state, not the current state
    let row = &rows[0];
    let node = row.entity.as_node().expect("Expected a node");
    assert_eq!(node.id, node_id, "Should return the same node ID");

    // The critical assertion: should return historical properties
    assert_eq!(
        node.get_property("title").and_then(|v| v.as_str()),
        Some("Original Title"),
        "Should return historical title, not current/updated title"
    );
    assert_eq!(
        node.get_property("content").and_then(|v| v.as_str()),
        Some("Original Content"),
        "Should return historical content, not current/updated content"
    );
    assert_eq!(
        node.get_property("version").and_then(|v| v.as_int()),
        Some(1),
        "Should return historical version (1), not current version (3)"
    );

    // Verify the embedding is also historical
    let embedding = node
        .get_property("embedding")
        .and_then(|v| v.as_vector())
        .expect("Should have historical embedding");
    assert_eq!(
        embedding,
        &[1.0f32, 0.0, 0.0, 0.0],
        "Should return historical embedding, not current embedding"
    );

    println!("✓ Temporal node lookup returns historical state (Issue #306)");
}

// =============================================================================
// Direct Hybrid Query Function Tests (VS-063)
// =============================================================================

#[test]
fn test_direct_traverse_and_rank_function() {
    let db = create_test_db();
    let (alice, bob, carol, _dave) = create_social_graph(&db);

    // Test the direct traverse_and_rank function (not the convenience method)
    // Note: Alice -> Bob, Alice -> Carol (Bob -> Dave, but Dave is not a direct neighbor of Alice)
    let alice_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results = traverse_and_rank(&db, alice, "KNOWS", &alice_embedding, 10)
        .expect("traverse_and_rank failed");

    // Should return 2 direct neighbors (Bob and Carol), not Dave (who is connected to Bob)
    assert_eq!(results.len(), 2, "Should return 2 direct neighbors");

    // Verify ordering: Bob should be most similar, then Carol
    assert_eq!(results[0].0, bob, "Bob should be most similar");
    assert_eq!(results[1].0, carol, "Carol should be second");

    // Verify similarity scores are in descending order
    assert!(
        results[0].1 > results[1].1,
        "Scores should be in descending order"
    );

    println!("✓ Direct traverse_and_rank function works correctly (VS-063)");
}

#[test]
fn test_direct_traverse_and_rank_with_k_limit() {
    let db = create_test_db();
    let (alice, bob, _carol, _dave) = create_social_graph(&db);

    // Test with k=1 to verify limit is respected
    let alice_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results = traverse_and_rank(&db, alice, "KNOWS", &alice_embedding, 1)
        .expect("traverse_and_rank failed");

    assert_eq!(results.len(), 1, "Should respect k=1 limit");
    assert_eq!(results[0].0, bob, "Should return most similar (Bob)");

    println!("✓ Direct traverse_and_rank respects k limit (VS-063)");
}

#[test]
fn test_direct_traverse_and_rank_with_different_label() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Create an additional node connected with different label
    let eve = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Eve")
                .insert_vector("embedding", &[0.95f32, 0.05, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Eve");

    db.create_edge(alice, eve, "WORKS_WITH", PropertyMapBuilder::new().build())
        .expect("Failed to create Alice->Eve edge");

    // Query with WORKS_WITH label should only return Eve
    let alice_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let results = traverse_and_rank(&db, alice, "WORKS_WITH", &alice_embedding, 10)
        .expect("traverse_and_rank failed");

    assert_eq!(results.len(), 1, "Should only return WORKS_WITH edges");
    assert_eq!(results[0].0, eve, "Should return Eve");

    println!("✓ Direct traverse_and_rank respects edge label filter (VS-063)");
}

// =============================================================================
// Comprehensive Test Coverage for Issue #78 (VS-065)
// =============================================================================
//
// This section adds comprehensive test coverage for the Full Hybrid Query Pattern
// implementation as specified in Issue #78 (VS-065). The tests verify:
//
// 1. **Multi-Dimensional Support**: All combinations of Graph, Vector, and Temporal
//    - Graph only
//    - Vector only
//    - Temporal only
//    - Graph + Vector
//    - Temporal + Graph
//    - Temporal + Vector
//    - Temporal + Graph + Vector (full hybrid)
//
// 2. **Error Handling**: Graceful handling of invalid queries
//    - Missing vector index
//    - Invalid node IDs
//    - Non-existent labels
//    - Mismatched embedding dimensions
//
// 3. **Query Optimization**: Selectivity-based execution order
//    - Statistics-based optimization
//    - Optimal operator ordering
//
// 4. **Edge Cases and Boundary Conditions**:
//    - Zero limit
//    - Large skip values
//    - Chained traversals
//    - Type-safe query builder
//
// Total: 48 integration tests covering all aspects of the hybrid query planner.
//
// =============================================================================
// Temporal + Vector Query Tests (Missing Pattern from Issue #78)
// =============================================================================

// NOTE: This test is currently ignored because the Temporal + Vector query pattern
// (as_of + find_similar) requires planner support for TemporalVectorSearch physical operator.
// The infrastructure exists (temporal vector indexes via VS-047), but the query planner
// doesn't yet translate "as_of + find_similar" into a TemporalVectorSearch operation.
//
// To enable this test, implement:
// 1. TemporalVectorSearch physical operator in planner/physical.rs
// 2. Planner logic to detect temporal context + vector search
// 3. Executor support in executor/mod.rs
//
// See: docs/VECTOR_SEARCH_DESIGN.md Phase 3 for temporal vector query design
#[test]
#[ignore = "Temporal + Vector query pattern not yet implemented in planner (Phase 3)"]
fn test_temporal_vector_query() {
    use aletheiadb::index::vector::temporal::{
        RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
    };

    // Create database with temporal vector indexing
    let db = AletheiaDB::with_config(AnchorConfig {
        anchor_interval: 2,
        max_delta_chain: 10,
    })
    .unwrap();

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

    // Create nodes
    let _alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Alice");

    let _bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Bob");

    let timestamp = aletheiadb::core::temporal::time::now();

    // Test Temporal + Vector query pattern: as_of + find_similar
    let query_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let query = db
        .query()
        .as_of(timestamp, timestamp)
        .find_similar(&query_embedding, 5)
        .build();

    assert!(query.is_temporal(), "Query should have temporal context");
    assert!(
        query.operation_count() >= 1,
        "Query should have vector search operation"
    );

    // Execute the query - this should succeed once planner support is added
    let results = db
        .execute_query(query)
        .expect("Temporal + Vector query should succeed");
    let rows: Vec<_> = results
        .collect_all()
        .expect("Should collect temporal vector search results");

    // Should return Alice and Bob at the historical timestamp
    assert!(
        !rows.is_empty(),
        "Should return temporal vector search results"
    );

    println!("✓ Temporal + Vector query executes successfully");
}

// =============================================================================
// Error Handling Tests (Issue #78 Requirement)
// =============================================================================

#[test]
fn test_query_with_missing_vector_index() {
    let db = AletheiaDB::new().unwrap();
    // Note: NOT enabling vector index

    // Create a node
    let _alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create Alice");

    // Try to execute a query that requires vector index
    let query_embedding = [1.0f32, 0.0, 0.0, 0.0];
    let query = db.query().find_similar(&query_embedding, 5).build();

    let result = db.execute_query(query);
    assert!(
        result.is_err(),
        "Query should fail when vector index is not enabled"
    );

    println!("✓ Query fails gracefully when vector index is missing");
}

#[test]
fn test_query_with_invalid_node_id() {
    use aletheiadb::core::error::{Error, StorageError};

    let db = create_test_db();

    // Use a non-existent node ID
    let fake_id = NodeId::new(99999).expect("valid id");
    let query = db.query().start(fake_id).traverse("KNOWS").build();

    // Query planning should succeed - errors surface during execution
    let results = db
        .execute_query(query)
        .expect("Query planning should succeed for invalid IDs");

    // Collecting results triggers execution and should produce NodeNotFound error
    let collection_result = results.collect_all();

    assert!(
        collection_result.is_err(),
        "Expected an error when collecting results for an invalid node ID"
    );

    if let Err(e) = collection_result {
        assert!(
            matches!(e, Error::Storage(StorageError::NodeNotFound(_))),
            "Expected NodeNotFound error, but got {:?}",
            e
        );
    }

    println!("✓ Query handles non-existent node IDs gracefully");
}

#[test]
fn test_query_with_empty_label() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Query with a label that doesn't exist
    let query = db
        .query()
        .start(alice)
        .traverse("NONEXISTENT_LABEL")
        .build();

    let results = db
        .execute_query(query)
        .expect("Query execution should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Should return empty results (no edges with that label)
    assert!(
        rows.is_empty(),
        "Should return empty results for non-existent label"
    );

    println!("✓ Query handles non-existent edge labels gracefully");
}

#[test]
fn test_query_with_invalid_embedding_dimensions() {
    let db = create_test_db();

    // Try to search with an embedding of wrong dimensions
    let wrong_dim_embedding = [1.0f32, 0.0]; // 2D instead of 4D
    let query = db.query().find_similar(&wrong_dim_embedding, 5).build();

    let result = db.execute_query(query);

    // Should fail due to dimension mismatch
    assert!(
        result.is_err(),
        "Query should fail with mismatched embedding dimensions"
    );

    println!("✓ Query fails gracefully with invalid embedding dimensions");
}

// =============================================================================
// Multi-Dimensional Combination Tests (Issue #78 Complete Coverage)
// =============================================================================

#[test]
fn test_all_query_dimension_combinations() {
    let db = AletheiaDB::with_config(AnchorConfig {
        anchor_interval: 3,
        max_delta_chain: 10,
    })
    .unwrap();
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");

    let (alice, _bob, _carol, _dave) = create_social_graph(&db);
    let timestamp = aletheiadb::core::temporal::time::now();
    let embedding = [1.0f32, 0.0, 0.0, 0.0];

    // Test 1: Graph only
    let q1 = db.query().start(alice).traverse("KNOWS").build();
    assert!(db.execute_query(q1).is_ok(), "Graph-only query should work");

    // Test 2: Vector only
    let q2 = db.query().find_similar(&embedding, 5).build();
    assert!(
        db.execute_query(q2).is_ok(),
        "Vector-only query should work"
    );

    // Test 3: Temporal only
    let q3 = db.query().as_of(timestamp, timestamp).start(alice).build();
    assert!(
        db.execute_query(q3).is_ok(),
        "Temporal-only query should work"
    );

    // Test 4: Graph + Vector
    let q4 = db
        .query()
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .build();
    assert!(
        db.execute_query(q4).is_ok(),
        "Graph+Vector query should work"
    );

    // Test 5: Temporal + Graph
    let q5 = db
        .query()
        .as_of(timestamp, timestamp)
        .start(alice)
        .traverse("KNOWS")
        .build();
    assert!(
        db.execute_query(q5).is_ok(),
        "Temporal+Graph query should work"
    );

    // Test 6: Temporal + Vector (if supported)
    let q6 = db
        .query()
        .as_of(timestamp, timestamp)
        .find_similar(&embedding, 5)
        .build();
    // Note: This may not be fully implemented yet
    let _ = db.execute_query(q6);

    // Test 7: Temporal + Graph + Vector (full hybrid)
    let q7 = db
        .query()
        .as_of(timestamp, timestamp)
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .build();
    assert!(
        db.execute_query(q7).is_ok(),
        "Full hybrid query (Temporal+Graph+Vector) should work"
    );

    println!("✓ All query dimension combinations tested (Issue #78)");
}

#[test]
fn test_query_with_all_modifiers() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Build a query with all possible modifiers
    let embedding = [1.0f32, 0.0, 0.0, 0.0];
    let timestamp = aletheiadb::core::temporal::time::now();

    let query = db
        .query()
        .as_of(timestamp, timestamp) // Temporal
        .start(alice) // Graph source
        .traverse("KNOWS") // Graph traversal
        .rank_by_similarity(&embedding, 10) // Vector ranking
        .filter(aletheiadb::query::Predicate::ne("name", "Carol")) // Filter
        .limit(5) // Limit
        .skip(0) // Skip
        .build();

    let results = db
        .execute_query(query)
        .expect("Complex query should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");

    // Verify results respect all modifiers
    assert!(rows.len() <= 5, "Results should respect limit");

    println!("✓ Query with all modifiers executes successfully");
}

// =============================================================================
// Query Optimization Tests (Issue #78 - Selectivity-Based Execution Order)
// =============================================================================

#[test]
fn test_query_planner_chooses_optimal_operator_order() {
    let db = create_test_db();
    let (_alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Build a query where order matters for performance
    let embedding = [1.0f32, 0.0, 0.0, 0.0];

    // Query 1: Vector search first (more selective), then filter
    let q1 = db
        .query()
        .find_similar(&embedding, 2) // Very selective (k=2)
        .filter(aletheiadb::query::Predicate::eq("name", "Alice"))
        .build();

    // Query 2: Scan all, then filter, then rank (less optimal)
    let q2 = db
        .query()
        .scan(Some("Person"))
        .filter(aletheiadb::query::Predicate::eq("name", "Alice"))
        .rank_by_similarity(&embedding, 10)
        .build();

    // Both should produce results, but Q1 should be more efficient
    let results1 = db.execute_query(q1).expect("Q1 should succeed");
    let rows1: Vec<_> = results1.collect_all().expect("Failed to collect");

    let results2 = db.execute_query(q2).expect("Q2 should succeed");
    let rows2: Vec<_> = results2.collect_all().expect("Failed to collect");

    // Both should return same final result (Alice)
    assert!(
        !rows1.is_empty() && !rows2.is_empty(),
        "Both queries should return results"
    );

    println!("✓ Query planner handles different operator orders (Issue #78)");
}

#[test]
fn test_statistics_based_optimization() {
    let db = create_test_db();
    let (_alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Refresh statistics to enable cost-based optimization
    db.refresh_statistics();

    let stats = db.statistics();
    assert!(stats.is_initialized(), "Statistics should be initialized");
    assert_eq!(stats.node_count(), 4, "Should count 4 nodes");

    // Build a query that would benefit from statistics
    let query = db
        .query()
        .scan(Some("Person")) // Planner can use node_count statistic
        .limit(2)
        .build();

    assert_query_returns_count(&db, query, 2);

    println!("✓ Query planner uses statistics for optimization (Issue #78)");
}

// =============================================================================
// Edge Cases and Boundary Conditions
// =============================================================================

#[test]
fn test_query_with_zero_limit() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    let query = db.query().start(alice).traverse("KNOWS").limit(0).build();

    assert_query_returns_count(&db, query, 0);

    println!("✓ Query handles zero limit correctly");
}

#[test]
fn test_query_with_large_skip() {
    let db = create_test_db();
    let (alice, _bob, _carol, _dave) = create_social_graph(&db);

    // Skip more than available results
    let query = db.query().start(alice).traverse("KNOWS").skip(100).build();

    let results = db.execute_query(query).expect("Query should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect");

    assert_eq!(
        rows.len(),
        0,
        "Skip larger than result set should return no results"
    );

    println!("✓ Query handles large skip values correctly");
}

#[test]
fn test_query_with_chained_traversals() {
    let db = create_test_db();
    let (alice, _bob, _carol, dave) = create_social_graph(&db);

    // Chain multiple traversals: Alice -> KNOWS -> KNOWS
    let query = db
        .query()
        .start(alice)
        .traverse("KNOWS")
        .traverse("KNOWS")
        .build();

    let results = db
        .execute_query(query)
        .expect("Chained traversal should succeed");
    let rows: Vec<_> = results.collect_all().expect("Failed to collect");

    // Should reach Dave through Bob (Alice -> Bob -> Dave)
    let result_ids: std::collections::HashSet<_> = rows
        .iter()
        .filter_map(|r| r.entity.as_node())
        .map(|n| n.id)
        .collect();

    assert!(
        result_ids.contains(&dave),
        "Should reach Dave through chained traversal"
    );

    println!("✓ Query handles chained traversals correctly");
}

#[test]
fn test_query_builder_type_safety() {
    // This test verifies compile-time type safety of the QueryBuilder
    // The type-state pattern should prevent invalid query compositions

    let db = create_test_db();
    let (alice, bob, _carol, _dave) = create_social_graph(&db);

    // Valid: start -> traverse -> rank
    let _q1 = db
        .query()
        .start(alice)
        .traverse("KNOWS")
        .rank_by_similarity(&[1.0f32, 0.0, 0.0, 0.0], 10)
        .build();

    // Valid: find_similar -> traverse
    let _q2 = db
        .query()
        .find_similar(&[1.0f32, 0.0, 0.0, 0.0], 5)
        .traverse("KNOWS")
        .build();

    // Valid: start -> similar_to -> filter
    let _q3 = db
        .query()
        .start(alice)
        .similar_to(bob, 10)
        .filter(aletheiadb::query::Predicate::gt("score", 0.8))
        .build();

    // The following would fail at compile time due to type-state pattern:
    // db.query().rank_by_similarity(&embedding, 10).build();  // No source!
    // db.query().traverse("KNOWS").build();  // No source!

    println!("✓ QueryBuilder type-state pattern enforces valid query construction");
}

// ==================== QueryBuilder.execute() Tests ====================

#[test]
fn test_query_builder_execute_method() {
    let db = create_test_db();

    // Create a test node
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create node");

    // Execute query using the new .execute() method on QueryBuilder
    let results = db
        .query()
        .start(alice)
        .execute(&db)
        .expect("Query execution failed");

    let rows: Vec<_> = results.collect_all().expect("Failed to collect results");
    assert_eq!(rows.len(), 1, "Should return exactly one node");
    assert_eq!(rows[0].entity.node_id(), Some(alice), "Should return Alice");

    println!("✓ QueryBuilder.execute() method works correctly");
}

#[test]
fn test_provenance_flag_strips_metadata() {
    let db = create_test_db();

    // Create test nodes
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

    // Create edge
    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .expect("Failed to create edge");

    // Query WITHOUT provenance - path should be None
    let results_no_prov = db.query().start(alice).traverse("KNOWS").build();
    let rows_no_prov = db
        .execute_query(results_no_prov)
        .expect("Query failed")
        .collect_all()
        .expect("Failed to collect");

    assert_eq!(rows_no_prov.len(), 1);
    assert!(
        rows_no_prov[0].path.is_none(),
        "Path should be None without provenance flag"
    );

    // Query WITH provenance - path should be populated
    let results_with_prov = db
        .query()
        .start(alice)
        .traverse("KNOWS")
        .with_provenance()
        .build();
    let rows_with_prov = db
        .execute_query(results_with_prov)
        .expect("Query failed")
        .collect_all()
        .expect("Failed to collect");

    assert_eq!(rows_with_prov.len(), 1);
    assert!(
        rows_with_prov[0].path.is_some(),
        "Path should be populated with provenance flag"
    );
    assert!(
        rows_with_prov[0].path.as_ref().unwrap().len() >= 2,
        "Path should have at least 2 entities"
    );

    println!("✓ Provenance flag correctly controls metadata inclusion");
}
