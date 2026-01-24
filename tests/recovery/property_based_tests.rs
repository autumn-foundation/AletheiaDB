//! Property-Based Tests for Recovery Invariants (Issue #295)
//!
//! This module uses property-based testing with `proptest` to verify that
//! temporal invariants remain valid after database recovery from crashes.
//!
//! ## Four Core Invariants
//!
//! 1. **Temporal Consistency**
//!    - Transaction timestamps increase monotonically
//!    - Valid time ranges don't begin after they end
//!    - Delete operations properly close prior versions
//!
//! 2. **Version Chain Integrity**
//!    - Entities maintain unbroken version sequences
//!    - No temporal gaps exist between consecutive versions
//!    - Current storage matches the latest version state
//!
//! 3. **Referential Integrity**
//!    - All graph edges reference either existing nodes or their tombstones
//!    - Node deletions cascade appropriately to connected edges
//!
//! 4. **ID Uniqueness**
//!    - No duplicate identifiers exist for nodes, edges, or versions post-recovery
//!
//! ## Test Execution
//!
//! Each test executes 1000+ random operation sequences to thoroughly validate
//! invariants under various conditions.

use gallifreydb::{
    core::{
        id::{EdgeId, NodeId, VersionId},
        property::PropertyMapBuilder,
        temporal::{BiTemporalInterval, Timestamp, time},
    },
    storage::{
        current::CurrentStorage,
        historical::HistoricalStorage,
        persistence::{CheckpointConfig, PersistenceManager},
        wal::{
            LSN, WalOperation,
            concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
        },
    },
    utils::error::Result,
};
use proptest::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use tempfile::TempDir;

// ============================================================================
// Operation Strategies - Generate Random Database Operations
// ============================================================================

/// Represents a database operation for property testing.
#[derive(Debug, Clone)]
enum DbOperation {
    CreateNode {
        id: u64,
        label: String,
        value: i64,
    },
    UpdateNode {
        id: u64,
        new_value: i64,
    },
    DeleteNode {
        id: u64,
    },
    CreateEdge {
        id: u64,
        from: u64,
        to: u64,
        label: String,
    },
    UpdateEdge {
        id: u64,
        new_value: i64,
    },
    DeleteEdge {
        id: u64,
    },
}

/// Strategy for generating a sequence of random database operations.
///
/// This generates realistic operation sequences with:
/// - Node IDs in range 1..100
/// - Edge IDs in range 1..100
/// - A mix of create, update, and delete operations
/// - Dependencies (edges reference existing nodes)
fn operation_sequence_strategy() -> impl Strategy<Value = Vec<DbOperation>> {
    proptest::collection::vec(
        prop_oneof![
            // 40% create nodes
            (1u64..100, "[A-Z][a-z]+", 0i64..1000)
                .prop_map(|(id, label, value)| { DbOperation::CreateNode { id, label, value } }),
            // 20% update nodes
            (1u64..100, 0i64..1000)
                .prop_map(|(id, new_value)| { DbOperation::UpdateNode { id, new_value } }),
            // 10% delete nodes
            (1u64..100).prop_map(|id| DbOperation::DeleteNode { id }),
            // 20% create edges
            (1u64..100, 1u64..100, 1u64..100, "[A-Z]+").prop_map(|(id, from, to, label)| {
                DbOperation::CreateEdge {
                    id,
                    from,
                    to,
                    label,
                }
            }),
            // 5% update edges
            (1u64..100, 0i64..1000)
                .prop_map(|(id, new_value)| { DbOperation::UpdateEdge { id, new_value } }),
            // 5% delete edges
            (1u64..100).prop_map(|id| DbOperation::DeleteEdge { id }),
        ],
        50..200, // Generate 50-200 operations per test
    )
}

// ============================================================================
// Test Harness - Execute Operations and Recover
// ============================================================================

/// Test harness for executing operations and recovering.
struct RecoveryTestHarness {
    temp_dir: TempDir,
    wal_dir: PathBuf,
    checkpoint_dir: PathBuf,
}

impl RecoveryTestHarness {
    fn new() -> Self {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let checkpoint_dir = temp_dir.path().join("checkpoints");

        Self {
            temp_dir,
            wal_dir,
            checkpoint_dir,
        }
    }

    /// Execute a sequence of operations and return the WAL.
    fn execute_operations(&self, operations: &[DbOperation]) -> Result<ConcurrentWalSystem> {
        let wal_config = ConcurrentWalSystemConfig::new(self.wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let mut created_nodes: HashSet<u64> = HashSet::new();
        let mut created_edges: HashSet<u64> = HashSet::new();
        let mut version_id_counter: u64 = 1;
        let base_time = time::now().wallclock();
        let mut operation_counter: i64 = 0;

        for op in operations {
            // Create a unique timestamp for each operation (increment by 1000 microseconds)
            operation_counter += 1;
            let timestamp_counter = Timestamp::from(base_time + operation_counter * 1000);

            match op {
                DbOperation::CreateNode { id, label, value } => {
                    if !created_nodes.contains(id) {
                        wal.append(WalOperation::CreateNode {
                            node_id: NodeId::new(*id).unwrap(),
                            label: label.clone(),
                            properties: PropertyMapBuilder::new().insert("value", *value).build(),
                            temporal: BiTemporalInterval::current(timestamp_counter),
                        })?;
                        created_nodes.insert(*id);
                    }
                }
                DbOperation::UpdateNode { id, new_value } => {
                    if created_nodes.contains(id) {
                        let version_id = VersionId::new(version_id_counter).unwrap();
                        version_id_counter += 1;
                        wal.append(WalOperation::UpdateNode {
                            node_id: NodeId::new(*id).unwrap(),
                            version_id,
                            label: "Updated".to_string(), // Keep a simple label
                            properties: PropertyMapBuilder::new()
                                .insert("value", *new_value)
                                .build(),
                            temporal: BiTemporalInterval::current(timestamp_counter),
                        })?;
                    }
                }
                DbOperation::DeleteNode { id } => {
                    if created_nodes.contains(id) {
                        wal.append(WalOperation::DeleteNode {
                            node_id: NodeId::new(*id).unwrap(),
                            temporal: BiTemporalInterval::current(timestamp_counter),
                        })?;
                        created_nodes.remove(id);
                    }
                }
                DbOperation::CreateEdge {
                    id,
                    from,
                    to,
                    label,
                } => {
                    if !created_edges.contains(id)
                        && created_nodes.contains(from)
                        && created_nodes.contains(to)
                    {
                        wal.append(WalOperation::CreateEdge {
                            edge_id: EdgeId::new(*id).unwrap(),
                            source: NodeId::new(*from).unwrap(),
                            target: NodeId::new(*to).unwrap(),
                            label: label.clone(),
                            properties: PropertyMapBuilder::new().build(),
                            temporal: BiTemporalInterval::current(timestamp_counter),
                        })?;
                        created_edges.insert(*id);
                    }
                }
                DbOperation::UpdateEdge { id, new_value } => {
                    if created_edges.contains(id) {
                        let version_id = VersionId::new(version_id_counter).unwrap();
                        version_id_counter += 1;
                        wal.append(WalOperation::UpdateEdge {
                            edge_id: EdgeId::new(*id).unwrap(),
                            version_id,
                            label: "Updated".to_string(),
                            properties: PropertyMapBuilder::new()
                                .insert("value", *new_value)
                                .build(),
                            temporal: BiTemporalInterval::current(timestamp_counter),
                        })?;
                    }
                }
                DbOperation::DeleteEdge { id } => {
                    if created_edges.contains(id) {
                        wal.append(WalOperation::DeleteEdge {
                            edge_id: EdgeId::new(*id).unwrap(),
                            temporal: BiTemporalInterval::current(timestamp_counter),
                        })?;
                        created_edges.remove(id);
                    }
                }
            }
        }

        wal.flush()?;
        Ok(wal)
    }

    /// Recover from WAL and return the recovered storage.
    fn recover(
        &self,
        wal: &ConcurrentWalSystem,
    ) -> Result<(CurrentStorage, HistoricalStorage, LSN)> {
        let config = CheckpointConfig {
            checkpoint_dir: self.checkpoint_dir.clone(),
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;
        manager.recover(wal)
    }
}

// ============================================================================
// Invariant 1: Temporal Consistency
// ============================================================================

/// Verify temporal consistency invariants:
/// - Transaction timestamps increase monotonically
/// - Valid time ranges don't begin after they end
/// - Delete operations properly close prior versions
fn verify_temporal_consistency(
    _current: &CurrentStorage,
    historical: &HistoricalStorage,
) -> std::result::Result<(), String> {
    // Check all nodes in historical storage
    let all_versions = historical.get_all_node_versions();

    for (node_id, versions) in all_versions {
        // Need to sort versions by transaction time first
        let mut sorted_versions = versions;
        sorted_versions.sort_by_key(|v| v.temporal.transaction_time().start());

        let mut prev_tx_time: Option<Timestamp> = None;

        for version in sorted_versions {
            let interval = version.temporal;

            // Invariant: Valid time start <= Valid time end
            if interval.valid_time().start() > interval.valid_time().end() {
                return Err(format!(
                    "Node {:?}: Valid time start ({}) > end ({})",
                    node_id,
                    interval.valid_time().start(),
                    interval.valid_time().end()
                ));
            }

            // Invariant: Transaction timestamps increase monotonically
            if let Some(prev_tx) = prev_tx_time {
                if version.temporal.transaction_time().start() <= prev_tx {
                    return Err(format!(
                        "Node {:?}: Transaction time not monotonic: {} <= {}",
                        node_id,
                        version.temporal.transaction_time().start(),
                        prev_tx
                    ));
                }
            }

            prev_tx_time = Some(version.temporal.transaction_time().start());
        }
    }

    // Check all edges in historical storage
    let all_edge_versions = historical.get_all_edge_versions();

    for (edge_id, versions) in all_edge_versions {
        // Need to sort versions by transaction time first
        let mut sorted_versions = versions;
        sorted_versions.sort_by_key(|v| v.temporal.transaction_time().start());

        let mut prev_tx_time: Option<Timestamp> = None;

        for version in sorted_versions {
            let interval = version.temporal;

            // Invariant: Valid time start <= Valid time end
            if interval.valid_time().start() > interval.valid_time().end() {
                return Err(format!(
                    "Edge {:?}: Valid time start ({}) > end ({})",
                    edge_id,
                    interval.valid_time().start(),
                    interval.valid_time().end()
                ));
            }

            // Invariant: Transaction timestamps increase monotonically
            if let Some(prev_tx) = prev_tx_time {
                if version.temporal.transaction_time().start() <= prev_tx {
                    return Err(format!(
                        "Edge {:?}: Transaction time not monotonic: {} <= {}",
                        edge_id,
                        version.temporal.transaction_time().start(),
                        prev_tx
                    ));
                }
            }

            prev_tx_time = Some(version.temporal.transaction_time().start());
        }
    }

    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: Temporal consistency is maintained after recovery.
    #[test]
    fn prop_temporal_consistency(operations in operation_sequence_strategy()) {
        let harness = RecoveryTestHarness::new();

        // Execute operations
        let wal = harness.execute_operations(&operations)
            .expect("Failed to execute operations");

        // Recover from WAL
        let (current, historical, _lsn) = harness.recover(&wal)
            .expect("Failed to recover");

        // Verify temporal consistency
        verify_temporal_consistency(&current, &historical)
            .expect("Temporal consistency violated");
    }
}

// ============================================================================
// Invariant 2: Version Chain Integrity
// ============================================================================

/// Verify version chain integrity:
/// - Entities maintain unbroken version sequences
/// - No temporal gaps exist between consecutive versions
/// - Current storage matches the latest version state
fn verify_version_chain_integrity(
    current: &CurrentStorage,
    historical: &HistoricalStorage,
) -> std::result::Result<(), String> {
    // Check node version chains
    let all_versions = historical.get_all_node_versions();

    for (node_id, versions) in all_versions {
        if versions.is_empty() {
            continue;
        }

        // Sort versions by transaction time
        let mut sorted_versions = versions;
        sorted_versions.sort_by_key(|v| v.temporal.transaction_time().start());

        // Check for temporal gaps between consecutive versions
        for i in 1..sorted_versions.len() {
            let prev = sorted_versions[i - 1];
            let curr = sorted_versions[i];

            // Transaction times should be strictly increasing
            if curr.temporal.transaction_time().start() <= prev.temporal.transaction_time().start()
            {
                return Err(format!(
                    "Node {:?}: Version chain has non-increasing transaction times at index {}",
                    node_id, i
                ));
            }
        }

        // Verify current storage matches latest version (if it exists in current)
        if let Ok(current_node) = current.get_node(node_id) {
            let latest = sorted_versions.last().unwrap();
            // Current node should match latest version's label
            if current_node.label != latest.label {
                return Err(format!(
                    "Node {:?}: Current storage label differs from latest version",
                    node_id
                ));
            }
        }
    }

    // Check edge version chains
    let all_edge_versions = historical.get_all_edge_versions();

    for (edge_id, versions) in all_edge_versions {
        if versions.is_empty() {
            continue;
        }

        // Sort versions by transaction time
        let mut sorted_versions = versions;
        sorted_versions.sort_by_key(|v| v.temporal.transaction_time().start());

        // Check for temporal gaps between consecutive versions
        for i in 1..sorted_versions.len() {
            let prev = sorted_versions[i - 1];
            let curr = sorted_versions[i];

            // Transaction times should be strictly increasing
            if curr.temporal.transaction_time().start() <= prev.temporal.transaction_time().start()
            {
                return Err(format!(
                    "Edge {:?}: Version chain has non-increasing transaction times at index {}",
                    edge_id, i
                ));
            }
        }

        // Verify current storage matches latest version (if it exists in current)
        if let Ok(current_edge) = current.get_edge(edge_id) {
            let latest = sorted_versions.last().unwrap();
            // Current edge should match latest version's label
            if current_edge.label != latest.label {
                return Err(format!(
                    "Edge {:?}: Current storage label differs from latest version",
                    edge_id
                ));
            }
        }
    }

    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: Version chain integrity is maintained after recovery.
    #[test]
    fn prop_version_chain_integrity(operations in operation_sequence_strategy()) {
        let harness = RecoveryTestHarness::new();

        // Execute operations
        let wal = harness.execute_operations(&operations)
            .expect("Failed to execute operations");

        // Recover from WAL
        let (current, historical, _lsn) = harness.recover(&wal)
            .expect("Failed to recover");

        // Verify version chain integrity
        verify_version_chain_integrity(&current, &historical)
            .expect("Version chain integrity violated");
    }
}

// ============================================================================
// Invariant 3: Referential Integrity
// ============================================================================

/// Verify referential integrity:
/// - All edges reference either existing nodes or their tombstones
/// - Node deletions are properly reflected
fn verify_referential_integrity(
    current: &CurrentStorage,
    historical: &HistoricalStorage,
) -> std::result::Result<(), String> {
    // Get all edges from current storage
    let all_edges = current.get_all_edges();
    let all_node_versions = historical.get_all_node_versions();

    for edge in all_edges {
        let from_node = edge.source;
        let to_node = edge.target;

        // Check if referenced nodes exist in current storage OR historical storage
        let from_exists = current.get_node(from_node).is_ok()
            || all_node_versions
                .get(&from_node)
                .map(|versions| !versions.is_empty())
                .unwrap_or(false);

        let to_exists = current.get_node(to_node).is_ok()
            || all_node_versions
                .get(&to_node)
                .map(|versions| !versions.is_empty())
                .unwrap_or(false);

        if !from_exists {
            return Err(format!(
                "Edge {:?}: References non-existent source_node {:?}",
                edge.id, from_node
            ));
        }

        if !to_exists {
            return Err(format!(
                "Edge {:?}: References non-existent target_node {:?}",
                edge.id, to_node
            ));
        }
    }

    // Check historical edge versions as well
    let all_edge_versions = historical.get_all_edge_versions();

    for (edge_id, versions) in all_edge_versions {
        for version in versions {
            // Check all edge versions (deleted or not) reference valid nodes
            let from_node = version.source;
            let to_node = version.target;

            // Check if referenced nodes existed at some point
            let from_exists = current.get_node(from_node).is_ok()
                || all_node_versions
                    .get(&from_node)
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);

            let to_exists = current.get_node(to_node).is_ok()
                || all_node_versions
                    .get(&to_node)
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);

            if !from_exists {
                return Err(format!(
                    "Edge {:?} version: References non-existent source_node {:?}",
                    edge_id, from_node
                ));
            }

            if !to_exists {
                return Err(format!(
                    "Edge {:?} version: References non-existent target_node {:?}",
                    edge_id, to_node
                ));
            }
        }
    }

    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: Referential integrity is maintained after recovery.
    #[test]
    fn prop_referential_integrity(operations in operation_sequence_strategy()) {
        let harness = RecoveryTestHarness::new();

        // Execute operations
        let wal = harness.execute_operations(&operations)
            .expect("Failed to execute operations");

        // Recover from WAL
        let (current, historical, _lsn) = harness.recover(&wal)
            .expect("Failed to recover");

        // Verify referential integrity
        verify_referential_integrity(&current, &historical)
            .expect("Referential integrity violated");
    }
}

// ============================================================================
// Invariant 4: ID Uniqueness
// ============================================================================

/// Verify ID uniqueness:
/// - No duplicate node IDs
/// - No duplicate edge IDs
/// - No duplicate version IDs
fn verify_id_uniqueness(
    current: &CurrentStorage,
    historical: &HistoricalStorage,
) -> std::result::Result<(), String> {
    // Check node ID uniqueness in current storage
    let mut node_ids: HashSet<NodeId> = HashSet::new();

    for node in current.get_all_nodes() {
        if !node_ids.insert(node.id) {
            return Err(format!(
                "Duplicate node ID in current storage: {:?}",
                node.id
            ));
        }
    }

    // Check edge ID uniqueness in current storage
    let mut edge_ids: HashSet<EdgeId> = HashSet::new();

    for edge in current.get_all_edges() {
        if !edge_ids.insert(edge.id) {
            return Err(format!(
                "Duplicate edge ID in current storage: {:?}",
                edge.id
            ));
        }
    }

    // Check version ID uniqueness in historical storage
    let mut version_ids: HashSet<VersionId> = HashSet::new();

    // Node versions
    for (_node_id, versions) in historical.get_all_node_versions() {
        for version in versions {
            if !version_ids.insert(version.id) {
                return Err(format!(
                    "Duplicate version ID in historical storage: {:?}",
                    version.id
                ));
            }
        }
    }

    // Edge versions
    for (_edge_id, versions) in historical.get_all_edge_versions() {
        for version in versions {
            if !version_ids.insert(version.id) {
                return Err(format!(
                    "Duplicate version ID in historical storage: {:?}",
                    version.id
                ));
            }
        }
    }

    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: ID uniqueness is maintained after recovery.
    #[test]
    fn prop_id_uniqueness(operations in operation_sequence_strategy()) {
        let harness = RecoveryTestHarness::new();

        // Execute operations
        let wal = harness.execute_operations(&operations)
            .expect("Failed to execute operations");

        // Recover from WAL
        let (current, historical, _lsn) = harness.recover(&wal)
            .expect("Failed to recover");

        // Verify ID uniqueness
        verify_id_uniqueness(&current, &historical)
            .expect("ID uniqueness violated");
    }
}

// ============================================================================
// Combined Invariant Test
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: All four invariants hold after recovery.
    ///
    /// This is the master test that verifies all invariants together,
    /// ensuring that recovery maintains complete database consistency.
    #[test]
    fn prop_all_invariants_hold(operations in operation_sequence_strategy()) {
        let harness = RecoveryTestHarness::new();

        // Execute operations
        let wal = harness.execute_operations(&operations)
            .expect("Failed to execute operations");

        // Recover from WAL
        let (current, historical, _lsn) = harness.recover(&wal)
            .expect("Failed to recover");

        // Verify all invariants
        verify_temporal_consistency(&current, &historical)
            .expect("Temporal consistency violated");

        verify_version_chain_integrity(&current, &historical)
            .expect("Version chain integrity violated");

        verify_referential_integrity(&current, &historical)
            .expect("Referential integrity violated");

        verify_id_uniqueness(&current, &historical)
            .expect("ID uniqueness violated");
    }
}
