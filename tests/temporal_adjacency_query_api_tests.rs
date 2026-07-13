//! Test suite for temporal adjacency query API integration with HistoricalStorage.
//!
//! These tests verify that HistoricalStorage exposes public methods for querying
//! edges at specific points in time using the Temporal Adjacency Index.

use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use aletheiadb::core::{EdgeId, InternedString, NodeId, VersionId};
use aletheiadb::index::temporal_adjacency::{TemporalAdjacencyConfig, TemporalAdjacencyIndex};
use aletheiadb::storage::historical::HistoricalStorage;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Test that get_outgoing_edges_at_time returns edges valid at the query time.
#[test]
fn test_get_outgoing_edges_at_time_returns_valid_edges() {
    let mut storage = HistoricalStorage::new();
    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));
    storage.set_temporal_adjacency_index(index.clone());

    let source = NodeId::new(100).unwrap();
    let target1 = NodeId::new(101).unwrap();
    let target2 = NodeId::new(102).unwrap();
    let edge1 = EdgeId::new(1).unwrap();
    let edge2 = EdgeId::new(2).unwrap();
    let label = InternedString::from_raw(1);

    let t1 = time::now();

    // Add edge1 at t1
    storage
        .add_edge_version(
            edge1,
            VersionId::new(1).unwrap(),
            t1,
            t1,
            label,
            source,
            target1,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    // Wait and create edge2 at t2
    thread::sleep(Duration::from_millis(10));
    let t2 = time::now();
    storage
        .add_edge_version(
            edge2,
            VersionId::new(1).unwrap(),
            t2,
            t2,
            label,
            source,
            target2,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    // Query at t1: should see edge1 only
    let edges_at_t1 = storage.get_outgoing_edges_at_time(source, t1, t1);
    assert_eq!(edges_at_t1.len(), 1);
    assert!(edges_at_t1.contains(&edge1));

    // Query at t2: should see both edges
    let edges_at_t2 = storage.get_outgoing_edges_at_time(source, t2, t2);
    assert_eq!(edges_at_t2.len(), 2);
    assert!(edges_at_t2.contains(&edge1));
    assert!(edges_at_t2.contains(&edge2));
}

/// Test that get_incoming_edges_at_time returns edges valid at the query time.
#[test]
fn test_get_incoming_edges_at_time_returns_valid_edges() {
    let mut storage = HistoricalStorage::new();
    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));
    storage.set_temporal_adjacency_index(index.clone());

    let target = NodeId::new(200).unwrap();
    let source1 = NodeId::new(201).unwrap();
    let source2 = NodeId::new(202).unwrap();
    let edge1 = EdgeId::new(10).unwrap();
    let edge2 = EdgeId::new(11).unwrap();
    let label = InternedString::from_raw(2);

    let t1 = time::now();

    // Add edge1 at t1
    storage
        .add_edge_version(
            edge1,
            VersionId::new(1).unwrap(),
            t1,
            t1,
            label,
            source1,
            target,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    // Wait and create edge2 at t2
    thread::sleep(Duration::from_millis(10));
    let t2 = time::now();
    storage
        .add_edge_version(
            edge2,
            VersionId::new(1).unwrap(),
            t2,
            t2,
            label,
            source2,
            target,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    // Query at t1: should see edge1 only
    let edges_at_t1 = storage.get_incoming_edges_at_time(target, t1, t1);
    assert_eq!(edges_at_t1.len(), 1);
    assert!(edges_at_t1.contains(&edge1));

    // Query at t2: should see both edges
    let edges_at_t2 = storage.get_incoming_edges_at_time(target, t2, t2);
    assert_eq!(edges_at_t2.len(), 2);
    assert!(edges_at_t2.contains(&edge1));
    assert!(edges_at_t2.contains(&edge2));
}

/// Test that deleted edges are NOT returned when querying AFTER deletion time.
#[test]
fn test_deleted_edges_not_returned_after_deletion() {
    let mut storage = HistoricalStorage::new();
    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));
    storage.set_temporal_adjacency_index(index.clone());

    let source = NodeId::new(300).unwrap();
    let target = NodeId::new(301).unwrap();
    let edge = EdgeId::new(20).unwrap();
    let label = InternedString::from_raw(3);

    let t_create = time::now();

    // Create edge at t_create
    storage
        .add_edge_version(
            edge,
            VersionId::new(1).unwrap(),
            t_create,
            t_create,
            label,
            source,
            target,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    // Wait and delete edge at t_delete
    thread::sleep(Duration::from_millis(10));
    let t_delete = time::now();
    storage
        .add_edge_version(
            edge,
            VersionId::new(2).unwrap(),
            t_delete,
            t_delete,
            label,
            source,
            target,
            PropertyMap::default(),
            true, // tombstone
        )
        .unwrap();

    // Wait and query after deletion
    thread::sleep(Duration::from_millis(10));
    let t_after = time::now();

    // Query at t_create: edge should exist
    let edges_at_create = storage.get_outgoing_edges_at_time(source, t_create, t_create);
    assert_eq!(edges_at_create.len(), 1);
    assert!(edges_at_create.contains(&edge));

    // Query at t_after: edge should NOT exist
    let edges_at_after = storage.get_outgoing_edges_at_time(source, t_after, t_after);
    assert_eq!(edges_at_after.len(), 0);
}

/// Test that deleted edges ARE returned when querying BEFORE deletion time.
#[test]
fn test_deleted_edges_returned_before_deletion() {
    let mut storage = HistoricalStorage::new();
    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));
    storage.set_temporal_adjacency_index(index.clone());

    let source = NodeId::new(400).unwrap();
    let target = NodeId::new(401).unwrap();
    let edge = EdgeId::new(30).unwrap();
    let label = InternedString::from_raw(4);

    let t_create = time::now();

    // Create edge at t_create
    storage
        .add_edge_version(
            edge,
            VersionId::new(1).unwrap(),
            t_create,
            t_create,
            label,
            source,
            target,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    // Record time between create and delete
    thread::sleep(Duration::from_millis(10));
    let t_before_delete = time::now();

    // Wait and delete edge
    thread::sleep(Duration::from_millis(10));
    let t_delete = time::now();
    storage
        .add_edge_version(
            edge,
            VersionId::new(2).unwrap(),
            t_delete,
            t_delete,
            label,
            source,
            target,
            PropertyMap::default(),
            true, // tombstone
        )
        .unwrap();

    // Query at t_before_delete: edge should still exist
    let edges = storage.get_outgoing_edges_at_time(source, t_before_delete, t_before_delete);
    assert_eq!(edges.len(), 1);
    assert!(edges.contains(&edge));
}

/// #3504 affirmative snapshot-isolation regression guard for the temporal
/// adjacency index (hunk 2 of the fix).
///
/// The pre-existing "before deletion" tests query with valid == tx, so they
/// pass under the old in-place valid-close too and do not distinguish the fix.
/// This test exercises the case the fix exists for: a bi-temporal AS-OF
/// adjacency read at a valid coordinate AT/AFTER the delete's `valid_from`,
/// but at a transaction coordinate BEFORE the delete was committed. Under
/// snapshot isolation such a reader must STILL observe the since-deleted edge.
///
/// Timeline (deterministic, valid and tx coordinates deliberately diverge):
///   create edge:  valid_from = t0,     tx = T0
///   delete edge:  valid_from = t_del,  tx = T_del   (t0 < t_del, T0 < T_del)
///   read snapshot: valid = t_del,      tx = t_mid    (T0 < t_mid < T_del)
///
/// #3504: under the removed in-place valid-close (the full pre-fix behavior),
/// `close_previous_version_intervals` shrank the superseded entry's valid_to to
/// t_del and the adjacency index mirrored that with `close_edge_valid_time`, so
/// the read below returned 0 -- a node/edge alive at the snapshot vanished
/// (valid-dimension snapshot-isolation violation). The fix keeps the valid
/// interval open and instead tx-closes the adjacency entry, so the earlier-tx
/// reader still sees the edge while a current-tx reader does not.
#[test]
fn deleted_edge_still_traversable_at_earlier_tx_snapshot_after_valid_from() {
    let mut storage = HistoricalStorage::new();
    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));
    storage.set_temporal_adjacency_index(index.clone());

    let source = NodeId::new(500).unwrap();
    let target = NodeId::new(501).unwrap();
    let edge = EdgeId::new(40).unwrap();
    let label = InternedString::from_raw(5);

    // Deterministic bi-temporal coordinates so valid/tx diverge and t0 < t_del,
    // T0 < t_mid < T_del hold exactly (no wallclock/sleep flakiness).
    let t0 = time::from_secs(1_000_000);
    let big_t0 = time::from_secs(1_000_000);
    let t_mid = time::from_secs(1_000_100);
    let t_del = time::from_secs(1_000_200);
    let big_t_del = time::from_secs(1_000_200);
    let after_del = time::from_secs(1_000_300);

    // Create the edge at (valid = t0, tx = T0). Source/target nodes stay alive.
    storage
        .add_edge_version(
            edge,
            VersionId::new(1).unwrap(),
            t0,
            big_t0,
            label,
            source,
            target,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    // Delete (tombstone) the EDGE at (valid = t_del, tx = T_del), strictly after
    // both the create's valid and tx coordinates.
    storage
        .add_edge_version(
            edge,
            VersionId::new(2).unwrap(),
            t_del,
            big_t_del,
            label,
            source,
            target,
            PropertyMap::default(),
            true, // tombstone
        )
        .unwrap();

    // Core #3504 assertion: an earlier-tx snapshot (T0 < t_mid < T_del) reading
    // at a valid coordinate at/after the delete's valid_from must STILL see the
    // edge. Under the removed valid-close this returned 0.
    let out_earlier_tx = storage.get_outgoing_edges_at_time(source, t_del, t_mid);
    assert_eq!(
        out_earlier_tx.len(),
        1,
        "deleted edge must remain traversable outgoing at an earlier-tx snapshot"
    );
    assert!(out_earlier_tx.contains(&edge));

    // Symmetric incoming direction.
    let in_earlier_tx = storage.get_incoming_edges_at_time(target, t_del, t_mid);
    assert_eq!(
        in_earlier_tx.len(),
        1,
        "deleted edge must remain traversable incoming at an earlier-tx snapshot"
    );
    assert!(in_earlier_tx.contains(&edge));

    // Guard against over-correction: at/after the delete's commit tx, the edge
    // must be invisible (current-state traversal must not resurrect it). This is
    // exactly what hunk 2's tx-close of the adjacency entry provides; reverting
    // hunk 2 makes these return 1.
    let out_at_del_tx = storage.get_outgoing_edges_at_time(source, t_del, big_t_del);
    assert_eq!(
        out_at_del_tx.len(),
        0,
        "deleted edge must be invisible at the delete's own commit tx"
    );
    let out_after_del_tx = storage.get_outgoing_edges_at_time(source, t_del, after_del);
    assert_eq!(
        out_after_del_tx.len(),
        0,
        "deleted edge must be invisible after the delete's commit tx"
    );
    let in_after_del_tx = storage.get_incoming_edges_at_time(target, t_del, after_del);
    assert_eq!(
        in_after_del_tx.len(),
        0,
        "deleted edge must be invisible incoming after the delete's commit tx"
    );
}

/// Test that methods return empty vectors when index is not set.
#[test]
fn test_query_methods_return_empty_without_index() {
    let storage = HistoricalStorage::new();
    let node = NodeId::new(500).unwrap();
    let t = time::now();

    // Without index, should return empty vectors
    let outgoing = storage.get_outgoing_edges_at_time(node, t, t);
    assert_eq!(outgoing.len(), 0);

    let incoming = storage.get_incoming_edges_at_time(node, t, t);
    assert_eq!(incoming.len(), 0);
}

/// Test with label filtering.
#[test]
fn test_query_with_label_filter() {
    let mut storage = HistoricalStorage::new();
    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));
    storage.set_temporal_adjacency_index(index.clone());

    let source = NodeId::new(600).unwrap();
    let target1 = NodeId::new(601).unwrap();
    let target2 = NodeId::new(602).unwrap();
    let edge1 = EdgeId::new(40).unwrap();
    let edge2 = EdgeId::new(41).unwrap();
    let label_knows = InternedString::from_raw(10);
    let label_likes = InternedString::from_raw(11);

    let t = time::now();

    // Add edges with different labels
    storage
        .add_edge_version(
            edge1,
            VersionId::new(1).unwrap(),
            t,
            t,
            label_knows,
            source,
            target1,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    storage
        .add_edge_version(
            edge2,
            VersionId::new(1).unwrap(),
            t,
            t,
            label_likes,
            source,
            target2,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    // Query with label filter (assumes API supports it)
    // For now, test that both edges are returned and can be filtered by caller
    let all_edges = storage.get_outgoing_edges_at_time(source, t, t);
    assert_eq!(all_edges.len(), 2);
}
