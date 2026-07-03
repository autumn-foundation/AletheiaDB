//! Uniqueness constraint enforcement on the write path.
//!
//! Called from `commit_with_timestamp_inner` after conflict detection and
//! before acquiring the timestamp lock.  Returns an RAII `ReservationGuard`
//! that auto-rolls back on drop if the commit fails later.

use crate::core::constraint::{ConstraintRegistry, ReservationGuard};
use crate::core::error::ConstraintError;
use crate::core::graph::Node;
use crate::core::id::NodeId;
use crate::core::interning::InternedString;
use crate::core::property::PropertyMap;
use std::sync::Arc;

use super::WriteTransaction;
use crate::api::transaction::write_buffer::BufferedWrite;

/// Enforce uniqueness constraints for the transaction's buffered node operations.
///
/// Returns a `ReservationGuard` that:
/// - On drop (rollback path): releases all added reservations from the index.
/// - After calling `guard.commit()` (success path): removes old-value reservations
///   freed by updates/deletes.
pub(crate) fn check_constraints(
    tx: &WriteTransaction,
    registry: &Arc<ConstraintRegistry>,
) -> std::result::Result<ReservationGuard, ConstraintError> {
    // ⚡ Bolt Optimization: Pre-allocate vectors based on buffer size to prevent heap reallocations during constraint checking on the hot commit path.

    // Borrow property maps directly from the buffer to avoid cloning on the hot path.
    let mut added_refs: Vec<(InternedString, &PropertyMap, NodeId)> =
        Vec::with_capacity(tx.buffer.len());

    // Pre-tx nodes whose constraint keys are freed on a successful commit.
    let mut removed_nodes: Vec<Node> = Vec::with_capacity(tx.buffer.len());

    for op in tx.buffer.operations() {
        match op {
            BufferedWrite::CreateNode {
                node_id,
                label,
                properties,
                ..
            } => {
                added_refs.push((*label, properties, *node_id));
            }
            BufferedWrite::UpdateNode {
                node_id,
                label,
                properties,
                ..
            } => {
                // Post-tx state is the new properties.
                added_refs.push((*label, properties, *node_id));
                // Pre-tx state: fetch current node to find keys being released.
                if let Ok(current_node) = tx.current.get_node(*node_id) {
                    removed_nodes.push(current_node);
                }
            }
            BufferedWrite::DeleteNode { node_id, .. } => {
                // All constraint keys for this node are freed on commit.
                if let Ok(current_node) = tx.current.get_node(*node_id) {
                    removed_nodes.push(current_node);
                }
            }
            _ => {}
        }
    }

    let removed_refs: Vec<(InternedString, &PropertyMap, NodeId)> = removed_nodes
        .iter()
        .map(|n| (n.label, &n.properties, n.id))
        .collect();

    registry.reserve_for_transaction(&added_refs, &removed_refs)
}
