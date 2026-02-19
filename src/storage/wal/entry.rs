//! WAL entry types and definitions.

use crate::core::{
    id::{EdgeId, NodeId, VersionId},
    interning::InternedString,
    property::PropertyMap,
    temporal::{Timestamp, time},
};

/// Maximum size of a single WAL entry in bytes (64 MB).
///
/// This limit prevents DoS attacks where a malicious user constructs a huge
/// entry (e.g., 1GB PropertyMap) to exhaust memory in the WAL ring buffer.
/// Since the ring buffer has fixed slot count (default 1024), unbounded entry
/// size would allow unbounded memory usage (1024 * 1GB = 1TB).
///
/// The limit is set to 64MB to match the default segment size. Entries larger
/// than this would force immediate segment rotation anyway.
pub const MAX_WAL_ENTRY_SIZE: usize = 64 * 1024 * 1024;

/// Log Sequence Number - monotonically increasing identifier for WAL entries
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LSN(pub u64);

impl LSN {
    /// Create the first LSN
    pub fn initial() -> Self {
        LSN(1)
    }

    /// Get the next LSN
    pub fn next(&self) -> Self {
        LSN(self.0 + 1)
    }
}

/// WAL operation types
#[derive(Debug, Clone)]
pub enum WalOperation {
    /// Create a new node
    CreateNode {
        /// The node ID
        node_id: NodeId,
        /// The node label (interned for efficiency)
        label: InternedString,
        /// The node properties
        properties: PropertyMap,
        /// When the node became valid in reality (user-controlled)
        valid_from: Timestamp,
    },
    /// Create a new edge
    CreateEdge {
        /// The edge ID
        edge_id: EdgeId,
        /// The source node ID
        source: NodeId,
        /// The target node ID
        target: NodeId,
        /// The edge label (interned for efficiency)
        label: InternedString,
        /// The edge properties
        properties: PropertyMap,
        /// When the edge became valid in reality (user-controlled)
        valid_from: Timestamp,
    },
    /// Update node (creates new version)
    UpdateNode {
        /// The node ID
        node_id: NodeId,
        /// The version ID
        version_id: VersionId,
        /// The new label (interned for efficiency)
        label: InternedString,
        /// The new properties
        properties: PropertyMap,
        /// When this update became valid in reality (user-controlled)
        valid_from: Timestamp,
    },
    /// Update edge (creates new version)
    UpdateEdge {
        /// The edge ID
        edge_id: EdgeId,
        /// The version ID
        version_id: VersionId,
        /// The new label (interned for efficiency)
        label: InternedString,
        /// The new properties
        properties: PropertyMap,
        /// When this update became valid in reality (user-controlled)
        valid_from: Timestamp,
    },
    /// Delete a node
    DeleteNode {
        /// The node ID
        node_id: NodeId,
        /// When the deletion became valid (typically commit time)
        valid_from: Timestamp,
    },
    /// Delete an edge
    DeleteEdge {
        /// The edge ID
        edge_id: EdgeId,
        /// When the deletion became valid (typically commit time)
        valid_from: Timestamp,
    },
    /// Checkpoint marker - indicates a snapshot was taken
    Checkpoint {
        /// The LSN at checkpoint
        lsn: LSN,
        /// When the checkpoint was created
        timestamp: Timestamp,
    },
}

/// A single WAL entry.
///
/// # Binary Format
///
/// WAL entries are serialized to disk with the following layout (little-endian):
///
/// ```text
/// ┌──────────┬───────────┬───────────────┬───────────────┬──────────────────┐
/// │ LSN (8b) │ Time (12b)│ Checksum (4b) │ Op Type (1b)  │ Operation Data...│
/// └──────────┴───────────┴───────────────┴───────────────┴──────────────────┘
/// ```
///
/// - **LSN** (8 bytes): Log Sequence Number for ordering.
/// - **Time** (12 bytes): [`Timestamp`] (HybridTimestamp) - 8 bytes wallclock, 4 bytes logical.
/// - **Checksum** (4 bytes): CRC32 of the entry (excluding the checksum field itself).
/// - **Op Type** (1 byte): Tag identifying the [`WalOperation`] variant.
/// - **Operation Data**: Variable-length data specific to the operation type.
///
/// See `src/storage/wal/serialization.rs` for detailed serialization logic.
#[derive(Debug, Clone)]
pub struct WalEntry {
    /// Log sequence number - unique, monotonically increasing identifier.
    pub lsn: LSN,
    /// Timestamp when the entry was logged (Hybrid Logical Clock).
    pub timestamp: Timestamp,
    /// The actual database operation (CreateNode, UpdateEdge, etc.).
    pub operation: WalOperation,
    /// CRC32 checksum for data integrity verification.
    ///
    /// The checksum is computed over the entire serialized entry, excluding the checksum field itself.
    pub checksum: u32,
}

impl WalEntry {
    /// Create a new WAL entry with computed checksum
    pub fn new(lsn: LSN, operation: WalOperation) -> Self {
        let timestamp = time::now();
        // Checksum will be computed during serialization
        WalEntry {
            lsn,
            timestamp,
            operation,
            checksum: 0, // Will be set during serialization
        }
    }

    /// Verify the checksum against serialized data
    pub fn verify_checksum(&self, serialized_data: &[u8]) -> bool {
        // Phase 2: Checksum now at bytes 20-24 (LSN=8 + HybridTimestamp=12)
        if serialized_data.len() < 24 {
            return false;
        }
        let stored_checksum = u32::from_le_bytes([
            serialized_data[20],
            serialized_data[21],
            serialized_data[22],
            serialized_data[23],
        ]);

        // Compute checksum over everything except the checksum field itself
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&serialized_data[0..20]); // LSN + timestamp
        hasher.update(&serialized_data[24..]); // Operation data
        let computed = hasher.finalize();

        stored_checksum == computed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lsn() {
        let lsn = LSN::initial();
        assert_eq!(lsn.0, 1);
        let next = lsn.next();
        assert_eq!(next.0, 2);
    }

    #[test]
    fn test_wal_entry_new() {
        let lsn = LSN(100);
        let op = WalOperation::Checkpoint {
            lsn: LSN(50),
            timestamp: time::now(),
        };
        let entry = WalEntry::new(lsn, op);
        assert_eq!(entry.lsn, lsn);
        assert_eq!(entry.checksum, 0); // Initially 0
    }

    #[test]
    fn test_verify_checksum_short_data() {
        let lsn = LSN(1);
        let op = WalOperation::Checkpoint {
            lsn: LSN(1),
            timestamp: time::now(),
        };
        let entry = WalEntry::new(lsn, op);
        assert!(!entry.verify_checksum(&[0u8; 10])); // Less than 24 bytes
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;
    use crate::storage::wal::serialization::serialize_entry_into;

    #[test]
    fn test_verify_checksum_success() {
        let lsn = LSN(123);
        let op = WalOperation::Checkpoint {
            lsn: LSN(456),
            timestamp: crate::core::temporal::time::now(),
        };
        let entry = WalEntry::new(lsn, op);

        // Serialize
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).expect("Serialization failed");

        // Verify checksum
        // This is the CRITICAL missing test: positive verification
        assert!(entry.verify_checksum(&buffer), "Checksum verification failed for valid entry");
    }
}
