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
    ///
    /// Note: For historical versions reconstructed from storage (not currently in memory),
    /// this may be `TxId(0)` if the creating transaction ID was not preserved in the
    /// historical storage format.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_metadata_new() {
        let tx_id = TxId::new(100);
        let timestamp = Timestamp::from(5000);
        let metadata = VersionMetadata::new(tx_id, timestamp);

        assert_eq!(metadata.created_by_tx, tx_id);
        assert_eq!(metadata.commit_timestamp, Some(timestamp));
    }

    #[test]
    fn test_version_metadata_uncommitted() {
        let tx_id = TxId::new(200);
        let metadata = VersionMetadata::uncommitted(tx_id);

        assert_eq!(metadata.created_by_tx, tx_id);
        assert_eq!(metadata.commit_timestamp, None);
    }

    #[test]
    fn test_version_metadata_default() {
        let metadata = VersionMetadata::default();
        let default_expected = VersionMetadata::default_for_existing();

        assert_eq!(metadata.created_by_tx, default_expected.created_by_tx);
        assert_eq!(metadata.commit_timestamp, default_expected.commit_timestamp);
        assert_eq!(metadata.created_by_tx, TxId::new(0));
        assert!(metadata.commit_timestamp.is_some());
    }

    #[test]
    fn test_version_metadata_debug() {
        let tx_id = TxId::new(123);
        let timestamp = Timestamp::from(456);
        let metadata = VersionMetadata::new(tx_id, timestamp);
        let debug_str = format!("{:?}", metadata);

        assert!(debug_str.contains("VersionMetadata"));
        assert!(debug_str.contains("created_by_tx"));
        assert!(debug_str.contains("commit_timestamp"));
        assert!(debug_str.contains("123"));
    }

    #[test]
    fn test_version_metadata_clone_copy() {
        let tx_id = TxId::new(123);
        let timestamp = Timestamp::from(456);
        let metadata = VersionMetadata::new(tx_id, timestamp);

        let copy = metadata; // Copy
        assert_eq!(metadata, copy);

        #[allow(clippy::clone_on_copy)]
        let clone = metadata.clone(); // Clone
        assert_eq!(metadata, clone);
    }
}
