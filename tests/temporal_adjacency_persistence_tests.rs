// Tests for temporal adjacency index persistence

use gallifreydb::core::temporal::time;
use gallifreydb::core::{EdgeId, InternedString, NodeId, TIMESTAMP_MAX};
use gallifreydb::index::temporal_adjacency::{TemporalAdjacencyConfig, TemporalAdjacencyIndex};
use std::sync::Arc;
use tempfile::TempDir;

#[test]
fn test_save_and_load_empty_index() {
    let temp_dir = TempDir::new().unwrap();
    let data_dir = temp_dir.path().to_path_buf();

    // Create empty index
    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));

    // Save to disk
    gallifreydb::storage::index_persistence::temporal_adjacency::save_temporal_adjacency_index(
        &index, &data_dir,
    )
    .unwrap();

    // Load from disk
    let loaded_index =
        gallifreydb::storage::index_persistence::temporal_adjacency::load_temporal_adjacency_index(
            &data_dir,
        )
        .unwrap();

    // Verify empty
    let t = time::now();
    let edges = loaded_index.get_outgoing_at_time(NodeId::new(1).unwrap(), t, t);
    assert_eq!(edges.len(), 0);
}

#[test]
fn test_save_and_load_index_with_entries() {
    let temp_dir = TempDir::new().unwrap();
    let data_dir = temp_dir.path().to_path_buf();

    // Create index with entries
    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));

    let edge1 = EdgeId::new(1).unwrap();
    let edge2 = EdgeId::new(2).unwrap();
    let source = NodeId::new(100).unwrap();
    let target1 = NodeId::new(200).unwrap();
    let target2 = NodeId::new(300).unwrap();
    let label = InternedString::from_raw(1);
    let t = time::now();

    index
        .insert_edge(
            edge1,
            source,
            target1,
            label,
            t,
            TIMESTAMP_MAX,
            t,
            TIMESTAMP_MAX,
        )
        .unwrap();
    index
        .insert_edge(
            edge2,
            source,
            target2,
            label,
            t,
            TIMESTAMP_MAX,
            t,
            TIMESTAMP_MAX,
        )
        .unwrap();

    // Save to disk
    gallifreydb::storage::index_persistence::temporal_adjacency::save_temporal_adjacency_index(
        &index, &data_dir,
    )
    .unwrap();

    // Load from disk
    let loaded_index =
        gallifreydb::storage::index_persistence::temporal_adjacency::load_temporal_adjacency_index(
            &data_dir,
        )
        .unwrap();

    // Verify entries were preserved
    let edges = loaded_index.get_outgoing_at_time(source, t, t);
    assert_eq!(edges.len(), 2);
    assert!(edges.contains(&edge1));
    assert!(edges.contains(&edge2));

    // Verify incoming edges
    let incoming1 = loaded_index.get_incoming_at_time(target1, t, t);
    assert_eq!(incoming1.len(), 1);
    assert_eq!(incoming1[0], edge1);
}

#[test]
fn test_save_and_load_preserves_temporal_ranges() {
    let temp_dir = TempDir::new().unwrap();
    let data_dir = temp_dir.path().to_path_buf();

    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));

    let edge_id = EdgeId::new(1).unwrap();
    let source = NodeId::new(100).unwrap();
    let target = NodeId::new(200).unwrap();
    let label = InternedString::from_raw(1);

    let t0 = time::now();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let t1 = time::now();

    // Insert edge with limited temporal range [t0, t1)
    index
        .insert_edge(edge_id, source, target, label, t0, t1, t0, TIMESTAMP_MAX)
        .unwrap();

    // Save and load
    gallifreydb::storage::index_persistence::temporal_adjacency::save_temporal_adjacency_index(
        &index, &data_dir,
    )
    .unwrap();

    let loaded_index =
        gallifreydb::storage::index_persistence::temporal_adjacency::load_temporal_adjacency_index(
            &data_dir,
        )
        .unwrap();

    // Edge should be found at t0
    let edges_at_t0 = loaded_index.get_outgoing_at_time(source, t0, t0);
    assert_eq!(edges_at_t0.len(), 1);

    // Edge should NOT be found at t1 (temporal range ended)
    let edges_at_t1 = loaded_index.get_outgoing_at_time(source, t1, t1);
    assert_eq!(edges_at_t1.len(), 0);
}

#[test]
fn test_multiple_nodes_with_edges() {
    let temp_dir = TempDir::new().unwrap();
    let data_dir = temp_dir.path().to_path_buf();

    let index = Arc::new(TemporalAdjacencyIndex::new(
        TemporalAdjacencyConfig::default(),
    ));

    let t = time::now();
    let label = InternedString::from_raw(1);

    // Node 1 -> 2 edges
    let node1 = NodeId::new(1).unwrap();
    index
        .insert_edge(
            EdgeId::new(1).unwrap(),
            node1,
            NodeId::new(10).unwrap(),
            label,
            t,
            TIMESTAMP_MAX,
            t,
            TIMESTAMP_MAX,
        )
        .unwrap();
    index
        .insert_edge(
            EdgeId::new(2).unwrap(),
            node1,
            NodeId::new(11).unwrap(),
            label,
            t,
            TIMESTAMP_MAX,
            t,
            TIMESTAMP_MAX,
        )
        .unwrap();

    // Node 2 -> 3 edges
    let node2 = NodeId::new(2).unwrap();
    for i in 0..3 {
        index
            .insert_edge(
                EdgeId::new(10 + i).unwrap(),
                node2,
                NodeId::new(20 + i).unwrap(),
                label,
                t,
                TIMESTAMP_MAX,
                t,
                TIMESTAMP_MAX,
            )
            .unwrap();
    }

    // Save and load
    gallifreydb::storage::index_persistence::temporal_adjacency::save_temporal_adjacency_index(
        &index, &data_dir,
    )
    .unwrap();

    let loaded_index =
        gallifreydb::storage::index_persistence::temporal_adjacency::load_temporal_adjacency_index(
            &data_dir,
        )
        .unwrap();

    // Verify node1 has 2 edges
    let edges1 = loaded_index.get_outgoing_at_time(node1, t, t);
    assert_eq!(edges1.len(), 2);

    // Verify node2 has 3 edges
    let edges2 = loaded_index.get_outgoing_at_time(node2, t, t);
    assert_eq!(edges2.len(), 3);
}
