//! Test suite for temporal adjacency query API integration with HistoricalStorage.
//!
//! These tests verify that HistoricalStorage exposes public methods for querying
//! edges at specific points in time using the Temporal Adjacency Index.

use gallifreydb::core::property::PropertyMap;
use gallifreydb::core::temporal::time;
use gallifreydb::core::{EdgeId, InternedString, NodeId, VersionId};
use gallifreydb::index::temporal_adjacency::{TemporalAdjacencyConfig, TemporalAdjacencyIndex};
use gallifreydb::storage::historical::HistoricalStorage;
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
