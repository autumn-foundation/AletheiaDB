//! Comprehensive Integration Tests for Hybrid Query Patterns (VS-071)
//!
//! This test file provides end-to-end integration tests for the hybrid query engine,
//! covering all combinations of Graph + Vector + Temporal dimensions.
//!
//! ## Test Categories
//!
//! 1. **traverse_and_rank** - Graph traversal with vector ranking
//!    - Various graph topologies (linear, star, cyclic, deep, wide)
//!    - Different edge labels and filters
//!
//! 2. **Temporal Vector Queries** - Time-travel with vector search
//!    - Point-in-time vector search
//!    - Vector search across time ranges
//!
//! 3. **Full Hybrid Patterns** - All three dimensions combined
//!    - Temporal + Graph + Vector queries
//!    - Complex multi-step hybrid queries
//!
//! 4. **Large-Scale Workloads** - Performance and stress testing
//!    - Large graph structures
//!    - High-dimensional vectors
//!
//! 5. **Concurrent Queries** - Thread safety validation
//!    - Parallel read queries
//!    - Concurrent read/write scenarios
//!
//! 6. **Edge Cases** - Boundary conditions and error handling
//!    - Empty results, large k, deep traversals
//!    - Invalid inputs and graceful degradation

use std::collections::HashSet;
use std::sync::Arc;
use std::thread;

use gallifreydb::{
    DistanceMetric, GallifreyDB, HnswConfig, PropertyMapBuilder, ReadOps, WriteOps,
    query::traverse_and_rank, storage::version::AnchorConfig,
};

// =============================================================================
// Test Constants
// =============================================================================

/// Vector dimension used throughout tests. Must match HnswConfig dimension.
const TEST_VECTOR_DIM: usize = 4;

// =============================================================================
// Test Helpers
// =============================================================================

/// Helper to create a test database with vector indexing enabled.
fn create_test_db() -> GallifreyDB {
    let db = GallifreyDB::new();
    let config = HnswConfig::new(TEST_VECTOR_DIM, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");
    db
}

/// Helper to create a test database with temporal configuration.
fn create_temporal_test_db() -> GallifreyDB {
    let db = GallifreyDB::with_config(AnchorConfig {
        anchor_interval: 3,
        max_delta_chain: 10,
    });
    let config = HnswConfig::new(TEST_VECTOR_DIM, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");
    db
}

/// Generate a normalized embedding vector for testing.
///
/// # Normalization
/// Vectors are normalized for cosine similarity. When using raw vectors without
/// normalization (e.g., `&[0.5f32, 0.5, 0.0, 0.0]`), results may differ from
/// normalized equivalents. Use this helper for consistent cosine similarity tests.
fn make_embedding(values: &[f32]) -> Vec<f32> {
    let mut v = values.to_vec();
    // Pad to TEST_VECTOR_DIM dimensions if needed
    while v.len() < TEST_VECTOR_DIM {
        v.push(0.0);
    }
    // Normalize for cosine similarity
    let magnitude: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if magnitude > 0.0 {
        v.iter_mut().for_each(|x| *x /= magnitude);
    }
    v
}

// =============================================================================
// Part 1: traverse_and_rank with Various Graph Structures
// =============================================================================

mod traverse_and_rank_tests {
    use super::*;

    /// Test traverse_and_rank on a linear graph: A -> B -> C -> D
    #[test]
    fn test_linear_graph() {
        let db = create_test_db();

        // Create linear chain: A -> B -> C -> D
        let a = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "A")
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let b = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "B")
                    .insert_vector("embedding", &make_embedding(&[0.9, 0.1, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let c = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "C")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let d = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "D")
                    .insert_vector("embedding", &make_embedding(&[0.0, 1.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // Create edges
        db.create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(b, c, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(c, d, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Query: Who does A know that's similar to A's embedding?
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let results = traverse_and_rank(&db, a, "KNOWS", &query_embedding, 10).unwrap();

        // Should return only B (direct neighbor of A)
        assert_eq!(results.len(), 1, "Linear graph: A only knows B directly");
        assert_eq!(results[0].0, b, "Should return B");

        println!("✓ Linear graph traverse_and_rank works correctly");
    }

    /// Test traverse_and_rank on a star graph: Center -> [A, B, C, D, E]
    #[test]
    fn test_star_graph() {
        let db = create_test_db();

        // Create center node
        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", "Center")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // Create spoke nodes with varying similarities to query
        let embeddings = [
            make_embedding(&[1.0, 0.0, 0.0, 0.0]), // Most similar to query
            make_embedding(&[0.8, 0.2, 0.0, 0.0]),
            make_embedding(&[0.5, 0.5, 0.0, 0.0]),
            make_embedding(&[0.2, 0.8, 0.0, 0.0]),
            make_embedding(&[0.0, 1.0, 0.0, 0.0]), // Least similar to query
        ];

        let mut spokes = Vec::new();
        for (i, emb) in embeddings.iter().enumerate() {
            let node = db
                .create_node(
                    "Spoke",
                    PropertyMapBuilder::new()
                        .insert("name", format!("Spoke{}", i))
                        .insert_vector("embedding", emb)
                        .build(),
                )
                .unwrap();
            db.create_edge(center, node, "CONNECTED", PropertyMapBuilder::new().build())
                .unwrap();
            spokes.push(node);
        }

        // Query from center, rank by similarity to [1, 0, 0, 0]
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let results = traverse_and_rank(&db, center, "CONNECTED", &query_embedding, 10).unwrap();

        // Should return all 5 spokes, ordered by similarity
        assert_eq!(results.len(), 5, "Star graph: Center connects to 5 spokes");

        // First result should be Spoke0 (most similar)
        assert_eq!(results[0].0, spokes[0], "Spoke0 should be most similar");

        // Verify ordering by similarity score (descending)
        for i in 1..results.len() {
            assert!(
                results[i - 1].1 >= results[i].1,
                "Results should be in descending similarity order"
            );
        }

        println!("✓ Star graph traverse_and_rank correctly orders by similarity");
    }

    /// Test traverse_and_rank on a cyclic graph: A -> B -> C -> A
    #[test]
    fn test_cyclic_graph() {
        let db = create_test_db();

        let a = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert("name", "A")
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let b = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert("name", "B")
                    .insert_vector("embedding", &make_embedding(&[0.7, 0.3, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let c = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert("name", "C")
                    .insert_vector("embedding", &make_embedding(&[0.3, 0.7, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // Create cycle: A -> B -> C -> A
        db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(b, c, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(c, a, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();

        // Query from A
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let results = traverse_and_rank(&db, a, "NEXT", &query_embedding, 10).unwrap();

        // Should only return B (direct outgoing edge from A)
        assert_eq!(results.len(), 1, "Cyclic graph: A directly connects to B");
        assert_eq!(results[0].0, b);

        println!("✓ Cyclic graph traverse_and_rank handles cycles correctly");
    }

    /// Test traverse_and_rank with deep multi-hop traversal
    #[test]
    fn test_deep_traversal() {
        let db = create_test_db();

        // Create a chain of 10 nodes
        let mut nodes = Vec::new();
        for i in 0..10 {
            let similarity = 1.0 - (i as f32 * 0.1);
            let node = db
                .create_node(
                    "Level",
                    PropertyMapBuilder::new()
                        .insert("depth", i as i64)
                        .insert_vector(
                            "embedding",
                            &make_embedding(&[similarity, 1.0 - similarity, 0.0, 0.0]),
                        )
                        .build(),
                )
                .unwrap();
            nodes.push(node);
        }

        // Create chain edges
        for i in 0..9 {
            db.create_edge(
                nodes[i],
                nodes[i + 1],
                "CHILD",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
        }

        // Use traverse_n for multi-hop
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let query = db
            .query()
            .start(nodes[0])
            .traverse_n("CHILD", 5) // 5 hops deep
            .rank_by_similarity(&query_embedding, 10)
            .build();

        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();

        // Should reach nodes at depth 1-5
        assert!(
            !rows.is_empty(),
            "Should reach nodes through multi-hop traversal"
        );

        // Check that we reached the expected depth
        let reached_depths: HashSet<_> = rows
            .iter()
            .filter_map(|r| r.entity.as_node())
            .filter_map(|n| n.get_property("depth").and_then(|v| v.as_int()))
            .collect();

        assert!(
            reached_depths.contains(&5),
            "Should reach depth 5 with 5-hop traversal"
        );

        println!("✓ Deep traversal (5 hops) works correctly");
    }

    /// Test traverse_and_rank with wide graph (many neighbors)
    #[test]
    fn test_wide_graph() {
        let db = create_test_db();

        let center = db
            .create_node(
                "Center",
                PropertyMapBuilder::new()
                    .insert("name", "Hub")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // Create 50 neighbors with varying similarities
        for i in 0..50 {
            let angle = (i as f32) * std::f32::consts::PI / 50.0;
            let emb = make_embedding(&[angle.cos(), angle.sin(), 0.0, 0.0]);
            let node = db
                .create_node(
                    "Neighbor",
                    PropertyMapBuilder::new()
                        .insert("index", i as i64)
                        .insert_vector("embedding", &emb)
                        .build(),
                )
                .expect("Failed to create neighbor node");
            db.create_edge(center, node, "LINKS", PropertyMapBuilder::new().build())
                .expect("Failed to create edge to neighbor");
        }

        // Query with k=10 (should return top 10 most similar)
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let results = traverse_and_rank(&db, center, "LINKS", &query_embedding, 10).unwrap();

        assert_eq!(results.len(), 10, "Should return exactly k=10 results");

        // Verify scores are in descending order
        for i in 1..results.len() {
            assert!(
                results[i - 1].1 >= results[i].1,
                "Results should be ordered by similarity"
            );
        }

        println!("✓ Wide graph (50 neighbors) correctly returns top-k");
    }

    /// Test traverse_and_rank with different edge labels
    #[test]
    fn test_different_edge_labels() {
        let db = create_test_db();

        let person = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // Create colleagues (WORKS_WITH)
        let colleague = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert_vector("embedding", &make_embedding(&[0.9, 0.1, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();
        db.create_edge(
            person,
            colleague,
            "WORKS_WITH",
            PropertyMapBuilder::new().build(),
        )
        .unwrap();

        // Create friends (FRIENDS_WITH)
        let friend = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Carol")
                    .insert_vector("embedding", &make_embedding(&[0.1, 0.9, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();
        db.create_edge(
            person,
            friend,
            "FRIENDS_WITH",
            PropertyMapBuilder::new().build(),
        )
        .unwrap();

        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);

        // Query WORKS_WITH only
        let works_results =
            traverse_and_rank(&db, person, "WORKS_WITH", &query_embedding, 10).unwrap();
        assert_eq!(works_results.len(), 1);
        assert_eq!(
            works_results[0].0, colleague,
            "Should only return colleague"
        );

        // Query FRIENDS_WITH only
        let friends_results =
            traverse_and_rank(&db, person, "FRIENDS_WITH", &query_embedding, 10).unwrap();
        assert_eq!(friends_results.len(), 1);
        assert_eq!(friends_results[0].0, friend, "Should only return friend");

        println!("✓ Edge label filtering works correctly");
    }
}

// =============================================================================
// Part 2: Temporal Vector Queries
// =============================================================================

mod temporal_vector_tests {
    use super::*;
    use gallifreydb::core::temporal::time::now;

    /// Test temporal query returns historical vector state
    #[test]
    fn test_temporal_query_historical_embedding() {
        let db = create_temporal_test_db();

        // Create node with initial embedding
        let initial_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let node_id = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Original")
                    .insert_vector("embedding", &initial_embedding)
                    .build(),
            )
            .unwrap();

        // Record timestamp after creation
        let create_time = now();
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Update with different embedding
        let updated_embedding = make_embedding(&[0.0, 1.0, 0.0, 0.0]);
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("title", "Updated")
                    .insert_vector("embedding", &updated_embedding)
                    .build(),
            )?;
            Ok(())
        })
        .unwrap();

        // Query at historical time should return original embedding
        let query = db
            .query()
            .as_of(create_time, create_time)
            .start(node_id)
            .build();

        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();

        assert_eq!(rows.len(), 1);
        let node = rows[0].entity.as_node().unwrap();
        assert_eq!(
            node.get_property("title").and_then(|v| v.as_str()),
            Some("Original")
        );

        println!("✓ Temporal query returns historical state");
    }

    /// Test temporal graph traversal with vector ranking
    #[test]
    fn test_temporal_traverse_and_rank() {
        let db = create_temporal_test_db();

        // Create source node
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // Create initial friend
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert_vector("embedding", &make_embedding(&[0.9, 0.1, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        let timestamp = now();
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Add another friend later
        let carol = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Carol")
                    .insert_vector("embedding", &make_embedding(&[0.95, 0.05, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        db.create_edge(alice, carol, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Full hybrid query: temporal + graph + vector
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let query = db
            .query()
            .as_of(timestamp, timestamp)
            .start(alice)
            .traverse("KNOWS")
            .rank_by_similarity(&query_embedding, 10)
            .build();

        assert!(query.is_temporal(), "Query should have temporal context");

        let results = db
            .execute_query(query)
            .expect("Failed to execute temporal query");
        let rows: Vec<_> = results
            .collect_all()
            .expect("Failed to collect query results");

        // At the historical timestamp, Alice only knew Bob.
        // Carol was added AFTER the timestamp, so she should NOT appear.
        // Issue #400 fixed: Temporal edge traversal is now implemented.
        assert_eq!(
            rows.len(),
            1,
            "At historical timestamp, Alice should only know Bob (not Carol)"
        );

        // Verify Bob is the one result
        let result_node = rows[0].entity.as_node().expect("Result should be a node");
        assert_eq!(
            result_node.id, bob,
            "The single result should be Bob (connected before timestamp)"
        );

        println!("✓ Temporal traverse_and_rank correctly filters edges by timestamp");
    }

    /// Test between() temporal range query
    #[test]
    fn test_temporal_range_query() {
        let db = create_temporal_test_db();

        let start_time = now();
        std::thread::sleep(std::time::Duration::from_millis(5));

        let node = db
            .create_node(
                "Event",
                PropertyMapBuilder::new()
                    .insert("name", "Conference")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));
        let end_time = now();

        // Query using between() range
        let query = db.query().between(start_time, end_time).start(node).build();

        assert!(query.is_temporal(), "Query should have temporal context");

        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();

        assert_eq!(rows.len(), 1, "Should find the node within time range");

        println!("✓ Temporal range query (between) works correctly");
    }
}

// =============================================================================
// Part 3: Full Hybrid Query Patterns
// =============================================================================

mod full_hybrid_tests {
    use super::*;
    use gallifreydb::core::temporal::time::now;
    use gallifreydb::query::Predicate;

    /// Test complete hybrid: temporal + graph + vector + filter
    #[test]
    fn test_complete_hybrid_query() {
        let db = create_temporal_test_db();

        // Create a social network
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("active", true)
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert("active", true)
                    .insert_vector("embedding", &make_embedding(&[0.9, 0.1, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let carol = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Carol")
                    .insert("active", false) // Inactive
                    .insert_vector("embedding", &make_embedding(&[0.95, 0.05, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(alice, carol, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Capture timestamp after data creation
        let timestamp = now();

        // Full hybrid: temporal + graph + vector + filter
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let query = db
            .query()
            .as_of(timestamp, timestamp)
            .start(alice)
            .traverse("KNOWS")
            .filter(Predicate::eq("active", true))
            .rank_by_similarity(&query_embedding, 10)
            .build();

        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();

        // Should only return Bob (Carol is inactive)
        assert_eq!(rows.len(), 1, "Should filter out inactive users");
        let node = rows[0].entity.as_node().unwrap();
        assert_eq!(node.id, bob);

        println!("✓ Complete hybrid query (temporal+graph+vector+filter) works");
    }

    /// Test chained traversals with vector ranking
    #[test]
    fn test_chained_traversal_with_ranking() {
        let db = create_test_db();

        // Create: Alice -> Bob -> [Carol, Dave]
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let carol = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Carol")
                    .insert_vector("embedding", &make_embedding(&[0.9, 0.1, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let dave = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Dave")
                    .insert_vector("embedding", &make_embedding(&[0.1, 0.9, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(bob, carol, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(bob, dave, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Two-hop traversal then rank
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let query = db
            .query()
            .start(alice)
            .traverse("KNOWS")
            .traverse("KNOWS")
            .rank_by_similarity(&query_embedding, 10)
            .build();

        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();

        // Should reach Carol and Dave through Bob
        assert_eq!(rows.len(), 2, "Should reach Carol and Dave");

        // Carol should be ranked higher (more similar to query)
        let first = rows[0].entity.as_node().unwrap();
        assert_eq!(first.id, carol, "Carol should be most similar");

        println!("✓ Chained traversal with vector ranking works");
    }

    /// Test hybrid query with limit and skip
    #[test]
    fn test_hybrid_with_pagination() {
        let db = create_test_db();

        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", "Center")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // Create 20 neighbors
        for i in 0..20 {
            let similarity = 1.0 - (i as f32 * 0.05);
            let node = db
                .create_node(
                    "Neighbor",
                    PropertyMapBuilder::new()
                        .insert("index", i as i64)
                        .insert_vector(
                            "embedding",
                            &make_embedding(&[similarity, 1.0 - similarity, 0.0, 0.0]),
                        )
                        .build(),
                )
                .unwrap();
            db.create_edge(center, node, "LINKS", PropertyMapBuilder::new().build())
                .unwrap();
        }

        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);

        // Test skip and limit
        let query = db
            .query()
            .start(center)
            .traverse("LINKS")
            .rank_by_similarity(&query_embedding, 20)
            .skip(5)
            .limit(10)
            .build();

        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();

        assert_eq!(
            rows.len(),
            10,
            "Should return exactly 10 after skip(5).limit(10)"
        );

        println!("✓ Hybrid query with pagination (skip/limit) works");
    }

    /// Test vector search followed by graph traversal
    #[test]
    fn test_vector_then_graph() {
        let db = create_test_db();

        // Create nodes with embeddings
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let _bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert_vector("embedding", &make_embedding(&[0.1, 0.9, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let carol = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Carol")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // Alice knows Carol (Bob exists but has no edges - tests that vector search finds nodes without connections)
        db.create_edge(alice, carol, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Find similar to embedding, then traverse
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let query = db
            .query()
            .find_similar(&query_embedding, 2)
            .traverse("KNOWS")
            .build();

        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();

        // Alice is most similar, and Alice knows Carol
        let result_ids: HashSet<_> = rows
            .iter()
            .filter_map(|r| r.entity.as_node())
            .map(|n| n.id)
            .collect();

        assert!(
            result_ids.contains(&carol),
            "Should find Carol through Alice"
        );

        println!("✓ Vector search then graph traversal works");
    }

    /// Test provenance tracking in hybrid queries
    #[test]
    fn test_hybrid_with_provenance() {
        let db = create_test_db();

        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert_vector("embedding", &make_embedding(&[0.9, 0.1, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Query with provenance enabled
        let query = db
            .query()
            .start(alice)
            .traverse("KNOWS")
            .with_provenance()
            .build();

        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();

        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].path.is_some(),
            "Provenance should include traversal path"
        );

        let path = rows[0].path.as_ref().unwrap();
        assert!(
            path.len() >= 2,
            "Path should include at least start and end"
        );

        println!("✓ Provenance tracking works in hybrid queries");
    }
}

// =============================================================================
// Part 4: Large-Scale Hybrid Workloads
// =============================================================================

mod large_scale_tests {
    use super::*;

    /// Test traverse_and_rank with 1000 neighbors
    #[test]
    fn test_large_neighbor_count() {
        let db = create_test_db();

        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", "Center")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // Create 1000 neighbors
        for i in 0..1000 {
            let angle = (i as f32) * 2.0 * std::f32::consts::PI / 1000.0;
            let emb = make_embedding(&[angle.cos(), angle.sin(), 0.0, 0.0]);
            let node = db
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert("index", i as i64)
                        .insert_vector("embedding", &emb)
                        .build(),
                )
                .unwrap();
            db.create_edge(center, node, "LINKS", PropertyMapBuilder::new().build())
                .unwrap();
        }

        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);

        // Time the query
        let start = std::time::Instant::now();
        let results = traverse_and_rank(&db, center, "LINKS", &query_embedding, 50).unwrap();
        let duration = start.elapsed();

        assert_eq!(results.len(), 50, "Should return top 50");
        assert!(
            duration.as_millis() < 1000,
            "Query should complete in under 1 second, took {:?}",
            duration
        );

        // Verify ordering
        for i in 1..results.len() {
            assert!(
                results[i - 1].1 >= results[i].1,
                "Results should be ordered by similarity"
            );
        }

        println!(
            "✓ Large-scale query (1000 neighbors, k=50) completed in {:?}",
            duration
        );
    }

    /// Test hybrid query with multiple hops and many nodes
    #[test]
    fn test_multi_hop_large_graph() {
        let db = create_test_db();

        // Create a tree structure: 1 root -> 10 children -> 10 grandchildren each = 111 nodes
        let root = db
            .create_node(
                "Root",
                PropertyMapBuilder::new()
                    .insert("level", 0i64)
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let mut children = Vec::new();
        for i in 0..10 {
            let child = db
                .create_node(
                    "Child",
                    PropertyMapBuilder::new()
                        .insert("level", 1i64)
                        .insert("index", i as i64)
                        .insert_vector("embedding", &make_embedding(&[0.8, 0.2, 0.0, 0.0]))
                        .build(),
                )
                .unwrap();
            db.create_edge(root, child, "HAS_CHILD", PropertyMapBuilder::new().build())
                .unwrap();
            children.push(child);
        }

        for (ci, &child) in children.iter().enumerate() {
            for gi in 0..10 {
                let similarity = 1.0 - ((ci * 10 + gi) as f32 * 0.01);
                let grandchild = db
                    .create_node(
                        "Grandchild",
                        PropertyMapBuilder::new()
                            .insert("level", 2i64)
                            .insert("parent_index", ci as i64)
                            .insert("index", gi as i64)
                            .insert_vector(
                                "embedding",
                                &make_embedding(&[similarity, 1.0 - similarity, 0.0, 0.0]),
                            )
                            .build(),
                    )
                    .expect("Failed to create grandchild node");
                db.create_edge(
                    child,
                    grandchild,
                    "HAS_CHILD",
                    PropertyMapBuilder::new().build(),
                )
                .expect("Failed to create edge to grandchild");
            }
        }

        // Two-hop traversal with ranking
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let query = db
            .query()
            .start(root)
            .traverse_n("HAS_CHILD", 2)
            .rank_by_similarity(&query_embedding, 20)
            .build();

        let start = std::time::Instant::now();
        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();
        let duration = start.elapsed();

        assert_eq!(rows.len(), 20, "Should return top 20 grandchildren");
        assert!(
            duration.as_millis() < 500,
            "Query should complete quickly, took {:?}",
            duration
        );

        println!(
            "✓ Multi-hop large graph query (111 nodes, 2 hops, k=20) completed in {:?}",
            duration
        );
    }

    /// Test batch creation and querying
    #[test]
    fn test_batch_operations() {
        let db = create_test_db();

        // Create hub nodes
        let mut hubs = Vec::new();
        for i in 0..10 {
            let hub = db
                .create_node(
                    "Hub",
                    PropertyMapBuilder::new()
                        .insert("hub_id", i as i64)
                        .insert_vector(
                            "embedding",
                            &make_embedding(&[
                                (i as f32) / 10.0,
                                1.0 - (i as f32) / 10.0,
                                0.0,
                                0.0,
                            ]),
                        )
                        .build(),
                )
                .unwrap();
            hubs.push(hub);
        }

        // Connect hubs in a chain
        for i in 0..9 {
            db.create_edge(
                hubs[i],
                hubs[i + 1],
                "NEXT",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
        }

        // Create 50 spokes per hub
        for (hi, &hub) in hubs.iter().enumerate() {
            for si in 0..50 {
                let spoke = db
                    .create_node(
                        "Spoke",
                        PropertyMapBuilder::new()
                            .insert("hub_id", hi as i64)
                            .insert("spoke_id", si as i64)
                            .insert_vector(
                                "embedding",
                                &make_embedding(&[(si as f32) / 50.0, 0.5, 0.0, 0.0]),
                            )
                            .build(),
                    )
                    .unwrap();
                db.create_edge(hub, spoke, "HAS_SPOKE", PropertyMapBuilder::new().build())
                    .unwrap();
            }
        }

        // Query from first hub
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let results = traverse_and_rank(&db, hubs[0], "HAS_SPOKE", &query_embedding, 10).unwrap();

        assert_eq!(results.len(), 10);

        println!("✓ Batch operations work correctly (10 hubs × 50 spokes)");
    }
}

// =============================================================================
// Part 5: Concurrent Hybrid Queries
// =============================================================================

mod concurrent_tests {
    use super::*;

    /// Test parallel read queries
    #[test]
    fn test_parallel_reads() {
        let db = Arc::new(create_test_db());

        // Create test graph
        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", "Center")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        for i in 0..20 {
            let node = db
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert("index", i as i64)
                        .insert_vector(
                            "embedding",
                            &make_embedding(&[
                                (i as f32) / 20.0,
                                1.0 - (i as f32) / 20.0,
                                0.0,
                                0.0,
                            ]),
                        )
                        .build(),
                )
                .expect("Failed to create node for parallel reads");
            db.create_edge(center, node, "LINKS", PropertyMapBuilder::new().build())
                .expect("Failed to create edge for parallel reads");
        }

        // Spawn multiple reader threads
        let mut handles = Vec::new();
        for thread_id in 0..4 {
            let db_clone = Arc::clone(&db);
            let center_clone = center;
            let handle = thread::spawn(move || {
                for _ in 0..10 {
                    let query_embedding = make_embedding(&[
                        (thread_id as f32) / 4.0,
                        1.0 - (thread_id as f32) / 4.0,
                        0.0,
                        0.0,
                    ]);
                    let results =
                        traverse_and_rank(&db_clone, center_clone, "LINKS", &query_embedding, 5)
                            .unwrap();
                    assert_eq!(
                        results.len(),
                        5,
                        "Thread {} should get 5 results",
                        thread_id
                    );
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().expect("Thread panicked");
        }

        println!("✓ Parallel reads (4 threads × 10 queries) work correctly");
    }

    /// Test concurrent read/write
    #[test]
    fn test_concurrent_read_write() {
        let db = Arc::new(create_test_db());

        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", "Center")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // Create initial nodes
        for i in 0..10 {
            let node = db
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert("index", i as i64)
                        .insert_vector(
                            "embedding",
                            &make_embedding(&[(i as f32) / 10.0, 0.5, 0.0, 0.0]),
                        )
                        .build(),
                )
                .unwrap();
            db.create_edge(center, node, "LINKS", PropertyMapBuilder::new().build())
                .unwrap();
        }

        // Reader threads
        let mut handles = Vec::new();
        for _ in 0..2 {
            let db_clone = Arc::clone(&db);
            let center_clone = center;
            handles.push(thread::spawn(move || {
                for _ in 0..20 {
                    let query_embedding = make_embedding(&[0.5, 0.5, 0.0, 0.0]);
                    let _ =
                        traverse_and_rank(&db_clone, center_clone, "LINKS", &query_embedding, 5);
                    thread::sleep(std::time::Duration::from_millis(1));
                }
            }));
        }

        // Writer thread
        let db_clone = Arc::clone(&db);
        let center_clone = center;
        handles.push(thread::spawn(move || {
            for i in 10..20 {
                let node = db_clone
                    .create_node(
                        "Node",
                        PropertyMapBuilder::new()
                            .insert("index", i as i64)
                            .insert_vector(
                                "embedding",
                                &make_embedding(&[(i as f32) / 20.0, 0.5, 0.0, 0.0]),
                            )
                            .build(),
                    )
                    .unwrap();
                db_clone
                    .create_edge(
                        center_clone,
                        node,
                        "LINKS",
                        PropertyMapBuilder::new().build(),
                    )
                    .unwrap();
                thread::sleep(std::time::Duration::from_millis(5));
            }
        }));

        // Wait for all threads
        for handle in handles {
            handle.join().expect("Thread panicked");
        }

        // Verify final state: 10 initial nodes + 10 added by writer thread = 20 total
        let query_embedding = make_embedding(&[0.5, 0.5, 0.0, 0.0]);
        let results = traverse_and_rank(&db, center, "LINKS", &query_embedding, 100)
            .expect("Failed to query final state");
        assert_eq!(
            results.len(),
            20,
            "Should have initial 10 nodes plus 10 added by writer thread"
        );

        println!("✓ Concurrent read/write works correctly");
    }

    /// Test query during updates
    #[test]
    fn test_query_during_updates() {
        let db = Arc::new(create_test_db());

        let node = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert("version", 0i64)
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let db_clone = Arc::clone(&db);
        let node_clone = node;

        // Updater thread
        let updater = thread::spawn(move || {
            for i in 1..=10 {
                db_clone
                    .write(|tx| {
                        tx.update_node(
                            node_clone,
                            PropertyMapBuilder::new()
                                .insert("version", i as i64)
                                .insert_vector(
                                    "embedding",
                                    &make_embedding(&[
                                        1.0 - (i as f32) / 10.0,
                                        (i as f32) / 10.0,
                                        0.0,
                                        0.0,
                                    ]),
                                )
                                .build(),
                        )?;
                        Ok(())
                    })
                    .unwrap();
                thread::sleep(std::time::Duration::from_millis(5));
            }
        });

        // Reader thread
        for _ in 0..20 {
            let result = db.read(|tx| tx.get_node(node));
            assert!(result.is_ok(), "Read should always succeed");
            thread::sleep(std::time::Duration::from_millis(2));
        }

        updater.join().expect("Updater panicked");

        println!("✓ Query during concurrent updates works correctly");
    }
}

// =============================================================================
// Part 6: Edge Cases
// =============================================================================

mod edge_case_tests {
    use super::*;

    /// Test traverse_and_rank with no neighbors
    #[test]
    fn test_empty_traversal_results() {
        let db = create_test_db();

        let isolated = db
            .create_node(
                "Isolated",
                PropertyMapBuilder::new()
                    .insert("name", "Lonely")
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let results = traverse_and_rank(&db, isolated, "KNOWS", &query_embedding, 10).unwrap();

        assert!(results.is_empty(), "Isolated node should have no neighbors");

        println!("✓ Empty traversal results handled correctly");
    }

    /// Test with k larger than result set
    #[test]
    fn test_large_k_value() {
        let db = create_test_db();

        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", "Center")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // Only 3 neighbors
        for i in 0..3 {
            let node = db
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert("index", i as i64)
                        .insert_vector(
                            "embedding",
                            &make_embedding(&[(i as f32) / 3.0, 0.5, 0.0, 0.0]),
                        )
                        .build(),
                )
                .unwrap();
            db.create_edge(center, node, "LINKS", PropertyMapBuilder::new().build())
                .unwrap();
        }

        // Request k=100 but only 3 exist
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let results = traverse_and_rank(&db, center, "LINKS", &query_embedding, 100).unwrap();

        assert_eq!(results.len(), 3, "Should return all 3 even with k=100");

        println!("✓ Large k value (k > result_count) handled correctly");
    }

    /// Test with k=1
    #[test]
    fn test_k_equals_one() {
        let db = create_test_db();

        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", "Center")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let most_similar = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert("name", "Best")
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let less_similar = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert("name", "Worse")
                    .insert_vector("embedding", &make_embedding(&[0.0, 1.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        db.create_edge(
            center,
            most_similar,
            "LINKS",
            PropertyMapBuilder::new().build(),
        )
        .unwrap();
        db.create_edge(
            center,
            less_similar,
            "LINKS",
            PropertyMapBuilder::new().build(),
        )
        .unwrap();

        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let results = traverse_and_rank(&db, center, "LINKS", &query_embedding, 1).unwrap();

        assert_eq!(results.len(), 1, "Should return exactly 1");
        assert_eq!(results[0].0, most_similar, "Should return most similar");

        println!("✓ k=1 correctly returns single most similar");
    }

    /// Test deep traversal (many hops)
    #[test]
    fn test_very_deep_traversal() {
        let db = create_test_db();

        // Create chain of 20 nodes
        let mut nodes = Vec::new();
        for i in 0..20 {
            let node = db
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert("depth", i as i64)
                        .insert_vector(
                            "embedding",
                            &make_embedding(&[
                                1.0 - (i as f32) / 20.0,
                                (i as f32) / 20.0,
                                0.0,
                                0.0,
                            ]),
                        )
                        .build(),
                )
                .unwrap();
            nodes.push(node);
        }

        for i in 0..19 {
            db.create_edge(
                nodes[i],
                nodes[i + 1],
                "NEXT",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
        }

        // 10-hop traversal
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let query = db
            .query()
            .start(nodes[0])
            .traverse_n("NEXT", 10)
            .rank_by_similarity(&query_embedding, 20)
            .build();

        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();

        // Should reach nodes at depths 1-10
        let depths: HashSet<_> = rows
            .iter()
            .filter_map(|r| r.entity.as_node())
            .filter_map(|n| n.get_property("depth").and_then(|v| v.as_int()))
            .collect();

        assert!(depths.contains(&10), "Should reach depth 10");

        println!("✓ Very deep traversal (10 hops) works correctly");
    }

    /// Test with non-existent edge label
    #[test]
    fn test_nonexistent_edge_label() {
        let db = create_test_db();

        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", "Center")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        let neighbor = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert("name", "Neighbor")
                    .insert_vector("embedding", &make_embedding(&[1.0, 0.0, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        db.create_edge(
            center,
            neighbor,
            "ACTUAL_LABEL",
            PropertyMapBuilder::new().build(),
        )
        .unwrap();

        // Query with wrong label
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let results = traverse_and_rank(&db, center, "WRONG_LABEL", &query_embedding, 10).unwrap();

        assert!(results.is_empty(), "Wrong label should return no results");

        println!("✓ Non-existent edge label returns empty results");
    }

    /// Test with identical embeddings
    #[test]
    fn test_identical_embeddings() {
        let db = create_test_db();

        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", "Center")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        // All neighbors have identical embeddings
        let embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        for i in 0..5 {
            let node = db
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert("index", i as i64)
                        .insert_vector("embedding", &embedding)
                        .build(),
                )
                .expect("Failed to create node with identical embedding");
            db.create_edge(center, node, "LINKS", PropertyMapBuilder::new().build())
                .expect("Failed to create edge to identical embedding node");
        }

        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let results = traverse_and_rank(&db, center, "LINKS", &query_embedding, 10).unwrap();

        assert_eq!(results.len(), 5, "Should return all 5 identical nodes");

        // All scores should be equal (or very close)
        let first_score = results[0].1;
        for result in &results {
            assert!(
                (result.1 - first_score).abs() < 0.001,
                "Identical embeddings should have identical scores"
            );
        }

        println!("✓ Identical embeddings handled correctly");
    }

    /// Test with zero vector
    ///
    /// Zero vectors are an edge case for cosine similarity (division by zero).
    /// The query should not panic and should handle the edge case gracefully,
    /// either by returning an error or by returning results with NaN/0 similarity.
    #[test]
    fn test_zero_embedding_query() {
        let db = create_test_db();

        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", "Center")
                    .insert_vector("embedding", &[0.5f32, 0.5, 0.0, 0.0])
                    .build(),
            )
            .expect("Failed to create center node");

        let neighbor = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert("name", "Neighbor")
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                    .build(),
            )
            .expect("Failed to create neighbor node");

        db.create_edge(center, neighbor, "LINKS", PropertyMapBuilder::new().build())
            .expect("Failed to create edge");

        // Query with zero vector (edge case for cosine similarity)
        let zero_embedding = [0.0f32, 0.0, 0.0, 0.0];
        let results = traverse_and_rank(&db, center, "LINKS", &zero_embedding, 10);

        // Zero vectors should be handled gracefully - query should not panic.
        // Expected behavior: either return an error or return results with similarity 0/NaN.
        assert!(
            results.is_ok(),
            "Query with zero vector should not panic or fail catastrophically"
        );
        if let Ok(r) = results {
            // With a zero vector query and cosine similarity, similarities will be 0 or NaN.
            // The function should still return the neighbor.
            assert_eq!(
                r.len(),
                1,
                "Should return the single neighbor even with zero query vector"
            );
        }

        println!("✓ Zero embedding query handled gracefully");
    }

    /// Test filter that matches nothing
    #[test]
    fn test_filter_matches_nothing() {
        let db = create_test_db();
        use gallifreydb::query::Predicate;

        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", "Center")
                    .insert_vector("embedding", &make_embedding(&[0.5, 0.5, 0.0, 0.0]))
                    .build(),
            )
            .unwrap();

        for i in 0..5 {
            let node = db
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert("type", "actual")
                        .insert("index", i as i64)
                        .insert_vector(
                            "embedding",
                            &make_embedding(&[(i as f32) / 5.0, 0.5, 0.0, 0.0]),
                        )
                        .build(),
                )
                .unwrap();
            db.create_edge(center, node, "LINKS", PropertyMapBuilder::new().build())
                .unwrap();
        }

        // Filter for type that doesn't exist
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let query = db
            .query()
            .start(center)
            .traverse("LINKS")
            .filter(Predicate::eq("type", "nonexistent"))
            .rank_by_similarity(&query_embedding, 10)
            .build();

        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();

        assert!(
            rows.is_empty(),
            "Filter matching nothing should return empty"
        );

        println!("✓ Filter matching nothing returns empty results");
    }

    /// Test scan with label filter
    #[test]
    fn test_scan_with_label_and_rank() {
        let db = create_test_db();

        // Create nodes with different labels
        for i in 0..5 {
            db.create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("Person{}", i))
                    .insert_vector(
                        "embedding",
                        &make_embedding(&[(i as f32) / 5.0, 0.5, 0.0, 0.0]),
                    )
                    .build(),
            )
            .unwrap();
        }

        for i in 0..5 {
            db.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("name", format!("Doc{}", i))
                    .insert_vector(
                        "embedding",
                        &make_embedding(&[0.5, (i as f32) / 5.0, 0.0, 0.0]),
                    )
                    .build(),
            )
            .unwrap();
        }

        // Scan only Person nodes
        let query_embedding = make_embedding(&[1.0, 0.0, 0.0, 0.0]);
        let query = db
            .query()
            .scan(Some("Person"))
            .rank_by_similarity(&query_embedding, 10)
            .build();

        let results = db.execute_query(query).unwrap();
        let rows: Vec<_> = results.collect_all().unwrap();

        assert_eq!(rows.len(), 5, "Should return all 5 Person nodes");

        // Verify all results are Person nodes
        for row in &rows {
            let node = row.entity.as_node().unwrap();
            assert!(
                node.get_property("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.starts_with("Person"))
                    .unwrap_or(false),
                "All results should be Person nodes"
            );
        }

        println!("✓ Scan with label filter and ranking works correctly");
    }
}
