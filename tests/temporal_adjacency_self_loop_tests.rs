//! Tests for self-loop edge handling in temporal adjacency index.

use gallifreydb::core::id::{EdgeId, NodeId};
use gallifreydb::core::interning::InternedString;
use gallifreydb::core::temporal::{TIMESTAMP_MAX, time};
use gallifreydb::index::temporal_adjacency::{TemporalAdjacencyConfig, TemporalAdjacencyIndex};
use std::thread;
use std::time::Duration;

#[test]
fn test_insert_self_loop() {
    let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
    let node = NodeId::new(1).unwrap();
    let edge = EdgeId::new(1).unwrap();
    let label = InternedString::from_raw(1);

    let t0 = time::now();
    thread::sleep(Duration::from_millis(10));
    let t1 = time::now();

    // Insert self-loop A -> A
    index
        .insert_edge(edge, node, node, label, t0, t1, t0, TIMESTAMP_MAX)
        .unwrap();

    // Query both directions - should return the edge
    let outgoing = index.get_outgoing_at_time(node, t0, t0);
    assert_eq!(outgoing, vec![edge], "Self-loop should appear in outgoing");

    let incoming = index.get_incoming_at_time(node, t0, t0);
    assert_eq!(incoming, vec![edge], "Self-loop should appear in incoming");
}

#[test]
fn test_self_loop_temporal_ranges() {
    let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
    let node = NodeId::new(1).unwrap();
    let edge = EdgeId::new(1).unwrap();
    let label = InternedString::from_raw(1);

    let t0 = time::now();
    thread::sleep(Duration::from_millis(10));
    let t1 = time::now();
    thread::sleep(Duration::from_millis(10));
    let t2 = time::now();

    // Insert self-loop valid from t0 to t2
    index
        .insert_edge(edge, node, node, label, t0, t2, t0, TIMESTAMP_MAX)
        .unwrap();

    // Query at t1 (within range) - should find edge
    let outgoing_t1 = index.get_outgoing_at_time(node, t1, t1);
    assert_eq!(outgoing_t1, vec![edge]);

    // Query at t2 (at boundary) - should NOT find edge (exclusive end)
    let outgoing_t2 = index.get_outgoing_at_time(node, t2, t2);
    assert_eq!(outgoing_t2, vec![], "Should not find edge at boundary");

    // Close valid time and query again
    index.close_edge_valid_time(edge, node, node, t1);
    let outgoing_after_close = index.get_outgoing_at_time(node, t1, t1);
    assert_eq!(
        outgoing_after_close,
        vec![],
        "Should not find edge after valid time closed"
    );
}

#[test]
fn test_self_loop_capacity_limits() {
    // Create index with very low limit for testing
    let config = TemporalAdjacencyConfig {
        max_entries_per_node: 3, // Allow only 3 entries total
    };
    let index = TemporalAdjacencyIndex::new(config);
    let node = NodeId::new(1).unwrap();
    let label = InternedString::from_raw(1);

    let t0 = time::now();

    // Insert first self-loop (adds 1 to outgoing, 1 to incoming)
    let edge1 = EdgeId::new(1).unwrap();
    index
        .insert_edge(
            edge1,
            node,
            node,
            label,
            t0,
            TIMESTAMP_MAX,
            t0,
            TIMESTAMP_MAX,
        )
        .unwrap();

    // Insert second self-loop (would add 1 more to each, total 2 per index)
    thread::sleep(Duration::from_millis(10));
    let t1 = time::now();
    let edge2 = EdgeId::new(2).unwrap();
    index
        .insert_edge(
            edge2,
            node,
            node,
            label,
            t1,
            TIMESTAMP_MAX,
            t0,
            TIMESTAMP_MAX,
        )
        .unwrap();

    // Try third self-loop (would add 1 more to each, total 3 per index)
    thread::sleep(Duration::from_millis(10));
    let t2 = time::now();
    let edge3 = EdgeId::new(3).unwrap();
    index
        .insert_edge(
            edge3,
            node,
            node,
            label,
            t2,
            TIMESTAMP_MAX,
            t0,
            TIMESTAMP_MAX,
        )
        .unwrap();

    // Try fourth self-loop (should fail - outgoing would have 4 entries)
    thread::sleep(Duration::from_millis(10));
    let t3 = time::now();
    let edge4 = EdgeId::new(4).unwrap();
    let result = index.insert_edge(
        edge4,
        node,
        node,
        label,
        t3,
        TIMESTAMP_MAX,
        t0,
        TIMESTAMP_MAX,
    );

    assert!(result.is_err(), "Should reject edge exceeding capacity");
    if let Err(e) = result {
        assert!(
            format!("{}", e).contains("Capacity exceeded"),
            "Should be capacity error, got: {}",
            e
        );
    }
}

#[test]
fn test_self_loop_transaction_time_closure() {
    let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
    let node = NodeId::new(1).unwrap();
    let edge = EdgeId::new(1).unwrap();
    let label = InternedString::from_raw(1);

    let t0 = time::now();
    thread::sleep(Duration::from_millis(10));
    let t1 = time::now();
    thread::sleep(Duration::from_millis(10));
    let t2 = time::now();

    // Insert self-loop
    index
        .insert_edge(edge, node, node, label, t0, t1, t0, TIMESTAMP_MAX)
        .unwrap();

    // Verify visible at t0
    assert_eq!(index.get_outgoing_at_time(node, t0, t0), vec![edge]);

    // Close transaction time at t1
    index.close_edge_transaction_time(edge, node, node, t1);

    // Should be visible at t0 (within tx range)
    assert_eq!(index.get_outgoing_at_time(node, t0, t0), vec![edge]);

    // Should NOT be visible at t2 (after tx close)
    assert_eq!(
        index.get_outgoing_at_time(node, t0, t2),
        vec![],
        "Should not be visible after transaction time closed"
    );
}

#[test]
fn test_self_loop_multiple_versions() {
    let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
    let node = NodeId::new(1).unwrap();
    let edge = EdgeId::new(1).unwrap();
    let label = InternedString::from_raw(1);

    let t0 = time::now();
    thread::sleep(Duration::from_millis(10));
    let t1 = time::now();
    thread::sleep(Duration::from_millis(10));
    let t2 = time::now();

    // Insert first version (t0 to t1)
    index
        .insert_edge(edge, node, node, label, t0, t1, t0, TIMESTAMP_MAX)
        .unwrap();

    // Insert second version (t1 to t2)
    index
        .insert_edge(edge, node, node, label, t1, t2, t0, TIMESTAMP_MAX)
        .unwrap();

    // Query at t0 - should find edge (first version)
    let result_t0 = index.get_outgoing_at_time(node, t0, t0);
    assert_eq!(result_t0, vec![edge], "Should find first version at t0");

    // Query at t1 - should find edge (second version, deduplicated)
    let result_t1 = index.get_outgoing_at_time(node, t1, t1);
    assert_eq!(
        result_t1,
        vec![edge],
        "Should find second version at t1 (deduplicated)"
    );

    // Query at t2 - should NOT find edge (outside all ranges)
    let result_t2 = index.get_outgoing_at_time(node, t2, t2);
    assert_eq!(
        result_t2,
        vec![],
        "Should not find edge outside all version ranges"
    );
}
