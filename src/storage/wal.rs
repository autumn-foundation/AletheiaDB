//! Write-Ahead Log (WAL) implementation for crash recovery and durability.
//!
//! The WAL provides:
//! - Sequential logging of all mutations
//! - Crash recovery by replaying operations
//! - Point-in-time recovery capabilities
//! - Configurable durability modes for performance tuning
//!
//! # Architecture
//!
//! GallifreyDB uses a **Concurrent WAL with Striped Lock-Free Ring Buffers** for
//! high-throughput write operations while maintaining ACID compliance.
//!
//! ```text
//!                     ┌─────────────────────┐
//!                     │    LSN Allocator    │
//!                     │  AtomicU64::fetch_add
//!                     └──────────┬──────────┘
//!                                │
//!        ┌───────────────────────┼───────────────────────┐
//!        ▼                       ▼                       ▼
//! ┌─────────────┐         ┌─────────────┐         ┌─────────────┐
//! │   Stripe 0  │         │   Stripe 1  │         │  Stripe N   │
//! │ Ring Buffer │         │ Ring Buffer │         │ Ring Buffer │
//! │ (Lock-free) │         │ (Lock-free) │         │ (Lock-free) │
//! └──────┬──────┘         └──────┬──────┘         └──────┬──────┘
//!        └───────────────────────┼───────────────────────┘
//!                                ▼
//!                     ┌─────────────────────┐
//!                     │  Flush Coordinator  │
//!                     │  - Sorts by LSN     │
//!                     │  - Writes segment   │
//!                     └─────────────────────┘
//! ```
//!
//! # Key Design Principles
//!
//! 1. **Lock-free append path**: Multiple threads can append concurrently without mutex contention
//! 2. **Global LSN ordering**: Single atomic counter ensures total ordering of all operations
//! 3. **Sorted flush**: Entries are sorted by LSN before writing to disk
//! 4. **ACID preserved**: Synchronous and GroupCommit modes remain fully ACID compliant
//!
//! # Durability Modes
//!
//! | Mode | Latency | Throughput | ACID |
//! |------|---------|------------|------|
//! | Synchronous | ~1.5ms | ~600/sec | ✅ Full |
//! | GroupCommit | ~10-50ms | ~100K+/sec | ✅ Full |
//! | Async | <100ns | ~500K+/sec | ❌ Eventual |
//!
//! See [`DurabilityMode`] for details.
//!
//! # Usage
//!
//! ```ignore
//! use gallifreydb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
//!
//! let config = ConcurrentWalSystemConfig::new("data/wal");
//! let wal = ConcurrentWalSystem::new(config)?;
//!
//! // Async append (returns immediately)
//! let lsn = wal.append_async(operation)?;
//!
//! // Commit with configured durability
//! wal.commit()?;
//!
//! // Shutdown gracefully
//! wal.shutdown();
//! ```

// Durability mode support
pub mod durability;
pub mod group_commit;

// Concurrent WAL modules
pub mod concurrent;
pub mod concurrent_system;
pub mod flush_coordinator;
pub mod lsn_allocator;
pub mod ring_buffer;
pub mod segment_reader;
pub mod stripe;

// Re-export key types
pub use durability::{DurabilityMode, WriteOptions};
pub use group_commit::GroupCommitCoordinator;

use crate::core::{
    id::{EdgeId, NodeId, VersionId},
    property::PropertyMap,
    temporal::{BiTemporalInterval, Timestamp, time},
};
use std::path::PathBuf;

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
        /// The node label
        label: String,
        /// The node properties
        properties: PropertyMap,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Create a new edge
    CreateEdge {
        /// The edge ID
        edge_id: EdgeId,
        /// The source node ID
        source: NodeId,
        /// The target node ID
        target: NodeId,
        /// The edge label
        label: String,
        /// The edge properties
        properties: PropertyMap,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Update node (creates new version)
    UpdateNode {
        /// The node ID
        node_id: NodeId,
        /// The version ID
        version_id: VersionId,
        /// The new label
        label: String,
        /// The new properties
        properties: PropertyMap,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Update edge (creates new version)
    UpdateEdge {
        /// The edge ID
        edge_id: EdgeId,
        /// The version ID
        version_id: VersionId,
        /// The new label
        label: String,
        /// The new properties
        properties: PropertyMap,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Delete a node
    DeleteNode {
        /// The node ID
        node_id: NodeId,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Delete an edge
    DeleteEdge {
        /// The edge ID
        edge_id: EdgeId,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
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
        // Extract checksum from data (stored at bytes 16-20)
        if serialized_data.len() < 20 {
            return false;
        }
        let stored_checksum = u32::from_le_bytes([
            serialized_data[16],
            serialized_data[17],
            serialized_data[18],
            serialized_data[19],
        ]);

        // Compute checksum over everything except the checksum field itself
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&serialized_data[0..16]); // LSN + timestamp
        hasher.update(&serialized_data[20..]); // Operation data
        let computed = hasher.finalize();

        stored_checksum == computed
    }
}

/// Configuration for WAL behavior
#[derive(Debug, Clone)]
pub struct WalConfig {
    /// Directory where WAL files are stored
    pub wal_dir: PathBuf,
    /// Maximum size of a WAL segment before rotation (in bytes)
    pub segment_size: usize,
    /// Number of WAL segments to keep for recovery
    pub segments_to_retain: usize,
    /// Durability mode controlling when data is synced to disk.
    ///
    /// This determines the tradeoff between durability guarantees and
    /// performance. See [`DurabilityMode`] for details.
    pub durability_mode: DurabilityMode,
}

impl Default for WalConfig {
    fn default() -> Self {
        WalConfig {
            wal_dir: PathBuf::from("gallifreydb/wal"),
            segment_size: 64 * 1024 * 1024, // 64MB
            segments_to_retain: 10,
            // GroupCommit by default: ACID-compliant with much better performance
            // than Synchronous. Use Synchronous only for critical financial transactions
            // that need minimum individual transaction latency.
            durability_mode: DurabilityMode::group_commit_default(),
        }
    }
}

impl WalConfig {
    /// Create a new WalConfig with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the durability mode.
    pub fn with_durability_mode(mut self, mode: DurabilityMode) -> Self {
        self.durability_mode = mode;
        self
    }

    /// Set the WAL directory.
    pub fn with_wal_dir(mut self, dir: PathBuf) -> Self {
        self.wal_dir = dir;
        self
    }

    /// Set the segment size.
    pub fn with_segment_size(mut self, size: usize) -> Self {
        self.segment_size = size;
        self
    }

    /// Set the number of segments to retain.
    pub fn with_segments_to_retain(mut self, count: usize) -> Self {
        self.segments_to_retain = count;
        self
    }
}

// =============================================================================
// Serialization Helpers
// =============================================================================

use crate::utils::error::Result;

/// Helper to serialize a string into the buffer (length prefix + bytes)
#[inline(always)]
fn serialize_str(s: &str, buffer: &mut Vec<u8>) {
    buffer.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buffer.extend_from_slice(s.as_bytes());
}

/// Serialize a WAL entry with CRC32 checksum into the provided buffer
///
/// This function reuses the provided buffer to avoid per-entry allocation.
/// The caller should clear the buffer before calling this function to maintain its capacity.
pub(crate) fn serialize_entry_into(entry: &WalEntry, buffer: &mut Vec<u8>) -> Result<()> {
    // Write LSN (8 bytes)
    buffer.extend_from_slice(&entry.lsn.0.to_le_bytes());

    // Write timestamp (8 bytes)
    buffer.extend_from_slice(&entry.timestamp.to_le_bytes());

    // Reserve space for checksum (4 bytes) - will fill in later
    let checksum_offset = buffer.len();
    buffer.extend_from_slice(&[0u8; 4]);

    // Write operation type and data with full serialization
    match &entry.operation {
        WalOperation::CreateNode {
            node_id,
            label,
            properties,
            temporal,
        } => {
            buffer.push(1); // operation type
            buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
            serialize_str(label, buffer);
            properties.serialize_into(buffer)?;
            temporal.serialize_into(buffer);
        }
        WalOperation::CreateEdge {
            edge_id,
            source,
            target,
            label,
            properties,
            temporal,
        } => {
            buffer.push(2); // operation type
            buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
            buffer.extend_from_slice(&source.as_u64().to_le_bytes());
            buffer.extend_from_slice(&target.as_u64().to_le_bytes());
            serialize_str(label, buffer);
            properties.serialize_into(buffer)?;
            temporal.serialize_into(buffer);
        }
        WalOperation::UpdateNode {
            node_id,
            version_id,
            label,
            properties,
            temporal,
        } => {
            buffer.push(3); // operation type
            buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
            buffer.extend_from_slice(&version_id.as_u64().to_le_bytes());
            serialize_str(label, buffer);
            properties.serialize_into(buffer)?;
            temporal.serialize_into(buffer);
        }
        WalOperation::UpdateEdge {
            edge_id,
            version_id,
            label,
            properties,
            temporal,
        } => {
            buffer.push(4); // operation type
            buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
            buffer.extend_from_slice(&version_id.as_u64().to_le_bytes());
            serialize_str(label, buffer);
            properties.serialize_into(buffer)?;
            temporal.serialize_into(buffer);
        }
        WalOperation::DeleteNode { node_id, temporal } => {
            buffer.push(6); // operation type
            buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
            temporal.serialize_into(buffer);
        }
        WalOperation::DeleteEdge { edge_id, temporal } => {
            buffer.push(7); // operation type
            buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
            temporal.serialize_into(buffer);
        }
        WalOperation::Checkpoint { lsn, timestamp } => {
            buffer.push(5); // operation type
            buffer.extend_from_slice(&lsn.0.to_le_bytes());
            buffer.extend_from_slice(&timestamp.to_le_bytes());
        }
    }

    // Compute CRC32 over everything except the checksum field
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&buffer[0..checksum_offset]); // LSN + timestamp
    hasher.update(&buffer[checksum_offset + 4..]); // Operation data
    let checksum = hasher.finalize();

    // Write the checksum into the reserved space
    buffer[checksum_offset..checksum_offset + 4].copy_from_slice(&checksum.to_le_bytes());

    Ok(())
}
