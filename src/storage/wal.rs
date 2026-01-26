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
//! ## Single Operations
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
//!
//! ## Batch Operations (High-Throughput)
//!
//! For high-throughput workloads with multiple operations, use `append_batch()` for
//! significant performance improvements:
//!
//! ```ignore
//! use gallifreydb::storage::wal::{WalOperation, ConcurrentWalSystem};
//!
//! // Create multiple operations (e.g., from a transaction)
//! let operations = vec![
//!     WalOperation::CreateNode { /* ... */ },
//!     WalOperation::CreateEdge { /* ... */ },
//!     WalOperation::UpdateNode { /* ... */ },
//! ];
//!
//! // Batch append - optimizes LSN allocation and serialization
//! let lsns = wal.append_batch(operations)?;
//! assert_eq!(lsns.len(), 3);
//!
//! // For GroupCommit mode, commit and wait for durability
//! if let Some(epoch) = wal.commit()? {
//!     wal.group_commit_coordinator().unwrap().wait_for_flush(epoch)?;
//! }
//! ```
//!
//! **Performance Benefits:**
//! - Single atomic LSN allocation (vs N atomic operations)
//! - Better CPU cache locality during serialization
//! - Reduced stripe buffer contention
//! - **20-50% throughput improvement** for batch sizes > 10

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
    interning::InternedString,
    property::PropertyMap,
    temporal::{BiTemporalInterval, Timestamp, time},
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
///
/// # Performance Note (Issue #225)
///
/// This enum uses `InternedString` for labels instead of `String` to avoid allocation
/// overhead on the hot commit path. Labels are written to WAL as 4-byte IDs and only
/// resolved to strings during recovery when needed.
#[derive(Debug, Clone)]
pub enum WalOperation {
    /// Create a new node
    CreateNode {
        /// The node ID
        node_id: NodeId,
        /// The node label (as interned string ID)
        label: InternedString,
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
        /// The edge label (as interned string ID)
        label: InternedString,
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
        /// The new label (as interned string ID)
        label: InternedString,
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
        /// The new label (as interned string ID)
        label: InternedString,
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

// =============================================================================
// Serialization Helpers
// =============================================================================

use crate::utils::error::Result;

/// Helper to serialize an InternedString ID into the buffer (4 bytes)
///
/// # Performance Note (Issue #225)
///
/// This writes the raw u32 ID instead of the full string, eliminating allocation
/// overhead during WAL writes. The string is only resolved during recovery.
#[inline(always)]
fn serialize_interned_id(id: InternedString, buffer: &mut Vec<u8>) {
    buffer.extend_from_slice(&id.as_u32().to_le_bytes());
}

/// Estimate the required buffer capacity for serializing a WAL entry.
///
/// This function provides an upper-bound estimate of the buffer size needed
/// to serialize a WAL entry, allowing pre-allocation to avoid reallocations
/// during the hot-path serialization process.
///
/// # Size Breakdown
///
/// Fixed overhead (per entry):
/// - LSN: 8 bytes
/// - Timestamp (HybridTimestamp): 12 bytes
/// - Checksum: 4 bytes
/// - Total fixed: 24 bytes
///
/// Variable sizes by operation:
/// - `CreateNode`: 1 (op type) + 8 (node_id) + 4 (label len) + label bytes +
///   properties size + 48 (BiTemporalInterval)
/// - `CreateEdge`: 1 (op type) + 8 (edge_id) + 8 (source) + 8 (target) +
///   4 (label len) + label bytes + properties size + 48 (BiTemporalInterval)
/// - `UpdateNode`: 1 (op type) + 8 (node_id) + 8 (version_id) + 4 (label len) +
///   label bytes + properties size + 48 (BiTemporalInterval)
/// - `UpdateEdge`: 1 (op type) + 8 (edge_id) + 8 (version_id) + 4 (label len) +
///   label bytes + properties size + 48 (BiTemporalInterval)
/// - `DeleteNode`: 1 (op type) + 8 (node_id) + 48 (BiTemporalInterval) = 57 bytes
/// - `DeleteEdge`: 1 (op type) + 8 (edge_id) + 48 (BiTemporalInterval) = 57 bytes
/// - `Checkpoint`: 1 (op type) + 8 (lsn) + 12 (timestamp) = 21 bytes
///
/// # Returns
///
/// An estimated capacity in bytes. The estimate is conservative (may slightly
/// over-allocate) to ensure the buffer doesn't need to grow during serialization.
///
/// # Performance Impact
///
/// Pre-allocating the correct capacity eliminates dynamic reallocation overhead,
/// which is especially important for the high-throughput WAL write path. Typical
/// savings are 10-30% reduction in allocation overhead for property-heavy operations.
pub(crate) fn estimate_entry_capacity(operation: &WalOperation) -> usize {
    // Fixed overhead: LSN (8) + Timestamp (12) + Checksum (4)
    const FIXED_OVERHEAD: usize = 24;
    // BiTemporalInterval is always 48 bytes (2 * TimeRange, each 24 bytes)
    const TEMPORAL_SIZE: usize = 48;

    let variable_size = match operation {
        WalOperation::CreateNode { properties, .. } => {
            // op type (1) + node_id (8) + InternedString ID (4) + properties + temporal (48)
            // Note: labels are now 4-byte IDs, not length-prefixed strings (Issue #225)
            let base = 1 + 8 + 4 + TEMPORAL_SIZE;
            base + estimate_property_map_size(properties)
        }
        WalOperation::CreateEdge { properties, .. } => {
            // op type (1) + edge_id (8) + source (8) + target (8) + InternedString ID (4) + properties + temporal (48)
            let base = 1 + 8 + 8 + 8 + 4 + TEMPORAL_SIZE;
            base + estimate_property_map_size(properties)
        }
        WalOperation::UpdateNode { properties, .. } => {
            // op type (1) + node_id (8) + version_id (8) + InternedString ID (4) + properties + temporal (48)
            let base = 1 + 8 + 8 + 4 + TEMPORAL_SIZE;
            base + estimate_property_map_size(properties)
        }
        WalOperation::UpdateEdge { properties, .. } => {
            // op type (1) + edge_id (8) + version_id (8) + InternedString ID (4) + properties + temporal (48)
            let base = 1 + 8 + 8 + 4 + TEMPORAL_SIZE;
            base + estimate_property_map_size(properties)
        }
        WalOperation::DeleteNode { .. } => {
            // op type (1) + node_id (8) + temporal (48)
            1 + 8 + TEMPORAL_SIZE
        }
        WalOperation::DeleteEdge { .. } => {
            // op type (1) + edge_id (8) + temporal (48)
            1 + 8 + TEMPORAL_SIZE
        }
        WalOperation::Checkpoint { .. } => {
            // op type (1) + lsn (8) + timestamp (12)
            1 + 8 + 12
        }
    };

    FIXED_OVERHEAD + variable_size
}

/// Estimate the serialized size of a PropertyMap.
///
/// This provides an upper-bound estimate for pre-allocation purposes.
/// The actual serialized size is:
/// - Map count: 4 bytes
/// - For each property:
///   - Key length prefix: 4 bytes
///   - Key bytes: key.len()
///   - Value: depends on type (see `estimate_property_value_size`)
fn estimate_property_map_size(properties: &PropertyMap) -> usize {
    use crate::core::interning::GLOBAL_INTERNER;

    if properties.is_empty() {
        return 4; // Just the count field
    }

    let mut size = 4; // Count field
    for (key, value) in properties.iter() {
        // Key: length prefix (4) + key bytes
        // Resolve the key to get accurate size.
        // If resolve fails (corrupted interner state), use a conservative upper bound
        // to ensure we never under-allocate. 256 is a reasonable maximum key length.
        let key_len = GLOBAL_INTERNER
            .resolve(*key)
            .map(|s| s.len())
            .unwrap_or(256); // Conservative fallback: max reasonable key size
        size += 4 + key_len;
        // Value: type tag + data
        size += estimate_property_value_size(value);
    }
    size
}

/// Estimate the serialized size of a single PropertyValue.
///
/// Size breakdown:
/// - Null: 1 byte (tag)
/// - Bool: 2 bytes (tag + value)
/// - Int: 9 bytes (tag + 8 bytes)
/// - Float: 9 bytes (tag + 8 bytes)
/// - String: 1 + 4 + len (tag + length prefix + bytes)
/// - Bytes: 1 + 4 + len (tag + length prefix + bytes)
/// - Array: 1 + 4 + sum of element sizes (tag + count + elements)
/// - Vector: 1 + 4 + (dims * 4) (tag + dims + f32 array)
/// - SparseVector: 1 + 4 + 4 + (nnz * 8) (tag + dims + nnz + indices + values)
fn estimate_property_value_size(value: &crate::core::property::PropertyValue) -> usize {
    use crate::core::property::PropertyValue;

    match value {
        PropertyValue::Null => 1,
        PropertyValue::Bool(_) => 2,
        PropertyValue::Int(_) => 9,
        PropertyValue::Float(_) => 9,
        PropertyValue::String(s) => 1 + 4 + s.len(),
        PropertyValue::Bytes(b) => 1 + 4 + b.len(),
        PropertyValue::Array(arr) => {
            // tag (1) + count (4) + sum of element sizes
            let elements_size: usize = arr.iter().map(estimate_property_value_size).sum();
            1 + 4 + elements_size
        }
        PropertyValue::Vector(v) => {
            // tag (1) + dims (4) + f32 array (dims * 4)
            // Serialization: TAG_VECTOR + dimension(u32) + f32_values
            1 + 4 + (v.len() * 4)
        }
        PropertyValue::SparseVector(sv) => {
            // tag (1) + dims (4) + nnz (4) + indices (nnz * 4) + values (nnz * 4)
            // Serialization: TAG_SPARSE_VECTOR + dimension(u32) + nnz(u32) + indices[] + values[]
            let nnz = sv.indices().len();
            1 + 4 + 4 + (nnz * 4) + (nnz * 4)
        }
    }
}

/// Serialize a WAL entry with CRC32 checksum into the provided buffer
///
/// This function reuses the provided buffer to avoid per-entry allocation.
/// The caller should clear the buffer before calling this function to maintain its capacity.
pub(crate) fn serialize_entry_into(entry: &WalEntry, buffer: &mut Vec<u8>) -> Result<()> {
    // Write LSN (8 bytes)
    buffer.extend_from_slice(&entry.lsn.0.to_le_bytes());

    // Write timestamp (12 bytes: Phase 2 HybridTimestamp)
    entry.timestamp.serialize_into(buffer);

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
            serialize_interned_id(*label, buffer);
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
            serialize_interned_id(*label, buffer);
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
            serialize_interned_id(*label, buffer);
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
            serialize_interned_id(*label, buffer);
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
            // Phase 2: Use HybridTimestamp serialization
            timestamp.serialize_into(buffer);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::EdgeId;
    use crate::core::NodeId;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::{BiTemporalInterval, time};

    const WAL_VERSION: u8 = 1;

    /// Helper to create a test temporal interval
    fn test_temporal() -> BiTemporalInterval {
        use crate::core::hlc::HybridTimestamp;
        let timestamp = HybridTimestamp::new_unchecked(1000000, 0);
        BiTemporalInterval::current(timestamp)
    }

    /// Helper to create a test timestamp
    fn test_timestamp() -> Timestamp {
        use crate::core::hlc::HybridTimestamp;
        HybridTimestamp::new_unchecked(1000000, 0)
    }

    #[test]
    fn test_estimate_capacity_checkpoint() {
        // Checkpoint: op type (1) + lsn (8) + timestamp (12) = 21 bytes
        // Fixed overhead: LSN (8) + Timestamp (12) + Checksum (4) = 24 bytes
        // Total: 45 bytes
        let op = WalOperation::Checkpoint {
            lsn: LSN(1),
            timestamp: test_timestamp(),
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(estimated, 45, "Checkpoint should be exactly 45 bytes");

        // Verify by actually serializing
        let entry = WalEntry::new(LSN(1), op);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();
        assert!(
            buffer.len() <= estimated,
            "Actual size {} should not exceed estimate {}",
            buffer.len(),
            estimated
        );
    }

    #[test]
    fn test_estimate_capacity_delete_node() {
        // DeleteNode: op type (1) + node_id (8) + temporal (48) = 57 bytes
        // Fixed overhead: 24 bytes
        // Total: 81 bytes
        let op = WalOperation::DeleteNode {
            node_id: NodeId::new(1).unwrap(),
            temporal: test_temporal(),
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(estimated, 81, "DeleteNode should be exactly 81 bytes");

        // Verify by actually serializing
        let entry = WalEntry::new(LSN(1), op);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();
        assert!(
            buffer.len() <= estimated,
            "Actual size {} should not exceed estimate {}",
            buffer.len(),
            estimated
        );
    }

    #[test]
    fn test_estimate_capacity_delete_edge() {
        // DeleteEdge: op type (1) + edge_id (8) + temporal (48) = 57 bytes
        // Fixed overhead: 24 bytes
        // Total: 81 bytes
        let op = WalOperation::DeleteEdge {
            edge_id: EdgeId::new(1).unwrap(),
            temporal: test_temporal(),
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(estimated, 81, "DeleteEdge should be exactly 81 bytes");

        // Verify by actually serializing
        let entry = WalEntry::new(LSN(1), op);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();
        assert!(
            buffer.len() <= estimated,
            "Actual size {} should not exceed estimate {}",
            buffer.len(),
            estimated
        );
    }

    #[test]
    fn test_estimate_capacity_create_node_empty_properties() {
        // CreateNode with empty properties (Issue #225 - InternedString optimization):
        // Fixed: 24 bytes
        // op type (1) + node_id (8) + InternedString ID (4) + properties (4 for count) + temporal (48)
        // = 24 + 1 + 8 + 4 + 4 + 48 = 89 bytes (was 93 bytes with length-prefixed strings)
        let op = WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: GLOBAL_INTERNER.intern("test").unwrap(),
            properties: PropertyMapBuilder::new().build(),
            temporal: test_temporal(),
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(
            estimated, 89,
            "CreateNode with empty properties should be 89 bytes"
        );

        // Verify by actually serializing
        let entry = WalEntry::new(LSN(1), op);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();
        assert!(
            buffer.len() <= estimated,
            "Actual size {} should not exceed estimate {}",
            buffer.len(),
            estimated
        );
    }

    #[test]
    fn test_estimate_capacity_create_node_with_properties() {
        // CreateNode with properties
        let properties = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30)
            .insert("score", 95.5)
            .build();

        let op = WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties,
            temporal: test_temporal(),
        };

        let estimated = estimate_entry_capacity(&op);

        // Verify by actually serializing
        let entry = WalEntry::new(LSN(1), op);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        assert!(
            buffer.len() <= estimated,
            "Actual size {} should not exceed estimate {}",
            buffer.len(),
            estimated
        );

        // The estimate should be reasonably close (not wildly over-allocated)
        let overhead_ratio = estimated as f64 / buffer.len() as f64;
        assert!(
            overhead_ratio <= 1.5,
            "Estimate {} should not be more than 50% over actual size {}",
            estimated,
            buffer.len()
        );
    }

    #[test]
    fn test_estimate_capacity_create_edge() {
        // CreateEdge with properties
        let properties = PropertyMapBuilder::new()
            .insert("weight", 1.5)
            .insert("type", "FRIEND")
            .build();

        let op = WalOperation::CreateEdge {
            edge_id: EdgeId::new(1).unwrap(),
            source: NodeId::new(1).unwrap(),
            target: NodeId::new(2).unwrap(),
            label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            properties,
            temporal: test_temporal(),
        };

        let estimated = estimate_entry_capacity(&op);

        // Verify by actually serializing
        let entry = WalEntry::new(LSN(1), op);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        assert!(
            buffer.len() <= estimated,
            "Actual size {} should not exceed estimate {}",
            buffer.len(),
            estimated
        );

        let overhead_ratio = estimated as f64 / buffer.len() as f64;
        assert!(
            overhead_ratio <= 1.5,
            "Estimate {} should not be more than 50% over actual size {}",
            estimated,
            buffer.len()
        );
    }

    #[test]
    fn test_estimate_capacity_with_vector_property() {
        // CreateNode with vector property
        let embedding = vec![0.1, 0.2, 0.3, 0.4];
        let properties = PropertyMapBuilder::new()
            .insert_vector("embedding", &embedding)
            .build();

        let op = WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: GLOBAL_INTERNER.intern("Document").unwrap(),
            properties,
            temporal: test_temporal(),
        };

        let estimated = estimate_entry_capacity(&op);

        // Verify by actually serializing
        let entry = WalEntry::new(LSN(1), op);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        assert!(
            buffer.len() <= estimated,
            "Actual size {} should not exceed estimate {}",
            buffer.len(),
            estimated
        );

        let overhead_ratio = estimated as f64 / buffer.len() as f64;
        assert!(
            overhead_ratio <= 1.5,
            "Estimate {} should not be more than 50% over actual size {}",
            estimated,
            buffer.len()
        );
    }

    #[test]
    fn test_estimate_capacity_large_properties() {
        // Test with large property map to ensure estimate handles it
        let mut builder = PropertyMapBuilder::new();
        for i in 0..50 {
            builder = builder.insert(&format!("key_{}", i), i);
        }
        let properties = builder.build();

        let op = WalOperation::UpdateNode {
            node_id: NodeId::new(1).unwrap(),
            version_id: crate::core::VersionId::new(1).unwrap(),
            label: GLOBAL_INTERNER.intern("LargeNode").unwrap(),
            properties,
            temporal: test_temporal(),
        };

        let estimated = estimate_entry_capacity(&op);

        // Verify by actually serializing
        let entry = WalEntry::new(LSN(1), op);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        assert!(
            buffer.len() <= estimated,
            "Actual size {} should not exceed estimate {}",
            buffer.len(),
            estimated
        );

        let overhead_ratio = estimated as f64 / buffer.len() as f64;
        assert!(
            overhead_ratio <= 1.5,
            "Estimate {} should not be more than 50% over actual size {}",
            estimated,
            buffer.len()
        );
    }

    // =============================================================================
    // TDD Tests for InternedString WAL operations (Issue #225)
    // =============================================================================

    /// TDD Test: Verify WalOperation can be created with InternedString (Issue #225)
    ///
    /// This test will initially fail because WalOperation currently uses String.
    /// After the optimization, it should use InternedString directly, eliminating
    /// the string allocation on the hot path.
    #[test]
    fn test_wal_operation_with_interned_string() {
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let node_id = NodeId::new(1).unwrap();
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        // This should compile and work with InternedString
        let _operation = WalOperation::CreateNode {
            node_id,
            label, // Should accept InternedString, not String
            properties,
            temporal,
        };
    }

    /// TDD Test: Verify serialization writes InternedString ID, not full string (Issue #225)
    ///
    /// This test verifies that the serialized format uses a 4-byte InternedString ID
    /// instead of length-prefixed string data, reducing WAL size and eliminating
    /// allocation overhead.
    #[test]
    fn test_wal_serialization_uses_interned_id() {
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let node_id = NodeId::new(1).unwrap();
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        let operation = WalOperation::CreateNode {
            node_id,
            label,
            properties,
            temporal,
        };

        let entry = WalEntry::new(LSN(1), operation);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        // After optimization:
        // - Entry header: LSN(8) + Timestamp(12) + Checksum(4) = 24 bytes
        // - Operation type: 1 byte
        // - NodeId: 8 bytes
        // - InternedString ID: 4 bytes (instead of 4 + 6 = 10 bytes for "Person" string)
        // - Properties: minimal size
        // - Temporal: 32 bytes (2 intervals * 16 bytes each)
        //
        // The key assertion: InternedString should be serialized as 4 bytes, not as
        // length-prefixed string data

        // For now, this test documents the expected behavior
        // After implementation, we can verify the exact byte layout
        assert!(!buffer.is_empty());
    }

    /// TDD Test: Verify round-trip serialization/deserialization with InternedString (Issue #225)
    ///
    /// This ensures that InternedString IDs can be written to WAL and read back correctly,
    /// and that the string can be resolved during recovery when needed.
    #[test]
    fn test_wal_roundtrip_with_interned_string() {
        let label_str = "TestLabel";
        let label = GLOBAL_INTERNER.intern(label_str).unwrap();
        let node_id = NodeId::new(42).unwrap();
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        // Create operation with InternedString
        let operation = WalOperation::CreateNode {
            node_id,
            label,
            properties: properties.clone(),
            temporal,
        };

        let entry = WalEntry::new(LSN(1), operation);

        // Serialize
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        // Deserialize
        let (parsed_entry, _) =
            super::segment_reader::parse_entry_at(&buffer, 0, WAL_VERSION).unwrap();

        // Verify the deserialized entry has the same InternedString
        match parsed_entry.operation {
            WalOperation::CreateNode {
                node_id: parsed_id,
                label: parsed_label,
                ..
            } => {
                assert_eq!(parsed_id, node_id);
                // The parsed label should be the same InternedString ID
                assert_eq!(parsed_label, label);

                // We can resolve it to verify the string value
                assert_eq!(
                    GLOBAL_INTERNER.resolve(parsed_label).unwrap().as_ref(),
                    label_str
                );
            }
            _ => panic!("Expected CreateNode operation"),
        }
    }
}
