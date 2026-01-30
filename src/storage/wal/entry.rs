//! WAL entry types and definitions.

use crate::core::{
    id::{EdgeId, NodeId, VersionId},
    interning::InternedString,
    property::PropertyMap,
    temporal::{Timestamp, time},
};

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

/// A single WAL entry
#[derive(Debug, Clone)]
pub struct WalEntry {
    /// Log sequence number
    pub lsn: LSN,
    /// Timestamp when logged
    pub timestamp: Timestamp,
    /// The operation to log
    pub operation: WalOperation,
    /// CRC32 checksum for corruption detection
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
