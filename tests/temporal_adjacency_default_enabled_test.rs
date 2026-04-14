//! Test that temporal adjacency index is enabled by default in AletheiaDB.
//!
//! This test verifies that users don't need to manually enable the temporal
//! adjacency index - it should work out of the box.

use aletheiadb::AletheiaDB;
use aletheiadb::api::transaction::{ReadOps, WriteOps};
use aletheiadb::core::property::PropertyMapBuilder;
use std::thread;
use std::time::Duration;

#[test]
fn test_temporal_adjacency_index_enabled_by_default() {
    // Create database with default configuration
    let db = AletheiaDB::new().unwrap();

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

    // Create edge at t0
    let (_, t0) = db
        .write_with_timestamp(|tx| {
            tx.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        })
        .unwrap();

    // Wait and delete edge at t1
    thread::sleep(Duration::from_millis(10));
    let (_edge_id, _t1) = db
        .write_with_timestamp(|tx| {
            let edges = tx.get_outgoing_edges(alice);
            tx.delete_edge(edges[0])
        })
        .unwrap();

    // CRITICAL TEST: Query for edges at t0 AFTER deletion
    // This should return the edge because temporal adjacency index
    // is enabled by default
    let edges_at_t0 = db.get_outgoing_edges_at_time(alice, t0, t0);

    assert_eq!(
        edges_at_t0.len(),
        1,
        "Temporal adjacency index should be enabled by default and find deleted edges"
    );
}

#[test]
fn test_temporal_queries_work_immediately() {
    // Create database and immediately use temporal queries
    let db = AletheiaDB::new().unwrap();

    let node_a = db
        .create_node("Node", PropertyMapBuilder::new().build())
        .unwrap();

    let node_b = db
        .create_node("Node", PropertyMapBuilder::new().build())
        .unwrap();

    let (edge_id, t_created) = db
        .write_with_timestamp(|tx| {
            tx.create_edge(node_a, node_b, "LINKS", PropertyMapBuilder::new().build())
        })
        .unwrap();

    // Should work without any setup
    let outgoing = db.get_outgoing_edges_at_time(node_a, t_created, t_created);
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0], edge_id);

    let incoming = db.get_incoming_edges_at_time(node_b, t_created, t_created);
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0], edge_id);
}

#[test]
fn test_temporal_index_survives_many_operations() {
    let db = AletheiaDB::new().unwrap();

    let source = db
        .create_node("Node", PropertyMapBuilder::new().build())
        .unwrap();

    // Create 10 edges at different times
    let mut timestamps = Vec::new();
    let mut edge_ids = Vec::new();

    for _ in 0..10 {
        thread::sleep(Duration::from_millis(5));
        let target = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();

        let (edge_id, t) = db
            .write_with_timestamp(|tx| {
                tx.create_edge(source, target, "EDGE", PropertyMapBuilder::new().build())
            })
            .unwrap();

        timestamps.push(t);
        edge_ids.push(edge_id);
    }

    // Verify temporal queries work for all time points
    for (i, &t) in timestamps.iter().enumerate() {
        let edges = db.get_outgoing_edges_at_time(source, t, t);
        // Should have i+1 edges at timestamp i (cumulative)
        assert_eq!(
            edges.len(),
            i + 1,
            "Should find {} edges at timestamp {}",
            i + 1,
            i
        );
    }
}
