//! End-to-end test for semantic pathfinder with temporal adjacency index.
//!
//! This test verifies that SemanticPathfinder can find paths through edges
//! that have been deleted from current storage, by using the temporal
//! adjacency index to query historical edge connectivity.

use gallifreydb::GallifreyDB;
use gallifreydb::api::transaction::WriteOps;
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::core::temporal::time;
use gallifreydb::experimental::semantic_pathfinding::SemanticPathfinder;
use gallifreydb::index::vector::{DistanceMetric, HnswConfig};
use std::thread;
use std::time::Duration;

#[test]
fn test_pathfinder_finds_path_through_deleted_edges() {
    let db = GallifreyDB::new().unwrap();

    // Enable vector index for semantic pathfinding
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(3, DistanceMetric::Cosine))
        .enable()
        .unwrap();

    // Create path: start -> middle -> end
    let start = db
        .create_node(
            "Start",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.5, 0.5, 0.0])
                .build(),
        )
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
        .create_node(
            "End",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.5, 0.5, 0.0])
                .build(),
        )
        .unwrap();

    // Record time before creating edges
    let t_before_edges = time::now();
    thread::sleep(Duration::from_millis(10));

    // Create edges at t0
    let (_, t0) = db
        .write_with_timestamp(|tx| {
            tx.create_edge(start, middle, "NEXT", PropertyMapBuilder::new().build())?;
            tx.create_edge(middle, end, "NEXT", PropertyMapBuilder::new().build())?;
            Ok(())
        })
        .unwrap();

    // Verify path exists at t0
    let pathfinder = SemanticPathfinder::new(&db, "embedding");
    let query = vec![1.0, 0.0, 0.0];

    let path_at_t0 = pathfinder
        .find_path_at_time(start, end, &query, t0)
        .unwrap();
    assert!(
        path_at_t0.is_some(),
        "Path should exist at t0 when edges exist"
    );
    assert_eq!(path_at_t0.as_ref().unwrap().len(), 3);
    assert_eq!(path_at_t0.as_ref().unwrap()[0], start);
    assert_eq!(path_at_t0.as_ref().unwrap()[1], middle);
    assert_eq!(path_at_t0.as_ref().unwrap()[2], end);

    // Wait and delete middle node (which deletes its edges)
    thread::sleep(Duration::from_millis(10));
    let (_, _t_delete) = db
        .write_with_timestamp(|tx| tx.delete_node_cascade(middle))
        .unwrap();

    // CRITICAL TEST: Query at t0 AFTER deletion
    // With temporal adjacency index, this should STILL find the path
    // because the edges existed at t0 even though they're deleted now
    let path_at_t0_after_deletion = pathfinder
        .find_path_at_time(start, end, &query, t0)
        .unwrap();

    assert!(
        path_at_t0_after_deletion.is_some(),
        "Path should be found at t0 even after edges are deleted, using temporal adjacency index"
    );
    assert_eq!(path_at_t0_after_deletion.as_ref().unwrap().len(), 3);
    assert_eq!(path_at_t0_after_deletion.as_ref().unwrap()[0], start);
    assert_eq!(path_at_t0_after_deletion.as_ref().unwrap()[1], middle);
    assert_eq!(path_at_t0_after_deletion.as_ref().unwrap()[2], end);

    // Verify no path exists BEFORE edges were created
    let path_before = pathfinder
        .find_path_at_time(start, end, &query, t_before_edges)
        .unwrap();
    assert!(
        path_before.is_none(),
        "Path should not exist before edges were created"
    );
}

#[test]
fn test_pathfinder_respects_edge_lifetimes() {
    let db = GallifreyDB::new().unwrap();

    db.vector_index("embedding")
        .hnsw(HnswConfig::new(3, DistanceMetric::Cosine))
        .enable()
        .unwrap();

    // Create nodes
    let start = db
        .create_node(
            "Start",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0, 0.0])
                .build(),
        )
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
        .create_node(
            "End",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    // Create first path at t1
    thread::sleep(Duration::from_millis(10));
    let (_, t1) = db
        .write_with_timestamp(|tx| {
            tx.create_edge(start, middle, "PATH1", PropertyMapBuilder::new().build())?;
            tx.create_edge(middle, end, "PATH1", PropertyMapBuilder::new().build())?;
            Ok(())
        })
        .unwrap();

    // Delete first path at t2
    thread::sleep(Duration::from_millis(10));
    let (_, t2) = db
        .write_with_timestamp(|tx| tx.delete_node_cascade(middle))
        .unwrap();

    // Create new middle node and path at t3
    thread::sleep(Duration::from_millis(10));
    let middle2 = db
        .create_node(
            "Middle2",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let (_, t3) = db
        .write_with_timestamp(|tx| {
            tx.create_edge(start, middle2, "PATH2", PropertyMapBuilder::new().build())?;
            tx.create_edge(middle2, end, "PATH2", PropertyMapBuilder::new().build())?;
            Ok(())
        })
        .unwrap();

    let pathfinder = SemanticPathfinder::new(&db, "embedding");
    let query = vec![1.0, 0.0, 0.0];

    // At t1: first path exists
    let path_t1 = pathfinder
        .find_path_at_time(start, end, &query, t1)
        .unwrap();
    assert!(path_t1.is_some(), "Path through middle should exist at t1");
    assert_eq!(path_t1.as_ref().unwrap()[1], middle);

    // At t2: no path (first deleted, second not created yet)
    let path_t2 = pathfinder
        .find_path_at_time(start, end, &query, t2)
        .unwrap();
    assert!(path_t2.is_none(), "No path should exist at t2");

    // At t3: second path exists
    let path_t3 = pathfinder
        .find_path_at_time(start, end, &query, t3)
        .unwrap();
    assert!(path_t3.is_some(), "Path through middle2 should exist at t3");
    assert_eq!(path_t3.as_ref().unwrap()[1], middle2);
}
