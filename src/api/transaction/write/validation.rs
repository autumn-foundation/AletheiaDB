//! Write validation and constraint checks.
//!
//! This module provides the logic to validate all buffered writes before
//! they are committed to the Write-Ahead Log (WAL) or applied to the
//! storage engine.
//!
//! # Validation Rules
//!
//! The following checks are enforced on every commit:
//!
//! 1.  **Temporal Consistency**:
//!     *   `valid_from` cannot be set arbitrarily far into the future (limited by `MAX_VALID_TIME_FUTURE_OFFSET_US`).
//!     *   `valid_from` for an update or delete must be *after* the entity's initial creation time.
//! 2.  **Referential Integrity**:
//!     *   New or updated edges must reference source and target nodes that either already exist
//!         in storage or are being created in the same transaction.
//!     *   Nodes cannot be referenced by edges if they are being deleted in the same transaction.

use super::{MAX_VALID_TIME_FUTURE_OFFSET_US, WriteTransaction};
use crate::core::error::{Result, TransactionError};
use crate::core::temporal::{Timestamp, time};

/// Validate that valid_from is not too far in the future.
///
/// Limits how far users can set valid_from into the future to prevent
/// abuse and maintain temporal query semantics.
pub(crate) fn validate_valid_from_future(valid_from: Timestamp) -> Result<()> {
    let current = time::now();
    let future_offset = valid_from.wallclock() as i128 - current.wallclock() as i128;

    if future_offset > MAX_VALID_TIME_FUTURE_OFFSET_US as i128 {
        return Err(crate::core::error::TemporalError::ValidTimeTooFarInFuture {
            valid_from,
            current_time: current,
            max_future_offset_us: MAX_VALID_TIME_FUTURE_OFFSET_US,
        }
        .into());
    }
    Ok(())
}

/// Validate that valid_from is not before entity creation for updates/deletes.
///
/// For updates and deletes, valid_from must be >= the entity's original
/// creation time to maintain temporal consistency.
pub(crate) fn validate_valid_from_not_before_creation(
    entity_id: &str,
    entity_creation_time: Timestamp,
    valid_from: Timestamp,
) -> Result<()> {
    if valid_from < entity_creation_time {
        return Err(
            crate::core::error::TemporalError::ValidTimeBeforeEntityCreation {
                valid_from,
                entity_creation_time,
                entity_id: entity_id.to_string(),
            }
            .into(),
        );
    }
    Ok(())
}

/// Validate all buffered writes.
///
/// Checks:
/// - Referential integrity (edges reference valid nodes)
/// - No constraint violations
pub(crate) fn validate(tx: &WriteTransaction) -> Result<()> {
    for write in tx.buffer.operations() {
        match write {
            crate::api::transaction::BufferedWrite::CreateEdge { source, target, .. }
            | crate::api::transaction::BufferedWrite::UpdateEdge { source, target, .. } => {
                // Check that source and target nodes exist
                // They might exist in current storage or be created in this transaction
                let node_exists = |node_id: &crate::core::NodeId| -> bool {
                    if tx.buffer.has_modified_node(*node_id) {
                        // Check buffered operation - fail if deleted
                        match tx.buffer.get_node_write(*node_id) {
                            Some(crate::api::transaction::BufferedWrite::DeleteNode { .. }) => {
                                false
                            }
                            _ => true, // Created or Updated means it exists
                        }
                    } else {
                        // Check storage
                        tx.current.get_node(*node_id).is_ok()
                    }
                };

                if !node_exists(source) {
                    return Err(TransactionError::ValidationFailed {
                        reason: format!("Edge source node {:?} does not exist", source),
                    }
                    .into());
                }

                if !node_exists(target) {
                    return Err(TransactionError::ValidationFailed {
                        reason: format!("Edge target node {:?} does not exist", target),
                    }
                    .into());
                }
            }
            _ => {
                // Other operations don't need validation
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::temporal::time;

    #[test]
    fn should_return_error_when_valid_from_too_far_in_future() {
        let current = time::now();
        // 10 years in the future is > MAX_VALID_TIME_FUTURE_OFFSET_US (which is usually a few hours or days)
        let future_time = current.wallclock() + (MAX_VALID_TIME_FUTURE_OFFSET_US * 2);
        let valid_from =
            HybridTimestamp::new(future_time, 0).expect("should create future timestamp");

        let result = validate_valid_from_future(valid_from);
        assert!(
            result.is_err(),
            "Expected error when valid_from is too far in future"
        );
        if let Err(e) = result {
            assert!(
                e.to_string().contains("too far in future"),
                "{}",
                e.to_string()
            );
        }
    }

    #[test]
    fn should_succeed_when_valid_from_is_now() {
        let current = time::now();
        let result = validate_valid_from_future(current);
        assert!(result.is_ok(), "Expected success for current time");
    }

    #[test]
    fn should_return_error_when_valid_from_is_before_creation() {
        let creation_time = HybridTimestamp::new(2000, 0).expect("should create timestamp");
        let valid_from = HybridTimestamp::new(1000, 0).expect("should create timestamp");

        let result = validate_valid_from_not_before_creation("node1", creation_time, valid_from);
        assert!(
            result.is_err(),
            "Expected error when valid_from is before creation time"
        );
        if let Err(e) = result {
            assert!(
                e.to_string().contains("before entity creation"),
                "{}",
                e.to_string()
            );
        }
    }

    #[test]
    fn should_succeed_when_valid_from_is_after_creation() {
        let creation_time = HybridTimestamp::new(1000, 0).expect("should create timestamp");
        let valid_from = HybridTimestamp::new(2000, 0).expect("should create timestamp");

        let result = validate_valid_from_not_before_creation("node1", creation_time, valid_from);
        assert!(
            result.is_ok(),
            "Expected success when valid_from is after creation time"
        );
    }
}
