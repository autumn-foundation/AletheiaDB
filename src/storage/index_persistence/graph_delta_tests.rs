use super::*;
use crate::storage::index_persistence::formats::{
    PersistedEdge, PersistedNode, PersistedPropertyMap,
};
use tempfile::tempdir;

fn create_test_node(id: u64, label_idx: u32, version_id: u64) -> PersistedNode {
    PersistedNode {
        id,
        label_idx,
        version_id,
        properties: PersistedPropertyMap::default(),
    }
}

fn create_test_edge(
    id: u64,
    source_id: u64,
    target_id: u64,
    label_idx: u32,
    version_id: u64,
) -> PersistedEdge {
    PersistedEdge {
        id,
        source_id,
        target_id,
        label_idx,
        version_id,
        properties: PersistedPropertyMap::default(),
    }
}

#[test]
fn test_delta_additions() {
    let dir = tempdir().unwrap();
    let base_path = dir.path().join("base.idx");
    let delta_path = dir.path().join("delta.idx");

    // Base state: 1 node
    let mut base_data = new_graph_index_data();
    base_data.nodes.push(create_test_node(1, 1, 100));
    base_data.node_count = 1;
    save_graph_index(&base_data, &base_path).unwrap();

    // Modified state: 2 nodes
    let mut modified_data = base_data.clone();
    modified_data.nodes.push(create_test_node(2, 2, 101));
    modified_data.node_count = 2;

    // Save delta
    save_graph_index_delta(&base_data, &modified_data, &delta_path, 3).unwrap();

    // Load with delta
    let loaded = load_graph_index_with_delta(&base_path, &delta_path).unwrap();

    assert_eq!(loaded.node_count, 2);
    assert_eq!(loaded.nodes.len(), 2);
    assert!(loaded.nodes.iter().any(|n| n.id == 1));
    assert!(loaded.nodes.iter().any(|n| n.id == 2));
}

#[test]
fn test_delta_deletions() {
    let dir = tempdir().unwrap();
    let base_path = dir.path().join("base.idx");
    let delta_path = dir.path().join("delta.idx");

    // Base state: 2 nodes
    let mut base_data = new_graph_index_data();
    base_data.nodes.push(create_test_node(1, 1, 100));
    base_data.nodes.push(create_test_node(2, 2, 101));
    base_data.node_count = 2;
    save_graph_index(&base_data, &base_path).unwrap();

    // Modified state: 1 node (node 2 deleted)
    let mut modified_data = new_graph_index_data();
    modified_data.nodes.push(create_test_node(1, 1, 100));
    modified_data.node_count = 1;

    // Save delta
    save_graph_index_delta(&base_data, &modified_data, &delta_path, 3).unwrap();

    // Load with delta
    let loaded = load_graph_index_with_delta(&base_path, &delta_path).unwrap();

    assert_eq!(loaded.node_count, 1);
    assert_eq!(loaded.nodes.len(), 1);
    assert!(loaded.nodes.iter().any(|n| n.id == 1));
    assert!(!loaded.nodes.iter().any(|n| n.id == 2));
}

#[test]
fn test_delta_modifications() {
    let dir = tempdir().unwrap();
    let base_path = dir.path().join("base.idx");
    let delta_path = dir.path().join("delta.idx");

    // Base state
    let mut base_data = new_graph_index_data();
    base_data.nodes.push(create_test_node(1, 1, 100));
    base_data.node_count = 1;
    save_graph_index(&base_data, &base_path).unwrap();

    // Modified state: node 1 version changed
    let mut modified_data = new_graph_index_data();
    modified_data.nodes.push(create_test_node(1, 1, 102)); // Changed version
    modified_data.node_count = 1;

    // Save delta
    save_graph_index_delta(&base_data, &modified_data, &delta_path, 3).unwrap();

    // Load with delta
    let loaded = load_graph_index_with_delta(&base_path, &delta_path).unwrap();

    assert_eq!(loaded.node_count, 1);
    assert_eq!(loaded.nodes[0].version_id, 102);
}

#[test]
fn test_delta_mixed_operations() {
    let dir = tempdir().unwrap();
    let base_path = dir.path().join("base.idx");
    let delta_path = dir.path().join("delta.idx");

    // Base: Nodes 1, 2, 3. Edges 1 (1->2)
    let mut base_data = new_graph_index_data();
    base_data.nodes.push(create_test_node(1, 1, 100));
    base_data.nodes.push(create_test_node(2, 2, 100));
    base_data.nodes.push(create_test_node(3, 3, 100));
    base_data.edges.push(create_test_edge(1, 1, 2, 10, 200));
    base_data.node_count = 3;
    base_data.edge_count = 1;
    save_graph_index(&base_data, &base_path).unwrap();

    // Modified:
    // - Node 1 modified (ver 101)
    // - Node 2 deleted
    // - Node 3 kept
    // - Node 4 added
    // - Edge 1 deleted
    // - Edge 2 added (1->3)
    let mut modified_data = new_graph_index_data();
    modified_data.nodes.push(create_test_node(1, 1, 101));
    modified_data.nodes.push(create_test_node(3, 3, 100));
    modified_data.nodes.push(create_test_node(4, 4, 100));
    modified_data.edges.push(create_test_edge(2, 1, 3, 11, 201));
    modified_data.node_count = 3;
    modified_data.edge_count = 1;

    // Save delta
    save_graph_index_delta(&base_data, &modified_data, &delta_path, 3).unwrap();

    // Load with delta
    let loaded = load_graph_index_with_delta(&base_path, &delta_path).unwrap();

    assert_eq!(loaded.node_count, 3);
    assert_eq!(loaded.nodes.len(), 3);
    assert_eq!(loaded.edge_count, 1);
    assert_eq!(loaded.edges.len(), 1);

    // Verify content
    let node1 = loaded.nodes.iter().find(|n| n.id == 1).unwrap();
    assert_eq!(node1.version_id, 101);

    assert!(!loaded.nodes.iter().any(|n| n.id == 2));

    let node3 = loaded.nodes.iter().find(|n| n.id == 3).unwrap();
    assert_eq!(node3.version_id, 100);

    let node4 = loaded.nodes.iter().find(|n| n.id == 4).unwrap();
    assert_eq!(node4.id, 4);

    let edge2 = loaded.edges.iter().find(|e| e.id == 2).unwrap();
    assert_eq!(edge2.source_id, 1);
    assert_eq!(edge2.target_id, 3);

    assert!(!loaded.edges.iter().any(|e| e.id == 1));
}

#[test]
fn test_delta_empty() {
    let dir = tempdir().unwrap();
    let base_path = dir.path().join("base.idx");
    let delta_path = dir.path().join("delta.idx");

    let mut base_data = new_graph_index_data();
    base_data.nodes.push(create_test_node(1, 1, 100));
    base_data.node_count = 1;
    save_graph_index(&base_data, &base_path).unwrap();

    // Modified is identical to base
    let modified_data = base_data.clone();

    save_graph_index_delta(&base_data, &modified_data, &delta_path, 3).unwrap();
    let loaded = load_graph_index_with_delta(&base_path, &delta_path).unwrap();

    assert_eq!(loaded.nodes.len(), 1);
    assert_eq!(loaded.nodes[0], base_data.nodes[0]);
}

#[test]
fn test_load_mmap_valid() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.idx");

    let mut data = new_graph_index_data();
    data.nodes.push(create_test_node(1, 1, 100));
    data.node_count = 1;

    save_graph_index(&data, &path).unwrap();

    let loaded = load_graph_index_mmap(&path).unwrap();
    assert_eq!(loaded.nodes.len(), 1);
    assert_eq!(loaded.nodes[0].id, 1);
}

#[test]
fn test_load_mmap_corrupted() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.idx");

    // Create valid file then corrupt it
    let mut data = new_graph_index_data();
    data.nodes.push(create_test_node(1, 1, 100));
    save_graph_index(&data, &path).unwrap();

    let mut bytes = std::fs::read(&path).unwrap();
    // Flip bits in the middle of data (before CRC)
    if bytes.len() > 20 {
        bytes[10] ^= 0xFF;
    }
    std::fs::write(&path, &bytes).unwrap();

    let result = load_graph_index_mmap(&path);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("CRC mismatch") || err.to_string().contains("corrupted"));
}

#[test]
fn test_load_mmap_truncated() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.idx");

    std::fs::write(&path, b"short").unwrap();

    let result = load_graph_index_mmap(&path);
    assert!(result.is_err());
}

#[test]
fn test_load_mmap_empty() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.idx");

    std::fs::write(&path, b"").unwrap();

    let result = load_graph_index_mmap(&path);
    assert!(result.is_err());
}
