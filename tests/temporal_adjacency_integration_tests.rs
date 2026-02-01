// Integration tests for TemporalAdjacencyIndex with HistoricalStorage

use gallifreydb::core::temporal::time;
use gallifreydb::core::{EdgeId, InternedString, NodeId, PropertyMap, VersionId};
use gallifreydb::index::temporal_adjacency::{TemporalAdjacencyConfig, TemporalAdjacencyIndex};
use gallifreydb::storage::historical::HistoricalStorage;
use std::sync::Arc;

#[test]
fn test_add_edge_version_updates_temporal_adjacency_index() {
    // Create historical storage and temporal adjacency index
    let mut storage = HistoricalStorage::new();
    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));

    // Set the index on storage
    storage.set_temporal_adjacency_index(index.clone());

    // Create edge version
    let edge_id = EdgeId::new(1).unwrap();
    let version_id = VersionId::new(1).unwrap();
    let source = NodeId::new(100).unwrap();
    let target = NodeId::new(200).unwrap();
    let label = InternedString::from_raw(1);
    let valid_from = time::now();
    let tx_time = time::now();

    // Add edge version
    storage
        .add_edge_version(
            edge_id,
            version_id,
            valid_from,
            tx_time,
            label,
            source,
            target,
            PropertyMap::default(),
            false, // not a tombstone
        )
        .unwrap();

    // Verify the index was updated - should find outgoing edge from source
    let outgoing = index.get_outgoing_at_time(source, valid_from, tx_time);
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0], edge_id);

    // Verify incoming edge to target
    let incoming = index.get_incoming_at_time(target, valid_from, tx_time);
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0], edge_id);
}

#[test]
fn test_tombstone_closes_temporal_adjacency_entry() {
    let mut storage = HistoricalStorage::new();
    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));
    storage.set_temporal_adjacency_index(index.clone());

    let edge_id = EdgeId::new(1).unwrap();
    let source = NodeId::new(100).unwrap();
    let target = NodeId::new(200).unwrap();
    let label = InternedString::from_raw(1);

    // Create edge at t0
    let t0 = time::now();
    storage
        .add_edge_version(
            edge_id,
            VersionId::new(1).unwrap(),
            t0,
            t0,
            label,
            source,
            target,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    // Wait and delete edge at t1 (tombstone)
    std::thread::sleep(std::time::Duration::from_millis(10));
    let t1 = time::now();
    storage
        .add_edge_version(
            edge_id,
            VersionId::new(2).unwrap(),
            t1,
            t1,
            label,
            source,
            target,
            PropertyMap::default(),
            true, // tombstone
        )
        .unwrap();

    // Edge should be found at t0
    let edges_at_t0 = index.get_outgoing_at_time(source, t0, t0);
    assert_eq!(edges_at_t0.len(), 1);

    // Edge should NOT be found at t1 (deleted)
    let edges_at_t1 = index.get_outgoing_at_time(source, t1, t1);
    assert_eq!(edges_at_t1.len(), 0);
}

#[test]
fn test_multiple_versions_track_temporal_changes() {
    let mut storage = HistoricalStorage::new();
    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));
    storage.set_temporal_adjacency_index(index.clone());

    let edge_id = EdgeId::new(1).unwrap();
    let source = NodeId::new(100).unwrap();
    let target = NodeId::new(200).unwrap();
    let label = InternedString::from_raw(1);

    // Create edge at t0
    let t0 = time::now();
    storage
        .add_edge_version(
            edge_id,
            VersionId::new(1).unwrap(),
            t0,
            t0,
            label,
            source,
            target,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    // Update edge at t1 (closes previous valid time)
    std::thread::sleep(std::time::Duration::from_millis(10));
    let t1 = time::now();
    storage
        .add_edge_version(
            edge_id,
            VersionId::new(2).unwrap(),
            t1,
            t1,
            label,
            source,
            target,
            PropertyMap::default(),
            false,
        )
        .unwrap();

    // Should find edge at both times (valid time was closed on v1, but v2 started)
    let edges_at_t0 = index.get_outgoing_at_time(source, t0, t0);
    assert_eq!(edges_at_t0.len(), 1);

    let edges_at_t1 = index.get_outgoing_at_time(source, t1, t1);
    assert_eq!(edges_at_t1.len(), 1);
}
