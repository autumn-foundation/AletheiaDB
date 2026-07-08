//! WAL recovery logic.
//!
//! This module provides the shared logic for replaying Write-Ahead Log (WAL) entries
//! back into the database's storage engines. It acts as the bridge between durable
//! disk storage and the in-memory/disk-backed data structures.
//!
//! # The "Resurrection" Process
//!
//! When AletheiaDB experiences an ungraceful shutdown (a crash or power failure),
//! the data in memory is lost. To ensure data durability (the 'D' in ACID), every
//! committed transaction is first written to the WAL on disk.
//!
//! Upon restart, this module reads those persisted WAL entries and "replays" them.
//! It reconstructs the exact state of the database by systematically applying every
//! recorded `Create`, `Update`, and `Delete` operation to both the `CurrentStorage`
//! (the hot, current state of the graph) and the `HistoricalStorage` (the temporal,
//! versioned state of the graph).
//!
//! # Why is it needed?
//!
//! Replaying the WAL is necessary because building the database's optimized read
//! structures (like the CSR adjacency matrices or vector indexes) is expensive and
//! typically done in memory. Instead of syncing these complex structures to disk on
//! every write, we sync a simple, sequential log of operations. The recovery process
//! uses this log to rebuild the complex structures exactly as they were.

use crate::core::constraint::ConstraintRegistry;
use crate::core::error::Result;
use crate::core::graph::{Edge, Node};
use crate::core::id::{TxId, VersionId};
use crate::core::version::VersionMetadata;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use crate::storage::wal::{LSN, WalOperation};

/// Replay WAL entries starting from a given LSN into existing storage instances.
///
/// # Arguments
///
/// * `wal` - The concurrent WAL system to read entries from
/// * `current` - Current storage to apply updates to
/// * `historical` - Historical storage to apply updates to
/// * `start_lsn` - The LSN to start replaying from (inclusive)
/// * `next_version_id` - The starting version ID for new versions created during replay
///
/// # Returns
///
/// A tuple containing:
/// - The final LSN reached after replay (current LSN of the WAL)
/// - The maximum node ID observed (if any)
/// - The maximum edge ID observed (if any)
/// - The next version ID to use (updated after replay)
///
/// # Examples
///
/// ```rust,ignore
/// # use tempfile::tempdir;
/// # use std::sync::Arc;
/// # use aletheiadb::WalConfigBuilder;
/// # use aletheiadb::storage::wal::{LSN, WalOperation};
/// # use aletheiadb::storage::wal::concurrent_system::ConcurrentWalSystem;
/// # use aletheiadb::storage::current::CurrentStorage;
/// # use aletheiadb::storage::historical::HistoricalStorage;
/// # // Note: This function is pub(crate) and generally called internally by
/// # // AletheiaDB::new() or CheckpointManager::recover().
/// # use aletheiadb::core::id::{NodeId, VersionId, TxId};
/// # use aletheiadb::core::interning::InternedString;
/// # use aletheiadb::core::property::PropertyMap;
/// # use aletheiadb::core::temporal::Timestamp;
/// # use aletheiadb::core::GLOBAL_INTERNER;
/// # fn main() -> aletheiadb::core::error::Result<()> {
/// // 1. Set up a temporary directory and WAL system
/// let dir = tempdir().unwrap();
/// let mut config = WalConfigBuilder::new().build();
/// config.wal_dir = dir.path().to_path_buf();
/// let wal = ConcurrentWalSystem::new(&config)?;
///
/// // 2. Simulate a crash: Write a raw entry directly to the WAL
/// let node_id = NodeId::new(1).unwrap();
/// let label = GLOBAL_INTERNER.intern("User").unwrap();
/// let ts = Timestamp::from(100);
///
/// let op = WalOperation::CreateNode {
///     node_id,
///     label,
///     properties: PropertyMap::default(),
///     valid_from: ts,
/// };
/// wal.append(op)?; // Append takes just the operation, timestamp is assigned by WAL
/// wal.flush()?;
///
/// // 3. The Resurrection: Create empty storage instances
/// let current = CurrentStorage::new();
/// let mut historical = HistoricalStorage::new();
///
/// // 4. Replay the WAL into storage
/// let start_lsn = LSN::initial();
/// let initial_version_id = 1;
///
/// // Internally, AletheiaDB calls this during initialization
/// let (final_lsn, max_node, _, next_vid) = replay_wal_into_storage(
///     &wal,
///     &current,
///     &mut historical,
///     start_lsn,
///     initial_version_id
/// )?;
///
/// // 5. Verify the state was reconstructed
/// assert_eq!(max_node, Some(1));
/// assert!(current.get_node(node_id).is_ok(), "Node was recovered!");
/// # Ok(())
/// # }
/// ```
pub(crate) fn replay_wal_into_storage(
    wal: &ConcurrentWalSystem,
    current: &CurrentStorage,
    historical: &mut HistoricalStorage,
    start_lsn: LSN,
    next_version_id: u64,
) -> Result<(LSN, Option<u64>, Option<u64>, u64)> {
    replay_wal_into_storage_with_constraints(
        wal,
        current,
        historical,
        start_lsn,
        next_version_id,
        None,
    )
}

pub(crate) fn replay_wal_into_storage_with_constraints(
    wal: &ConcurrentWalSystem,
    current: &CurrentStorage,
    historical: &mut HistoricalStorage,
    start_lsn: LSN,
    mut next_version_id: u64,
    constraint_registry: Option<&ConstraintRegistry>,
) -> Result<(LSN, Option<u64>, Option<u64>, u64)> {
    const RECOVERY_TX_ID: u64 = 0;

    let mut max_node_id: Option<u64> = None;
    let mut max_edge_id: Option<u64> = None;

    let wal_entries = wal.read_from(start_lsn)?;

    if !wal_entries.is_empty() {
        #[cfg(feature = "observability")]
        tracing::info!(
            "Replaying {} WAL entries from LSN {}",
            wal_entries.len(),
            start_lsn.0
        );
        #[cfg(not(feature = "observability"))]
        eprintln!(
            "Replaying {} WAL entries from LSN {}",
            wal_entries.len(),
            start_lsn.0
        );
    }

    for entry in wal_entries {
        match entry.operation {
            WalOperation::CreateNode {
                node_id,
                label,
                properties,
                valid_from,
                provenance,
            } => {
                max_node_id = Some(match max_node_id {
                    Some(current_max) => current_max.max(node_id.as_u64()),
                    None => node_id.as_u64(),
                });

                let interned_label = label;

                // Transaction time comes from when the WAL entry was logged
                let commit_timestamp = entry.timestamp;
                let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);
                let version_id = VersionId::new(next_version_id)?;
                next_version_id += 1;

                let node = Node::with_metadata(
                    node_id,
                    interned_label,
                    properties.clone(),
                    version_id,
                    metadata,
                );

                current.insert_node_direct(node, commit_timestamp)?;
                historical.add_node_version_with_provenance(
                    node_id,
                    version_id,
                    valid_from,
                    commit_timestamp,
                    interned_label,
                    properties,
                    false, // not a tombstone
                    provenance.map(std::sync::Arc::new),
                )?;
            }
            WalOperation::CreateEdge {
                edge_id,
                source,
                target,
                label,
                properties,
                valid_from,
                provenance,
            } => {
                max_edge_id = Some(match max_edge_id {
                    Some(current_max) => current_max.max(edge_id.as_u64()),
                    None => edge_id.as_u64(),
                });

                let interned_label = label;

                let commit_timestamp = entry.timestamp;
                let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);
                let version_id = VersionId::new(next_version_id)?;
                next_version_id += 1;

                let edge = Edge::with_metadata(
                    edge_id,
                    interned_label,
                    source,
                    target,
                    properties.clone(),
                    version_id,
                    metadata,
                );

                current.insert_edge_direct(edge)?;
                historical.add_edge_version_with_provenance(
                    edge_id,
                    version_id,
                    valid_from,
                    commit_timestamp,
                    interned_label,
                    source,
                    target,
                    properties,
                    false, // not a tombstone
                    provenance.map(std::sync::Arc::new),
                )?;
            }
            WalOperation::UpdateNode {
                node_id,
                version_id,
                label,
                properties,
                valid_from,
                provenance,
            } => {
                next_version_id = next_version_id.max(version_id.as_u64() + 1);

                let interned_label = label;

                let commit_timestamp = entry.timestamp;
                let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);

                let node = Node::with_metadata(
                    node_id,
                    interned_label,
                    properties.clone(),
                    version_id,
                    metadata,
                );

                current.update_node_direct(node, commit_timestamp)?;

                if let Some(prev_version_id) = historical.get_current_node_version(node_id) {
                    historical
                        .close_node_version_transaction_time(prev_version_id, commit_timestamp)?;
                }

                historical.add_node_version_with_provenance(
                    node_id,
                    version_id,
                    valid_from,
                    commit_timestamp,
                    interned_label,
                    properties,
                    false, // not a tombstone
                    provenance.map(std::sync::Arc::new),
                )?;
            }
            WalOperation::UpdateEdge {
                edge_id,
                version_id,
                label,
                properties,
                valid_from,
                provenance,
            } => {
                next_version_id = next_version_id.max(version_id.as_u64() + 1);

                let current_edge = current.get_edge(edge_id)?;

                let interned_label = label;

                let commit_timestamp = entry.timestamp;
                let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);

                let edge = Edge::with_metadata(
                    edge_id,
                    interned_label,
                    current_edge.source,
                    current_edge.target,
                    properties.clone(),
                    version_id,
                    metadata,
                );

                current.update_edge_direct(edge)?;

                if let Some(prev_version_id) = historical.get_current_edge_version(edge_id) {
                    historical
                        .close_edge_version_transaction_time(prev_version_id, commit_timestamp)?;
                }

                historical.add_edge_version_with_provenance(
                    edge_id,
                    version_id,
                    valid_from,
                    commit_timestamp,
                    interned_label,
                    current_edge.source,
                    current_edge.target,
                    properties,
                    false, // not a tombstone
                    provenance.map(std::sync::Arc::new),
                )?;
            }
            WalOperation::DeleteNode {
                node_id,
                valid_from: _,
            } => {
                // If the node doesn't exist in current storage, it might have been deleted already
                // or never existed (if we're replaying a delete for something we missed creation of?).
                // But for linear WAL replay, creation should have happened before.
                // However, `get_node` might fail.
                if let Ok(node) = current.get_node(node_id) {
                    let commit_timestamp = entry.timestamp;

                    if let Some(current_version_id) = historical.get_current_node_version(node_id) {
                        historical.close_node_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    let tombstone_version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

                    // Tombstones use commit_timestamp for both valid_from and tx_time
                    // The is_tombstone=true flag closes the valid_time immediately
                    historical.add_node_version(
                        node_id,
                        tombstone_version_id,
                        commit_timestamp,
                        commit_timestamp,
                        node.label,
                        node.properties.clone(),
                        true, // is_tombstone
                    )?;

                    current.delete_node_direct(node_id, commit_timestamp)?;
                }
            }
            WalOperation::DeleteEdge {
                edge_id,
                valid_from: _,
            } => {
                if let Ok(edge) = current.get_edge(edge_id) {
                    let commit_timestamp = entry.timestamp;

                    if let Some(current_version_id) = historical.get_current_edge_version(edge_id) {
                        historical.close_edge_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    let tombstone_version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

                    // Tombstones use commit_timestamp for both valid_from and tx_time
                    // The is_tombstone=true flag closes the valid_time immediately
                    historical.add_edge_version(
                        edge_id,
                        tombstone_version_id,
                        commit_timestamp,
                        commit_timestamp,
                        edge.label,
                        edge.source,
                        edge.target,
                        edge.properties.clone(),
                        true, // is_tombstone
                    )?;

                    current.delete_edge_direct(edge_id)?;
                }
            }
            WalOperation::RetractNode { node_id, valid_to } => {
                // Valid-time retraction (Issue #3230). Reconstruct exactly what
                // the original commit did:
                //   (a) close the head version's TRANSACTION time (its valid
                //       interval stays untouched — append-only, never rewrite);
                //   (b) append a new version with the same properties whose
                //       valid interval is closed at the logged `valid_to`
                //       (honored faithfully — NOT the replay/commit time);
                //   (c) remove the node from current storage.
                if let Ok(node) = current.get_node(node_id) {
                    let commit_timestamp = entry.timestamp;

                    // Capture the head's valid_from BEFORE appending, so the
                    // retraction version reproduces the same interval start.
                    let head_version_id = historical.get_current_node_version(node_id);
                    let valid_from = head_version_id
                        .and_then(|vid| historical.get_node_version(vid))
                        .map(|v| v.temporal.valid_time().start())
                        .unwrap_or(commit_timestamp);

                    if let Some(current_version_id) = head_version_id {
                        historical.close_node_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    let retraction_version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

                    historical.add_retracted_node_version(
                        node_id,
                        retraction_version_id,
                        valid_from,
                        valid_to,
                        commit_timestamp,
                        node.label,
                        node.properties.clone(),
                    )?;

                    current.delete_node_direct(node_id, commit_timestamp)?;
                }
            }
            WalOperation::RetractEdge { edge_id, valid_to } => {
                // Valid-time retraction of an edge (Issue #3230); mirrors the
                // RetractNode arm above, honoring the logged `valid_to`.
                if let Ok(edge) = current.get_edge(edge_id) {
                    let commit_timestamp = entry.timestamp;

                    let head_version_id = historical.get_current_edge_version(edge_id);
                    let valid_from = head_version_id
                        .and_then(|vid| historical.get_edge_version(vid))
                        .map(|v| v.temporal.valid_time().start())
                        .unwrap_or(commit_timestamp);

                    if let Some(current_version_id) = head_version_id {
                        historical.close_edge_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    let retraction_version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

                    historical.add_retracted_edge_version(
                        edge_id,
                        retraction_version_id,
                        valid_from,
                        valid_to,
                        commit_timestamp,
                        edge.label,
                        edge.source,
                        edge.target,
                        edge.properties.clone(),
                    )?;

                    current.delete_edge_direct(edge_id)?;
                }
            }
            WalOperation::Checkpoint { .. } => {
                // Checkpoint markers are informational only during replay
            }
            WalOperation::DeclareUniqueConstraint { label, property } => {
                if let Some(reg) = constraint_registry {
                    reg.declare(label, property);
                }
            }
            WalOperation::DropUniqueConstraint { label, property } => {
                if let Some(reg) = constraint_registry {
                    reg.undeclare(label, property);
                }
            }
        }
    }

    let final_lsn = wal.current_lsn();

    Ok((final_lsn, max_node_id, max_edge_id, next_version_id))
}

/// Replay ONLY constraint declaration/drop entries from the full WAL history.
///
/// Called during index-persistence startup BEFORE the regular snapshot-based
/// WAL replay.  Because constraint declarations are written to the WAL before
/// the corresponding node data, they may lie at LSNs below the persisted
/// snapshot LSN and therefore be skipped by the normal differential replay.
/// This pass reads every WAL entry from LSN 0 and replays only the two
/// constraint-related operations, giving us the net constraint state.
pub(crate) fn replay_constraint_declarations_from_wal(
    wal: &ConcurrentWalSystem,
    registry: &ConstraintRegistry,
) -> Result<()> {
    let all_entries = wal.read_from(LSN::initial())?;
    for entry in all_entries {
        match entry.operation {
            WalOperation::DeclareUniqueConstraint { label, property } => {
                registry.declare(label, property);
            }
            WalOperation::DropUniqueConstraint { label, property } => {
                registry.undeclare(label, property);
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use tempfile::tempdir;

    #[test]
    fn replay_constraint_declarations_declare_and_drop() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let label = GLOBAL_INTERNER.intern("RcvTestLabel").unwrap();
        let prop = GLOBAL_INTERNER.intern("rcvTestProp").unwrap();

        // Declare then drop: net result = not active.
        wal.append(WalOperation::DeclareUniqueConstraint {
            label,
            property: prop,
        })
        .unwrap();
        wal.append(WalOperation::DropUniqueConstraint {
            label,
            property: prop,
        })
        .unwrap();
        wal.flush().unwrap();

        let registry = ConstraintRegistry::new();
        replay_constraint_declarations_from_wal(&wal, &registry).unwrap();

        assert!(
            !registry.is_constrained(label, prop),
            "net declare+drop must leave constraint inactive"
        );
    }

    /// Issue #3230: `RetractNode`/`RetractEdge` replay must reconstruct the
    /// retraction exactly — close the head's transaction time, append a
    /// version whose valid interval honors the logged `valid_to` (NOT the
    /// replay/commit time, unlike the historical DeleteNode behavior), and
    /// remove the entity from current storage.
    #[test]
    fn replay_retract_node_honors_valid_to() {
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::property::PropertyMap;
        use crate::core::temporal::time;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let node_id = crate::core::NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("RcvRetractNode").unwrap();
        let now = time::now().wallclock();
        let valid_from = crate::core::hlc::HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();
        let valid_to = crate::core::hlc::HybridTimestamp::new(now - 1_800_000_000, 0).unwrap();

        wal.append(WalOperation::CreateNode {
            node_id,
            label,
            properties: PropertyMap::new(),
            valid_from,
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::RetractNode { node_id, valid_to })
            .unwrap();
        wal.flush().unwrap();

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();
        replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), 1).unwrap();

        // Removed from current state.
        assert!(current.get_node(node_id).is_err());

        // The head version carries the closed valid interval
        // [valid_from, valid_to) — honoring the logged valid_to.
        let head_id = historical.get_current_node_version(node_id).unwrap();
        let head = historical.get_node_version(head_id).unwrap();
        assert_eq!(head.temporal.valid_time().start(), valid_from);
        assert_eq!(
            head.temporal.valid_time().end(),
            valid_to,
            "replay must honor the logged valid_to, not the replay time"
        );

        // Bi-temporal reads: visible strictly before valid_to, not at/after.
        let probe_before = crate::core::hlc::HybridTimestamp::new(now - 2_700_000_000, 0).unwrap();
        assert!(
            historical
                .get_node_at_time(node_id, probe_before, time::now())
                .is_ok()
        );
        assert!(
            historical
                .get_node_at_time(node_id, valid_to, time::now())
                .is_err()
        );
        assert!(
            historical
                .get_node_at_time(node_id, time::now(), time::now())
                .is_err()
        );

        // Append-only AS OF SYSTEM_TIME contract, post-replay (fix-round
        // #8): the PRE-retraction head must still carry an OPEN valid
        // interval with its transaction time closed at the LOGGED
        // RetractNode entry timestamp — replay must not substitute the
        // replay time, and must never rewrite the pre-retraction record.
        let entries = wal.read_from(LSN::initial()).unwrap();
        let logged_retract_ts = entries
            .iter()
            .find(|e| matches!(e.operation, WalOperation::RetractNode { .. }))
            .expect("RetractNode entry must be in the WAL")
            .timestamp;
        let logged_create_ts = entries
            .iter()
            .find(|e| matches!(e.operation, WalOperation::CreateNode { .. }))
            .expect("CreateNode entry must be in the WAL")
            .timestamp;

        let history = historical.get_node_history(node_id).unwrap();
        assert_eq!(history.version_count(), 2, "create + retraction versions");
        let pre_retraction_head = &history.versions[0];
        assert!(
            pre_retraction_head.temporal.valid_time().is_current(),
            "pre-retraction head's valid interval must stay open-ended after replay"
        );
        assert!(
            !pre_retraction_head.temporal.transaction_time().is_current(),
            "pre-retraction head's transaction time must be closed after replay"
        );
        assert_eq!(
            pre_retraction_head.temporal.transaction_time().end(),
            logged_retract_ts,
            "transaction time must close at the LOGGED entry timestamp, not the replay time"
        );

        // Anchoring AS OF SYSTEM_TIME before the retraction's logged commit
        // shows the fact open-ended (valid even at valid times >= valid_to);
        // anchoring at the retraction's commit does not.
        assert!(
            historical
                .get_node_at_time(node_id, time::now(), logged_create_ts)
                .is_ok(),
            "AS OF SYSTEM_TIME before the retraction must show the fact open-ended"
        );
        assert!(
            historical
                .get_node_at_time(node_id, time::now(), logged_retract_ts)
                .is_err()
        );
    }

    /// Issue #3230: RetractEdge replay — same contract as RetractNode.
    #[test]
    fn replay_retract_edge_honors_valid_to() {
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::property::PropertyMap;
        use crate::core::temporal::time;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let source = crate::core::NodeId::new(1).unwrap();
        let target = crate::core::NodeId::new(2).unwrap();
        let edge_id = crate::core::EdgeId::new(1).unwrap();
        let node_label = GLOBAL_INTERNER.intern("RcvRetractEdgeNode").unwrap();
        let edge_label = GLOBAL_INTERNER.intern("RCV_RETRACT_EDGE").unwrap();
        let now = time::now().wallclock();
        let valid_from = crate::core::hlc::HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();
        let valid_to = crate::core::hlc::HybridTimestamp::new(now - 1_800_000_000, 0).unwrap();

        for node_id in [source, target] {
            wal.append(WalOperation::CreateNode {
                node_id,
                label: node_label,
                properties: PropertyMap::new(),
                valid_from,
                provenance: None,
            })
            .unwrap();
        }
        wal.append(WalOperation::CreateEdge {
            edge_id,
            source,
            target,
            label: edge_label,
            properties: PropertyMap::new(),
            valid_from,
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::RetractEdge { edge_id, valid_to })
            .unwrap();
        wal.flush().unwrap();

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();
        replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), 1).unwrap();

        assert!(current.get_edge(edge_id).is_err());
        let head_id = historical.get_current_edge_version(edge_id).unwrap();
        let head = historical.get_edge_version(head_id).unwrap();
        assert_eq!(head.temporal.valid_time().start(), valid_from);
        assert_eq!(head.temporal.valid_time().end(), valid_to);

        let probe_before = crate::core::hlc::HybridTimestamp::new(now - 2_700_000_000, 0).unwrap();
        assert!(
            historical
                .get_edge_at_time(edge_id, probe_before, time::now())
                .is_ok()
        );
        assert!(
            historical
                .get_edge_at_time(edge_id, valid_to, time::now())
                .is_err()
        );
    }

    #[test]
    fn replay_constraint_declarations_declare_survives() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let label = GLOBAL_INTERNER.intern("RcvSurviveLabel").unwrap();
        let prop = GLOBAL_INTERNER.intern("rcvSurviveProp").unwrap();

        wal.append(WalOperation::DeclareUniqueConstraint {
            label,
            property: prop,
        })
        .unwrap();
        wal.flush().unwrap();

        let registry = ConstraintRegistry::new();
        replay_constraint_declarations_from_wal(&wal, &registry).unwrap();

        assert!(
            registry.is_constrained(label, prop),
            "declare without drop must leave constraint active after replay"
        );
    }
}
