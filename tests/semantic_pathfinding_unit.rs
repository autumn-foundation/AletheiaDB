use aletheiadb::time;
// Moved from src/query/semantic_pathfinding.rs to break circular dependencies
use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::error::Error;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::db::AletheiaDB;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::query::semantic_pathfinding::*;

fn create_test_db_sentry() -> AletheiaDB {
    let db = AletheiaDB::new().unwrap();
    // Enable vector index to ensure vector properties are handled correctly
    // (though SemanticPathfinder works with raw properties too)
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(3, DistanceMetric::Cosine))
        .enable()
        .unwrap();
    db
}

#[test]
fn test_semantic_pathfinding_prefers_similar_nodes() {
    let db = create_test_db_sentry();

    // Topic: "Fruits" (Query will be close to this)
    let fruit_vec = vec![1.0, 0.0, 0.0];

    // Topic: "Tech" (Dissimilar)
    let tech_vec = vec![0.0, 1.0, 0.0];

    // Start
    let start = db
        .create_node(
            "Start",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.5, 0.5, 0.0])
                .build(),
        )
        .unwrap();

    // End
    let end = db
        .create_node(
            "End",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.5, 0.5, 0.0])
                .build(),
        )
        .unwrap();

    // Path 1: "Apple" (Fruit) -> End
    let apple = db
        .create_node(
            "Apple",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &fruit_vec)
                .build(),
        )
        .unwrap();

    // Path 2: "Laptop" (Tech) -> End
    let laptop = db
        .create_node(
            "Laptop",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &tech_vec)
                .build(),
        )
        .unwrap();

    // Edges
    db.create_edge(start, apple, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(apple, end, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();

    db.create_edge(start, laptop, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(laptop, end, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();

    // Find path with query "Banana" (Fruit-like)
    let query = vec![0.9, 0.1, 0.0];

    let pathfinder = SemanticPathfinder::new(&db, "embedding");
    let path = pathfinder
        .find_path(start, end, &query, 10, false)
        .unwrap()
        .unwrap();

    // Should prefer Apple over Laptop
    assert_eq!(path.len(), 3);
    assert_eq!(path[0], start);
    assert_eq!(path[1], apple);
    assert_eq!(path[2], end);
}

#[test]
fn test_semantic_pathfinding_time_travel() {
    let db = create_test_db_sentry();
    let _now = time::now();

    // Create nodes
    let start = db
        .create_node("Start", PropertyMapBuilder::new().build())
        .unwrap();
    let middle = db
        .create_node(
            "Middle",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();
    let end = db
        .create_node("End", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edges at t0
    // Use write_with_timestamp to ensure t0 covers the creation
    let (_, t_edges) = db
        .write_with_timestamp(|tx| {
            tx.create_edge(start, middle, "NEXT", PropertyMapBuilder::new().build())?;
            tx.create_edge(middle, end, "NEXT", PropertyMapBuilder::new().build())?;
            Ok::<_, Error>(())
        })
        .unwrap();

    let t0 = t_edges;

    let query = vec![1.0, 0.0, 0.0];
    let pathfinder = SemanticPathfinder::new(&db, "embedding");

    // Query at t0: Path should exist (BEFORE DELETION)
    let path_t0 = pathfinder
        .find_path_at_time(start, end, &query, t0, 10, false)
        .unwrap();
    assert!(path_t0.is_some(), "Path should exist at t0 before deletion");

    // Delete "Middle" node at t1 (which should break the path)
    // Use delete_node_cascade to ensure edges are also deleted from current storage
    let (_, t_delete) = db
        .write_with_timestamp(|tx| tx.delete_node_cascade(middle))
        .unwrap();
    let _t1 = t_delete;

    // Verify time monotonicity (HLC guarantees distinct timestamps)
    assert!(
        t_delete > t0,
        "Time must advance monotonically for subsequent transactions"
    );

    // Query at t0 AFTER deletion: With temporal adjacency index (enabled by default),
    // the path SHOULD be found even though edges are deleted from current storage
    let path_t0_after_delete = pathfinder
        .find_path_at_time(start, end, &query, t0, 10, false)
        .unwrap();
    assert!(
        path_t0_after_delete.is_some(),
        "Temporal adjacency index (enabled by default) should find path through deleted edges"
    );
    let path = path_t0_after_delete.unwrap();
    assert_eq!(path.len(), 3);
    assert_eq!(path[0], start);
    assert_eq!(path[1], middle);
    assert_eq!(path[2], end);

    // Test "Future Path" scenario: Path exists now but didn't in past

    let new_middle = db
        .create_node(
            "NewMiddle",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let (_, t_new_edges) = db
        .write_with_timestamp(|tx| {
            tx.create_edge(start, new_middle, "NEXT", PropertyMapBuilder::new().build())?;
            tx.create_edge(new_middle, end, "NEXT", PropertyMapBuilder::new().build())?;
            Ok::<_, Error>(())
        })
        .unwrap();

    let t2 = t_new_edges;

    // Path exists at t2
    let path_t2 = pathfinder
        .find_path_at_time(start, end, &query, t2, 10, false)
        .unwrap();
    assert!(path_t2.is_some(), "Path should exist at t2");

    // Query at t0 again: Should find the ORIGINAL path through middle
    // (not the new path through new_middle which was created at t2)
    // With temporal adjacency index, deleted edges are still accessible
    // when querying at times before they were deleted.
    let path_t0_check = pathfinder
        .find_path_at_time(start, end, &query, t0, 10, false)
        .unwrap();
    assert!(
        path_t0_check.is_some(),
        "Should find original path at t0 (through middle, not new_middle)"
    );
    // Verify it's the original middle node, not new_middle
    assert_eq!(path_t0_check.as_ref().unwrap()[1], middle);
}

#[test]
fn test_pathfinding_zero_max_depth() {
    let db = create_test_db_sentry();
    // Create a minimal graph A -> B
    let a = db
        .create_node("A", PropertyMapBuilder::new().build())
        .unwrap();
    let b = db
        .create_node("B", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();

    let query = vec![0.0; 3];
    let pathfinder = SemanticPathfinder::new(&db, "embedding");

    // Max depth 0 should fail to find path if A != B
    let path = pathfinder.find_path(a, b, &query, 0, false).unwrap();
    assert!(path.is_none(), "Depth 0 should not allow traversal");
}

#[test]
fn test_pathfinding_start_equals_end() {
    let db = create_test_db_sentry();
    let a = db
        .create_node("A", PropertyMapBuilder::new().build())
        .unwrap();

    let query = vec![0.0; 3];
    let pathfinder = SemanticPathfinder::new(&db, "embedding");

    // Should find path [A] immediately
    let path = pathfinder.find_path(a, a, &query, 10, false).unwrap();
    assert!(path.is_some());
    assert_eq!(path.unwrap(), vec![a]);
}

#[test]
fn test_pathfinding_disconnected() {
    let db = create_test_db_sentry();
    let a = db
        .create_node("A", PropertyMapBuilder::new().build())
        .unwrap();
    let b = db
        .create_node("B", PropertyMapBuilder::new().build())
        .unwrap();

    // No edges

    let query = vec![0.0; 3];
    let pathfinder = SemanticPathfinder::new(&db, "embedding");

    let path = pathfinder.find_path(a, b, &query, 10, false).unwrap();
    assert!(path.is_none());
}

#[test]
fn test_pathfinding_cycle() {
    let db = create_test_db_sentry();
    let a = db
        .create_node("A", PropertyMapBuilder::new().build())
        .unwrap();
    let b = db
        .create_node("B", PropertyMapBuilder::new().build())
        .unwrap();

    // Cycle: A -> B -> A
    db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(b, a, "BACK", PropertyMapBuilder::new().build())
        .unwrap();

    let query = vec![0.0; 3];
    let pathfinder = SemanticPathfinder::new(&db, "embedding");

    // Search for unreachable C
    let c = db
        .create_node("C", PropertyMapBuilder::new().build())
        .unwrap();

    // Should terminate and return None, not hang
    let path = pathfinder.find_path(a, c, &query, 10, false).unwrap();
    assert!(path.is_none());
}

#[test]
fn test_calculate_semantic_cost_dimension_mismatch() {
    let db = create_test_db_sentry();
    // Node with 3D vector
    let a = db
        .create_node(
            "A",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();
    let b = db
        .create_node(
            "B",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.0, 1.0, 0.0])
                .build(),
        )
        .unwrap();

    db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();

    // Query with 4D vector -> Mismatch!
    let query = vec![0.0; 4];
    let pathfinder = SemanticPathfinder::new(&db, "embedding");

    // Sentry 🛡️: Should handle dimension mismatch gracefully by treating the node as incompatible
    // (infinite cost), effectively blocking the path.
    // Since A->B is the only path, and B is incompatible, it should return Ok(None).
    let result = pathfinder.find_path(a, b, &query, 10, false);
    assert!(result.is_ok());
    assert!(
        result.unwrap().is_none(),
        "Path should be blocked due to dimension mismatch"
    );
}

#[test]
fn test_pathfinding_skips_incompatible_dimensions() {
    // 🛡️ Sentry Test: Mixed dimensions should not crash pathfinding.
    // Setup:
    // Start (3D) -> Broken (4D) -> End (3D)
    //            -> Valid (3D)  -> End (3D)
    //
    // Pathfinding should navigate around Broken and use Valid.

    let db = create_test_db_sentry();

    // Nodes
    let start = db
        .create_node(
            "Start",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let end = db
        .create_node(
            "End",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.0, 0.0, 1.0])
                .build(),
        )
        .unwrap();

    // Create nodes with different property name for "broken" 4D vector
    // to bypass potential index validation during creation.
    // We will tell pathfinder to use "embedding_mixed".

    let broken = db
        .create_node(
            "Broken",
            PropertyMapBuilder::new()
                .insert_vector("embedding_mixed", &[0.5, 0.5, 0.5, 0.5])
                .build(),
        )
        .unwrap();

    let valid = db
        .create_node(
            "Valid",
            PropertyMapBuilder::new()
                .insert_vector("embedding_mixed", &[0.5, 0.5, 0.0])
                .build(),
        )
        .unwrap();

    // Update Start and End to also use "embedding_mixed" using explicit transaction
    db.write(|tx| {
        tx.update_node(
            start,
            PropertyMapBuilder::new()
                .insert_vector("embedding_mixed", &[1.0, 0.0, 0.0])
                .build(),
        )
    })
    .unwrap();
    db.write(|tx| {
        tx.update_node(
            end,
            PropertyMapBuilder::new()
                .insert_vector("embedding_mixed", &[0.0, 0.0, 1.0])
                .build(),
        )
    })
    .unwrap();

    // Connect
    db.create_edge(start, broken, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(broken, end, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();

    db.create_edge(start, valid, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(valid, end, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();

    // Pathfinding
    let query = vec![1.0, 0.0, 0.0];
    let pathfinder = SemanticPathfinder::new(&db, "embedding_mixed");

    let result = pathfinder.find_path(start, end, &query, 10, false);

    match result {
        Ok(Some(p)) => {
            // If it succeeds, verify it took the valid path
            assert_eq!(p, vec![start, valid, end], "Should take valid path");
        }
        Ok(None) => panic!("Should find a path (returned None)"),
        Err(e) => {
            panic!(
                "Regression: Dimension mismatch error was not suppressed. Expected successful pathfinding skipping invalid node. Error: {}",
                e
            );
        }
    }
}
