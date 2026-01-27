//! Tests for HLC Phase 3: Network Message Timestamp Synchronization.
//!
//! These tests verify HLC integration with network messages:
//! - NodeData/EdgeData use HybridTimestamp
//! - Prepare/Commit responses include HLC timestamps
//! - Network messages synchronize clocks via send/receive

use gallifreydb::core::hlc::HybridTimestamp;
use gallifreydb::core::id::{EdgeId, NodeId};
use gallifreydb::storage::sharding::network::{
    CommitResponse, EdgeData, MigrationBatch, NodeData, PrepareResponse,
};

/// RED PHASE TEST 1: NodeData should use HybridTimestamp for temporal fields
#[test]
fn test_node_data_uses_hybrid_timestamp() {
    let node_id = NodeId::new(1).unwrap();
    let label = "Person".to_string();
    let properties = vec![1, 2, 3, 4]; // Serialized properties

    let valid_from = HybridTimestamp::new(1_000_000, 0).unwrap();
    let valid_to = Some(HybridTimestamp::new(2_000_000, 5).unwrap());

    let node_data = NodeData {
        id: node_id,
        label: label.clone(),
        properties: properties.clone(),
        valid_from,
        valid_to,
    };

    assert_eq!(node_data.id, node_id);
    assert_eq!(node_data.label, label);
    assert_eq!(node_data.valid_from, valid_from);
    assert_eq!(node_data.valid_to, valid_to);
    assert_eq!(node_data.valid_from.wallclock(), 1_000_000);
    assert_eq!(node_data.valid_from.logical(), 0);
}

/// RED PHASE TEST 2: EdgeData should use HybridTimestamp for temporal fields
#[test]
fn test_edge_data_uses_hybrid_timestamp() {
    let edge_id = EdgeId::new(100).unwrap();
    let source = NodeId::new(1).unwrap();
    let target = NodeId::new(2).unwrap();
    let label = "KNOWS".to_string();
    let properties = vec![5, 6, 7, 8];

    let valid_from = HybridTimestamp::new(1_500_000, 3).unwrap();
    let valid_to = None; // Still valid

    let edge_data = EdgeData {
        id: edge_id,
        source,
        target,
        label: label.clone(),
        properties: properties.clone(),
        valid_from,
        valid_to,
    };

    assert_eq!(edge_data.id, edge_id);
    assert_eq!(edge_data.source, source);
    assert_eq!(edge_data.target, target);
    assert_eq!(edge_data.label, label);
    assert_eq!(edge_data.valid_from, valid_from);
    assert_eq!(edge_data.valid_to, valid_to);
    assert_eq!(edge_data.valid_from.wallclock(), 1_500_000);
    assert_eq!(edge_data.valid_from.logical(), 3);
}

/// RED PHASE TEST 3: MigrationBatch should preserve HLC timestamps
#[test]
fn test_migration_batch_preserves_hlc_timestamps() {
    let node1_ts = HybridTimestamp::new(1_000_000, 0).unwrap();
    let node2_ts = HybridTimestamp::new(1_001_000, 2).unwrap();
    let edge1_ts = HybridTimestamp::new(1_002_000, 1).unwrap();

    let nodes = vec![
        NodeData {
            id: NodeId::new(1).unwrap(),
            label: "Person".to_string(),
            properties: vec![],
            valid_from: node1_ts,
            valid_to: None,
        },
        NodeData {
            id: NodeId::new(2).unwrap(),
            label: "Person".to_string(),
            properties: vec![],
            valid_from: node2_ts,
            valid_to: Some(HybridTimestamp::new(1_005_000, 0).unwrap()),
        },
    ];

    let edges = vec![EdgeData {
        id: EdgeId::new(1).unwrap(),
        source: NodeId::new(1).unwrap(),
        target: NodeId::new(2).unwrap(),
        label: "KNOWS".to_string(),
        properties: vec![],
        valid_from: edge1_ts,
        valid_to: None,
    }];

    let batch = MigrationBatch {
        migration_id: 1,
        batch_number: 0,
        is_last: true,
        nodes,
        edges,
        checksum: 12345,
    };

    // Verify HLC timestamps are preserved
    assert_eq!(batch.nodes[0].valid_from, node1_ts);
    assert_eq!(batch.nodes[1].valid_from, node2_ts);
    assert_eq!(batch.edges[0].valid_from, edge1_ts);
}

/// RED PHASE TEST 4: PrepareResponse should include HLC timestamp
#[test]
fn test_prepare_response_includes_hlc_timestamp() {
    let shard_ts = HybridTimestamp::new(1_234_567, 42).unwrap();

    let response = PrepareResponse {
        ready: true,
        reason: None,
        timestamp: shard_ts,
    };

    assert!(response.ready);
    assert_eq!(response.timestamp, shard_ts);
    assert_eq!(response.timestamp.wallclock(), 1_234_567);
    assert_eq!(response.timestamp.logical(), 42);
}

/// RED PHASE TEST 5: CommitResponse should include HLC timestamp
#[test]
fn test_commit_response_includes_hlc_timestamp() {
    let commit_ts = HybridTimestamp::new(2_000_000, 10).unwrap();

    let response = CommitResponse {
        success: true,
        timestamp: commit_ts,
    };

    assert!(response.success);
    assert_eq!(response.timestamp, commit_ts);
    assert_eq!(response.timestamp.wallclock(), 2_000_000);
    assert_eq!(response.timestamp.logical(), 10);
}

/// RED PHASE TEST 6: HLC synchronization across network round-trip
#[test]
fn test_hlc_synchronization_in_network_round_trip() {
    // Coordinator sends prepare request with its HLC
    let coordinator_ts = HybridTimestamp::new(1_000_000, 5).unwrap();

    // Shard receives request and advances its HLC
    let shard_local_ts = HybridTimestamp::new(999_000, 10).unwrap(); // Clock behind
    let physical_time = 1_001_000;

    // Shard uses receive() to synchronize
    let shard_updated_ts = shard_local_ts
        .receive(coordinator_ts, physical_time)
        .unwrap();

    // Shard responds with its synchronized timestamp
    let prepare_response = PrepareResponse {
        ready: true,
        reason: None,
        timestamp: shard_updated_ts,
    };

    // Coordinator receives response and synchronizes its HLC
    let coordinator_physical = 1_002_000;
    let coordinator_updated_ts = coordinator_ts
        .receive(prepare_response.timestamp, coordinator_physical)
        .unwrap();

    // Verify causality: coordinator's final timestamp > all previous
    assert!(coordinator_updated_ts > coordinator_ts);
    assert!(coordinator_updated_ts > shard_local_ts);
    assert!(coordinator_updated_ts > shard_updated_ts);
}

/// RED PHASE TEST 7: Migration data ordering by HLC timestamp
#[test]
fn test_migration_data_ordering_by_hlc() {
    let mut nodes = vec![
        NodeData {
            id: NodeId::new(3).unwrap(),
            label: "C".to_string(),
            properties: vec![],
            valid_from: HybridTimestamp::new(1_000_000, 2).unwrap(),
            valid_to: None,
        },
        NodeData {
            id: NodeId::new(1).unwrap(),
            label: "A".to_string(),
            properties: vec![],
            valid_from: HybridTimestamp::new(1_000_000, 0).unwrap(),
            valid_to: None,
        },
        NodeData {
            id: NodeId::new(2).unwrap(),
            label: "B".to_string(),
            properties: vec![],
            valid_from: HybridTimestamp::new(1_000_000, 1).unwrap(),
            valid_to: None,
        },
    ];

    // Sort by HLC timestamp (same wallclock, different logical)
    nodes.sort_by_key(|n| n.valid_from);

    // Verify correct ordering
    assert_eq!(nodes[0].label, "A");
    assert_eq!(nodes[1].label, "B");
    assert_eq!(nodes[2].label, "C");

    // Verify timestamps are ordered
    assert!(nodes[0].valid_from < nodes[1].valid_from);
    assert!(nodes[1].valid_from < nodes[2].valid_from);
}
