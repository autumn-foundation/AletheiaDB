//! Error types for the core module.

use thiserror::Error;

/// Errors occurring within the core module.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CoreError {
    /// Invalid ID value (out of range or reserved).
    #[error(
        "Invalid {id_type} ID {id}: exceeds maximum allowed value {max} (reserved range for internal use)",
        max = crate::core::id::MAX_VALID_ID
    )]
    InvalidId {
        /// The invalid ID value
        id: u64,
        /// The type of ID (node/edge/version)
        id_type: &'static str,
    },

    /// Corrupted data detected during serialization/deserialization.
    #[error("Corrupted data: {0}")]
    CorruptedData(String),

    /// Invalid vector data.
    #[error("Invalid vector: {reason}")]
    InvalidVector {
        /// Reason why the vector is invalid
        reason: String,
    },

    /// Vector dimension exceeds maximum allowed.
    #[error("Vector dimension {dimension} exceeds maximum allowed {max_allowed}")]
    DimensionTooLarge {
        /// The actual dimension
        dimension: usize,
        /// The maximum allowed dimension
        max_allowed: usize,
    },
}
