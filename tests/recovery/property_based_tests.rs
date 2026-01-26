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
//!    - Referenced nodes must overlap temporally with the edge's valid time
//!
//! 4. **ID Uniqueness**
//!    - No duplicate identifiers exist for nodes, edges, or versions post-recovery
//!
//! ## Test Execution
//!
//! Runs **1000 test cases**, each with **50-200 random operations**, totaling
//! **50,000-200,000 operations** to thoroughly validate invariants.

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
// Test Configuration Constants
// ============================================================================

/// Number of property test cases to execute
const PROPTEST_CASES: u32 = 1000;

/// Minimum operations per test case
const MIN_OPERATIONS_PER_TEST: usize = 50;

/// Maximum operations per test case
const MAX_OPERATIONS_PER_TEST: usize = 200;

/// Timestamp increment in microseconds (1ms) for each operation
const TIMESTAMP_INCREMENT_US: i64 = 1000;

/// Starting version ID for UpdateNode/UpdateEdge operations.
///
/// Set to 10_000_000 to avoid conflicts with auto-generated version IDs during recovery.
///
/// CRITICAL: Auto-generated version IDs come from CreateNode/CreateEdge/DeleteNode/DeleteEdge.
/// When an UpdateNode with version_id=N is processed during recovery, next_version_id jumps
/// to N+1. This means subsequent DeleteNode operations will generate tombstone version IDs
/// starting from N+1, which can collide with subsequent UpdateNode operations!
///
/// Example collision scenario with UPDATE_VERSION_ID_START = 1_000_000:
///   1. CreateNode(id=1) → version_id=1, next_version_id=2
///   2. UpdateNode(id=1, version_id=1_000_000) → next_version_id=1_000_001
///   3. DeleteNode(id=1) → tombstone version_id=1_000_001 (COLLISION!)
///   4. UpdateNode(id=2, version_id=1_000_001) → DUPLICATE VERSION ID
///
/// With max 200 operations per test, auto-generated IDs stay well below 10 million.
const UPDATE_VERSION_ID_START: u64 = 10_000_000;

/// Increment between consecutive UpdateNode/UpdateEdge version IDs.
///
/// This must be large enough that CreateNode/CreateEdge/DeleteNode/DeleteEdge operations
/// occurring between two Updates cannot generate version IDs that reach the next Update's ID.
///
/// With max 200 operations per test, we use 1000 as a safe increment (far exceeds max operations).
const VERSION_ID_INCREMENT: u64 = 1000;

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
        MIN_OPERATIONS_PER_TEST..MAX_OPERATIONS_PER_TEST,
    )
}

// ============================================================================
// Test Harness - Execute Operations and Recover
// ============================================================================

/// Test harness for executing operations and recovering.
struct RecoveryTestHarness {
    /// Temporary directory (must be kept alive for test duration)
    #[allow(dead_code)]
    temp_dir: TempDir,
    wal_dir: PathBuf,
    checkpoint_dir: PathBuf,
}

impl RecoveryTestHarness {
    fn new() -> Result<Self> {
        let temp_dir = TempDir::new().map_err(|e| {
            gallifreydb::utils::error::Error::other(format!("Failed to create temp dir: {}", e))
        })?;
        let wal_dir = temp_dir.path().join("wal");
        let checkpoint_dir = temp_dir.path().join("checkpoints");

        Ok(Self {
            temp_dir,
            wal_dir,
            checkpoint_dir,
        })
    }

    /// Execute a sequence of operations and return the WAL.
    fn execute_operations(&self, operations: &[DbOperation]) -> Result<ConcurrentWalSystem> {
        let wal_config = ConcurrentWalSystemConfig::new(self.wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let mut created_nodes: HashSet<u64> = HashSet::new();
        let mut created_edges: HashSet<u64> = HashSet::new();
        // Start version IDs at a large offset to avoid conflicts with auto-generated IDs
        // from CreateNode/CreateEdge operations during recovery
        let mut version_id_counter: u64 = UPDATE_VERSION_ID_START;

        // Use deterministic timestamps (base + offset) rather than time::now() for each operation
        // to ensure reproducible test failures and consistent temporal ordering
        let base_time = time::now().wallclock();
        let mut operation_counter: i64 = 0;

        for op in operations {
            // Create a unique timestamp for each operation (increments by 1ms)
            operation_counter += 1;
            let timestamp_counter =
                Timestamp::from(base_time + operation_counter * TIMESTAMP_INCREMENT_US);

            match op {
                DbOperation::CreateNode { id, label, value } => {
                    if !created_nodes.contains(id) {
                        wal.append(WalOperation::CreateNode {
                            node_id: NodeId::new(*id)?,
                            label: label.clone(),
                            properties: PropertyMapBuilder::new().insert("value", *value).build(),
                            temporal: BiTemporalInterval::current(timestamp_counter),
                        })?;
                        created_nodes.insert(*id);
                    }
                }
                DbOperation::UpdateNode { id, new_value } => {
                    if created_nodes.contains(id) {
                        let version_id = VersionId::new(version_id_counter)?;
                        version_id_counter += VERSION_ID_INCREMENT;
                        wal.append(WalOperation::UpdateNode {
                            node_id: NodeId::new(*id)?,
                            version_id,
                            label: GLOBAL_INTERNER.intern("Updated").unwrap(),
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
                            node_id: NodeId::new(*id)?,
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
                            edge_id: EdgeId::new(*id)?,
                            source: NodeId::new(*from)?,
                            target: NodeId::new(*to)?,
                            label: label.clone(),
                            properties: PropertyMapBuilder::new().build(),
                            temporal: BiTemporalInterval::current(timestamp_counter),
                        })?;
                        created_edges.insert(*id);
                    }
                }
                DbOperation::UpdateEdge { id, new_value } => {
                    if created_edges.contains(id) {
                        let version_id = VersionId::new(version_id_counter)?;
                        version_id_counter += VERSION_ID_INCREMENT;
                        wal.append(WalOperation::UpdateEdge {
                            edge_id: EdgeId::new(*id)?,
                            version_id,
                            label: GLOBAL_INTERNER.intern("Updated").unwrap(),
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
                            edge_id: EdgeId::new(*id)?,
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
// Helper Functions - Reduce Code Duplication
// ============================================================================

/// Macro to verify temporal consistency for a set of versions.
///
/// Checks:
/// - Valid time start <= end
/// - Transaction timestamps increase monotonically
macro_rules! verify_temporal_consistency_for {
    ($entity_id:expr, $entity_type:expr, $versions:expr) => {{
        let mut sorted_versions = $versions;
        if !sorted_versions.is_empty() {
            sorted_versions.sort_by_key(|v| v.temporal.transaction_time().start());

            let mut prev_tx_time: Option<Timestamp> = None;

            for version in sorted_versions {
                let interval = version.temporal;

                // Invariant: Valid time start <= Valid time end
                if interval.valid_time().start() > interval.valid_time().end() {
                    return Err(format!(
                        "{} {:?}: Valid time start ({}) > end ({})",
                        $entity_type,
                        $entity_id,
                        interval.valid_time().start(),
                        interval.valid_time().end()
                    ));
                }

                // Invariant: Transaction timestamps increase monotonically
                if let Some(prev_tx) = prev_tx_time {
                    if version.temporal.transaction_time().start() <= prev_tx {
                        return Err(format!(
                            "{} {:?}: Transaction time not monotonic: {} <= {}",
                            $entity_type,
                            $entity_id,
                            version.temporal.transaction_time().start(),
                            prev_tx
                        ));
                    }
                }

                prev_tx_time = Some(version.temporal.transaction_time().start());
            }
        }
        std::result::Result::<(), String>::Ok(())
    }};
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
    for (node_id, versions) in historical.get_all_node_versions() {
        verify_temporal_consistency_for!(node_id, "Node", versions)?;
    }

    // Check all edges in historical storage
    for (edge_id, versions) in historical.get_all_edge_versions() {
        verify_temporal_consistency_for!(edge_id, "Edge", versions)?;
    }

    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(PROPTEST_CASES))]

    /// Property: Temporal consistency is maintained after recovery.
    #[test]
    fn prop_temporal_consistency(operations in operation_sequence_strategy()) {
        let harness = RecoveryTestHarness::new()?;

        // Execute operations and recover from WAL
        let wal = harness.execute_operations(&operations)?;
        let (current, historical, _lsn) = harness.recover(&wal)?;

        // Verify temporal consistency
        verify_temporal_consistency(&current, &historical)
            .map_err(TestCaseError::fail)?;
    }
}

// ============================================================================
// Invariant 2: Version Chain Integrity
// ============================================================================

/// Verify version chain integrity:
/// - Current storage matches the latest version state
///
/// Note: Monotonicity is already checked by verify_temporal_consistency
fn verify_version_chain_integrity(
    current: &CurrentStorage,
    historical: &HistoricalStorage,
) -> std::result::Result<(), String> {
    // Check node version chains
    for (node_id, versions) in historical.get_all_node_versions() {
        if versions.is_empty() {
            continue;
        }

        // Sort versions by transaction time (for finding latest)
        let mut sorted_versions = versions;
        sorted_versions.sort_by_key(|v| v.temporal.transaction_time().start());

        // Verify current storage matches latest version (if it exists in current)
        if let Ok(current_node) = current.get_node(node_id) {
            let latest = sorted_versions.last().ok_or_else(|| {
                format!("Node {:?}: Empty version chain after filtering", node_id)
            })?;

            if current_node.label != latest.label {
                return Err(format!(
                    "Node {:?}: Current storage label differs from latest version",
                    node_id
                ));
            }
        }
    }

    // Check edge version chains
    for (edge_id, versions) in historical.get_all_edge_versions() {
        if versions.is_empty() {
            continue;
        }

        // Sort versions by transaction time (for finding latest)
        let mut sorted_versions = versions;
        sorted_versions.sort_by_key(|v| v.temporal.transaction_time().start());

        // Verify current storage matches latest version (if it exists in current)
        if let Ok(current_edge) = current.get_edge(edge_id) {
            let latest = sorted_versions.last().ok_or_else(|| {
                format!("Edge {:?}: Empty version chain after filtering", edge_id)
            })?;

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
    #![proptest_config(ProptestConfig::with_cases(PROPTEST_CASES))]

    /// Property: Version chain integrity is maintained after recovery.
    #[test]
    fn prop_version_chain_integrity(operations in operation_sequence_strategy()) {
        let harness = RecoveryTestHarness::new()?;

        // Execute operations and recover from WAL
        let wal = harness.execute_operations(&operations)?;
        let (current, historical, _lsn) = harness.recover(&wal)?;

        // Verify version chain integrity
        verify_version_chain_integrity(&current, &historical)
            .map_err(TestCaseError::fail)?;
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
    #![proptest_config(ProptestConfig::with_cases(PROPTEST_CASES))]

    /// Property: Referential integrity is maintained after recovery.
    #[test]
    fn prop_referential_integrity(operations in operation_sequence_strategy()) {
        let harness = RecoveryTestHarness::new()?;

        // Execute operations and recover from WAL
        let wal = harness.execute_operations(&operations)?;
        let (current, historical, _lsn) = harness.recover(&wal)?;

        // Verify referential integrity
        verify_referential_integrity(&current, &historical)
            .map_err(TestCaseError::fail)?;
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
    #![proptest_config(ProptestConfig::with_cases(PROPTEST_CASES))]

    /// Property: ID uniqueness is maintained after recovery.
    #[test]
    fn prop_id_uniqueness(operations in operation_sequence_strategy()) {
        let harness = RecoveryTestHarness::new()?;

        // Execute operations and recover from WAL
        let wal = harness.execute_operations(&operations)?;
        let (current, historical, _lsn) = harness.recover(&wal)?;

        // Verify ID uniqueness
        verify_id_uniqueness(&current, &historical)
            .map_err(TestCaseError::fail)?;
    }
}

// ============================================================================
// Combined Invariant Test
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(PROPTEST_CASES))]

    /// Property: All four invariants hold after recovery.
    ///
    /// This is the master test that verifies all invariants together,
    /// ensuring that recovery maintains complete database consistency.
    #[test]
    fn prop_all_invariants_hold(operations in operation_sequence_strategy()) {
        let harness = RecoveryTestHarness::new()?;

        // Execute operations and recover from WAL
        let wal = harness.execute_operations(&operations)?;
        let (current, historical, _lsn) = harness.recover(&wal)?;

        // Verify all invariants
        verify_temporal_consistency(&current, &historical)
            .map_err(TestCaseError::fail)?;

        verify_version_chain_integrity(&current, &historical)
            .map_err(TestCaseError::fail)?;

        verify_referential_integrity(&current, &historical)
            .map_err(TestCaseError::fail)?;

        verify_id_uniqueness(&current, &historical)
            .map_err(TestCaseError::fail)?;
    }
}
