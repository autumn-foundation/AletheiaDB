use super::{MAX_VALID_TIME_FUTURE_OFFSET_US, WriteTransaction};
use crate::core::temporal::{Timestamp, time};
use crate::utils::error::{Result, TransactionError};

/// Validate that valid_from is not too far in the future.
///
/// Limits how far users can set valid_from into the future to prevent
/// abuse and maintain temporal query semantics.
pub(crate) fn validate_valid_from_future(valid_from: Timestamp) -> Result<()> {
    let current = time::now();
    let future_offset = valid_from.wallclock() as i128 - current.wallclock() as i128;

    if future_offset > MAX_VALID_TIME_FUTURE_OFFSET_US as i128 {
        return Err(
            crate::utils::error::TemporalError::ValidTimeTooFarInFuture {
                valid_from,
                current_time: current,
                max_future_offset_us: MAX_VALID_TIME_FUTURE_OFFSET_US,
            }
            .into(),
        );
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
            crate::utils::error::TemporalError::ValidTimeBeforeEntityCreation {
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
                if !tx.buffer.has_modified_node(*source) && tx.current.get_node(*source).is_err() {
                    return Err(TransactionError::ValidationFailed {
                        reason: format!("Edge source node {:?} does not exist", source),
                    }
                    .into());
                }
                if !tx.buffer.has_modified_node(*target) && tx.current.get_node(*target).is_err() {
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
