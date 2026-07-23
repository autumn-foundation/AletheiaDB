//! Write conflict detection for Snapshot Isolation.
//!
//! This module implements the "First-Committer-Wins" rule, ensuring that two
//! concurrent transactions cannot modify the same entity.
//!
//! # Snapshot Isolation
//!
//! In Snapshot Isolation, a transaction T1 must abort if it attempts to modify
//! an entity that was modified by another transaction T2 that committed after
//! T1 started.
//!
//! # Mechanism
//!
//! We iterate through all entities modified in the write buffer and check their
//! current version in storage. If the current version's `commit_timestamp` is
//! greater than our `snapshot_timestamp`, a conflict has occurred.

use super::WriteTransaction;
use crate::api::transaction::BufferedWrite;
use crate::core::error::{Result, TransactionError};
use crate::core::id::{EdgeId, NodeId};
use std::collections::HashSet;

/// Detect write-write conflicts for Snapshot Isolation.
///
/// Checks if any entity modified by this transaction has been committed
/// by another transaction after our snapshot was taken. This implements
/// the First-Committer-Wins rule of Snapshot Isolation.
///
/// # Algorithm
///
/// For each operation in the write buffer:
/// 1.  Look up the entity in `CurrentStorage`.
/// 2.  If the entity exists, check its `commit_timestamp`.
/// 3.  If `commit_timestamp > snapshot_timestamp`, conflict!
/// 4.  If the entity doesn't exist but we are updating it, it implies deletion by another transaction. Conflict!
///
/// # Errors
///
/// Returns `TransactionError::SerializationFailure` if a write-write conflict is detected.
pub(crate) fn detect_conflicts(tx: &WriteTransaction) -> Result<()> {
    // Pre-lock CAS fast-path (Issue #3577): reject a PURE compare-and-set whose
    // expected version already fails to match the committed head, BEFORE any WAL
    // frame is appended+fsync'd at commit. This runs in the same guard-free
    // context as the snapshot-isolation check below (it only READS current
    // storage; no `historical.write()` guard is held), so it adds no new lock
    // site and preserves the lock-acquisition order.
    //
    // This is a best-effort fast-path, NOT a replacement for the authoritative
    // under-guard re-check in `cas::detect_cas_precondition_violations`:
    //
    //   * Only PURE CAS (`lease: None`) is rejected here. A lease claim's
    //     OR-branch (`version == expected` OR `lease_until <= commit_ts`) can
    //     flip from false to true between this pre-lock read and the LATER
    //     commit timestamp (the lease expires relative to commit), so a pre-lock
    //     reject of a lease claim would be WRONG — lease claims fall through
    //     untouched to the authoritative under-guard check.
    //   * The committed head advances monotonically, so a pure-CAS pre-lock
    //     mismatch can never become a match at commit: rejecting it now is
    //     sound. Two concurrent pure-CAS that both read `head == expected` here
    //     both pass the fast-path; the under-guard check still catches the loser
    //     (first-committer-wins).
    //
    // This is now purely an optimization: since Issue #3413 the authoritative
    // re-check in `cas::detect_cas_precondition_violations` also runs BEFORE the
    // WAL append (under `current_timestamp`), so a rejected CAS never writes a
    // durable frame regardless of this fast-path. The fast-path still lets the
    // common single-threaded stale-CAS case bail out earlier — before acquiring
    // `current_timestamp` and building the WAL batch — rather than at the
    // commit-clock re-check.
    for precondition in &tx.cas_preconditions {
        // Lease claims MUST reach the authoritative under-guard check unchanged.
        // A fenced claim (DBOS Phase 3e) always carries a lease today
        // (`claim_with_lease_fenced_impl` sets both `lease` and `fence`), so this
        // `lease.is_some()` skip already routes every fenced claim to the
        // authoritative under-guard fence check — the pre-lock fast-path never
        // sees (and so cannot mis-reject on a stale read) a fence. A future
        // fence-ONLY claim variant (`lease: None, fence: Some(_)`) would slip
        // past this guard into the pure-CAS branch below and MUST revisit this:
        // it would need its own `fence.is_none()` clause here so it, too, falls
        // through to the under-guard fence gate.
        if precondition.lease.is_some() {
            continue;
        }
        match precondition.target {
            super::cas::CasTarget::Node(node_id) => {
                let actual = tx.current.get_node(node_id).ok().map(|n| n.current_version);
                if actual != Some(precondition.expected_version) {
                    return Err(TransactionError::CasMismatch {
                        expected: precondition.expected_version,
                        actual,
                    }
                    .into());
                }
            }
            super::cas::CasTarget::Edge(edge_id) => {
                let actual = tx.current.get_edge(edge_id).ok().map(|e| e.current_version);
                if actual != Some(precondition.expected_version) {
                    return Err(TransactionError::CasMismatch {
                        expected: precondition.expected_version,
                        actual,
                    }
                    .into());
                }
            }
        }
    }

    // Entities CREATED in this same transaction cannot conflict with another
    // transaction (Issue #3417): their ids were freshly allocated by this
    // tx's id generators, so no other committed transaction can hold a version
    // for them. A later update/delete/retract of a same-tx-created entity
    // reads its buffered create (read-your-own-writes), which means the
    // absence of a *committed* row is expected — not a concurrent deletion.
    // Without this skip, the "missing committed row => deleted by another tx"
    // branches below would raise a spurious SerializationFailure for a
    // create-then-update/delete/retract in one transaction.
    let mut created_nodes: HashSet<NodeId> = HashSet::new();
    let mut created_edges: HashSet<EdgeId> = HashSet::new();
    for write in tx.buffer.operations() {
        match write {
            BufferedWrite::CreateNode { node_id, .. } => {
                created_nodes.insert(*node_id);
            }
            BufferedWrite::CreateEdge { edge_id, .. } => {
                created_edges.insert(*edge_id);
            }
            _ => {}
        }
    }

    // Compare-and-set targets (Issue #3577) are DELIBERATELY excluded from the
    // pre-lock snapshot-isolation check: their conflict semantics is the
    // version-based CAS precondition, re-checked under the `historical.write()`
    // guard (`cas::detect_cas_precondition_violations`). Applying
    // first-committer-wins here too would preempt that authoritative check and
    // surface a retriable `SerializationFailure` for a lost claim that must
    // instead be the non-retriable `CasMismatch`.
    let cas_nodes = tx.cas_target_nodes();
    let cas_edges = tx.cas_target_edges();

    for write in tx.buffer.operations() {
        match write {
            // UpdateNode: check if node was modified or deleted after our snapshot
            crate::api::transaction::BufferedWrite::UpdateNode { node_id, .. } => {
                if created_nodes.contains(node_id) || cas_nodes.contains(node_id) {
                    continue;
                }
                match tx.current.get_node(*node_id) {
                    Ok(current_node) => {
                        // Node exists - check if it was modified after our snapshot
                        if let Some(commit_ts) = current_node.metadata.commit_timestamp
                            && commit_ts > tx.snapshot.snapshot_timestamp
                        {
                            return Err(TransactionError::SerializationFailure {
                                entity: format!("{:?}", node_id),
                                reason: format!(
                                    "Version committed at {} after snapshot at {}",
                                    commit_ts, tx.snapshot.snapshot_timestamp
                                ),
                            }
                            .into());
                        }
                    }
                    Err(_) => {
                        // Node doesn't exist in current storage. Since we successfully
                        // called update_node() earlier (which reads from current storage),
                        // the node must have been deleted by another transaction.
                        return Err(TransactionError::SerializationFailure {
                            entity: format!("{:?}", node_id),
                            reason: "Node was deleted by another transaction".to_string(),
                        }
                        .into());
                    }
                }
            }

            // UpdateEdge: check if edge was modified or deleted after our snapshot
            crate::api::transaction::BufferedWrite::UpdateEdge { edge_id, .. } => {
                if created_edges.contains(edge_id) || cas_edges.contains(edge_id) {
                    continue;
                }
                match tx.current.get_edge(*edge_id) {
                    Ok(current_edge) => {
                        // Edge exists - check if it was modified after our snapshot
                        if let Some(commit_ts) = current_edge.metadata.commit_timestamp
                            && commit_ts > tx.snapshot.snapshot_timestamp
                        {
                            return Err(TransactionError::SerializationFailure {
                                entity: format!("{:?}", edge_id),
                                reason: format!(
                                    "Version committed at {} after snapshot at {}",
                                    commit_ts, tx.snapshot.snapshot_timestamp
                                ),
                            }
                            .into());
                        }
                    }
                    Err(_) => {
                        // Edge doesn't exist - it was deleted by another transaction
                        return Err(TransactionError::SerializationFailure {
                            entity: format!("{:?}", edge_id),
                            reason: "Edge was deleted by another transaction".to_string(),
                        }
                        .into());
                    }
                }
            }

            // DeleteNode: check if node was modified after our snapshot
            crate::api::transaction::BufferedWrite::DeleteNode { node_id, .. } => {
                if created_nodes.contains(node_id) {
                    continue;
                }
                // Get current version from storage
                if let Ok(current_node) = tx.current.get_node(*node_id)
                    && let Some(commit_ts) = current_node.metadata.commit_timestamp
                    && commit_ts > tx.snapshot.snapshot_timestamp
                {
                    return Err(TransactionError::SerializationFailure {
                        entity: format!("{:?}", node_id),
                        reason: format!(
                            "Version committed at {} after snapshot at {}",
                            commit_ts, tx.snapshot.snapshot_timestamp
                        ),
                    }
                    .into());
                }
            }

            // DeleteEdge: check if edge was modified after our snapshot
            crate::api::transaction::BufferedWrite::DeleteEdge { edge_id, .. } => {
                if created_edges.contains(edge_id) {
                    continue;
                }
                // Get current version from storage
                if let Ok(current_edge) = tx.current.get_edge(*edge_id)
                    && let Some(commit_ts) = current_edge.metadata.commit_timestamp
                    && commit_ts > tx.snapshot.snapshot_timestamp
                {
                    return Err(TransactionError::SerializationFailure {
                        entity: format!("{:?}", edge_id),
                        reason: format!(
                            "Version committed at {} after snapshot at {}",
                            commit_ts, tx.snapshot.snapshot_timestamp
                        ),
                    }
                    .into());
                }
            }

            // RetractNode: like UpdateNode, the entity must still exist and
            // be unmodified since our snapshot. A concurrent delete/retract
            // (entity gone) or a concurrent update (newer commit) both
            // invalidate the valid_from this retraction was validated against.
            crate::api::transaction::BufferedWrite::RetractNode { node_id, .. } => {
                if created_nodes.contains(node_id) {
                    continue;
                }
                match tx.current.get_node(*node_id) {
                    Ok(current_node) => {
                        if let Some(commit_ts) = current_node.metadata.commit_timestamp
                            && commit_ts > tx.snapshot.snapshot_timestamp
                        {
                            return Err(TransactionError::SerializationFailure {
                                entity: format!("{:?}", node_id),
                                reason: format!(
                                    "Version committed at {} after snapshot at {}",
                                    commit_ts, tx.snapshot.snapshot_timestamp
                                ),
                            }
                            .into());
                        }
                    }
                    Err(_) => {
                        return Err(TransactionError::SerializationFailure {
                            entity: format!("{:?}", node_id),
                            reason: "Node was deleted by another transaction".to_string(),
                        }
                        .into());
                    }
                }
            }

            // RetractEdge: same contract as RetractNode.
            crate::api::transaction::BufferedWrite::RetractEdge { edge_id, .. } => {
                if created_edges.contains(edge_id) {
                    continue;
                }
                match tx.current.get_edge(*edge_id) {
                    Ok(current_edge) => {
                        if let Some(commit_ts) = current_edge.metadata.commit_timestamp
                            && commit_ts > tx.snapshot.snapshot_timestamp
                        {
                            return Err(TransactionError::SerializationFailure {
                                entity: format!("{:?}", edge_id),
                                reason: format!(
                                    "Version committed at {} after snapshot at {}",
                                    commit_ts, tx.snapshot.snapshot_timestamp
                                ),
                            }
                            .into());
                        }
                    }
                    Err(_) => {
                        return Err(TransactionError::SerializationFailure {
                            entity: format!("{:?}", edge_id),
                            reason: "Edge was deleted by another transaction".to_string(),
                        }
                        .into());
                    }
                }
            }

            // CreateNode and CreateEdge don't need conflict detection
            // since they're creating new entities that didn't exist before
            _ => {}
        }
    }

    Ok(())
}
