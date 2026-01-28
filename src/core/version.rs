//! Version metadata shared between core and storage.

use crate::core::id::TxId;
use crate::core::temporal::Timestamp;

/// Metadata about version creation for Snapshot Isolation.
///
/// This tracks which transaction created a version and when it was committed,
/// enabling proper visibility checking for Snapshot Isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionMetadata {
    /// Transaction that created this version
    pub created_by_tx: TxId,

    /// When this version was committed (None if uncommitted)
    pub commit_timestamp: Option<Timestamp>,
}

impl VersionMetadata {
    /// Create new version metadata for a committed version.
    pub fn new(created_by_tx: TxId, commit_timestamp: Timestamp) -> Self {
        VersionMetadata {
            created_by_tx,
            commit_timestamp: Some(commit_timestamp),
        }
    }

    /// Create metadata for an uncommitted version.
    pub fn uncommitted(created_by_tx: TxId) -> Self {
        VersionMetadata {
            created_by_tx,
            commit_timestamp: None,
        }
    }

    /// Create default metadata for existing data (migration helper).
    pub fn default_for_existing() -> Self {
        use crate::core::hlc::HybridTimestamp;
        VersionMetadata {
            created_by_tx: TxId::new(0),
            // Phase 2: Use HybridTimestamp instead of integer literal
            commit_timestamp: Some(HybridTimestamp::new_unchecked(0, 0)),
        }
    }
}

impl Default for VersionMetadata {
    fn default() -> Self {
        Self::default_for_existing()
    }
}
