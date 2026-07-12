//! Serialization logic for WAL entries.
//!
//! This module implements a highly optimized, zero-allocation binary format
//! specifically designed for the Write-Ahead Log (WAL).
//!
//! # Why a Custom Format?
//!
//! AletheiaDB requires extreme write throughput (~100K+ writes/sec). General-purpose
//! serialization formats like JSON, Bincode, or Protobuf introduce unacceptable
//! overhead due to intermediate allocations, schema encoding, or lack of
//! precise capacity prediction.
//!
//! By using a custom, tightly packed byte-level format, we achieve:
//! 1. **Zero-Allocation Hot Paths**: By predicting exact sizes using `estimate_entry_capacity`,
//!    we pre-allocate a single buffer and stream bytes directly into it via `serialize_operation_into`.
//!    This prevents heap fragmentation under heavy write load.
//! 2. **Ring Buffer Compatibility**: The fixed-size headers and predictable payloads
//!    allow us to write directly into the memory-mapped `RingBuffer` used by the
//!    concurrent WAL system.
//! 3. **Built-in Integrity**: Every record is suffixed with a CRC32 checksum over its
//!    contents (including the `LSN` and `Timestamp`). This guarantees that partial
//!    writes or disk corruption are detected during recovery before they can corrupt
//!    the database state.
//!
//! # Binary Layout
//!
//! Every WAL entry has a fixed 24-byte overhead, followed by a variable-length payload:
//!
//! `[LSN: 8 bytes] [Timestamp: 12 bytes] [Checksum: 4 bytes] [Payload: ...]`
//!
//! The Payload always begins with a 1-byte Operation Tag (e.g., `OP_CREATE_NODE`),
//! followed by the specific fields for that operation.

#[cfg(test)]
use super::entry::WalEntry;
use super::entry::{LSN, WalOperation};
use crate::core::error::Result;
use crate::core::id::VersionId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::provenance::Provenance;
use crate::core::temporal::Timestamp;

// WAL operation type tags for the binary format.
// These must match the deserialization in segment_reader.rs.
pub(crate) const OP_CREATE_NODE: u8 = 1;
pub(crate) const OP_CREATE_EDGE: u8 = 2;
pub(crate) const OP_UPDATE_NODE: u8 = 3;
pub(crate) const OP_UPDATE_EDGE: u8 = 4;
pub(crate) const OP_CHECKPOINT: u8 = 5;
pub(crate) const OP_DELETE_NODE: u8 = 6;
pub(crate) const OP_DELETE_EDGE: u8 = 7;
pub(crate) const OP_DECLARE_UNIQUE_CONSTRAINT: u8 = 8;
pub(crate) const OP_DROP_UNIQUE_CONSTRAINT: u8 = 9;
pub(crate) const OP_RETRACT_NODE: u8 = 10;
pub(crate) const OP_RETRACT_EDGE: u8 = 11;

/// Sentinel u64 stored in a v9+ delete/retract payload when the operation
/// carries no tombstone `version_id` (Issue #3406). `u64::MAX` is always
/// `> MAX_VALID_ID` (`u64::MAX - 1000`), so it can never collide with a real
/// version id and round-trips cleanly back to `None` on parse. In practice the
/// live write path always logs `Some(id)`; the sentinel only exists to make the
/// `Option<VersionId>` serialization total.
pub(crate) const TOMBSTONE_VERSION_ID_ABSENT: u64 = u64::MAX;
/// Terminal transaction-commit marker (Issue #3413). Only appears in segments
/// at or above `WAL_VERSION_TX_FRAMING`.
pub(crate) const OP_COMMIT_TX: u8 = 12;
/// Leading transaction-begin marker (Issue #3413). Only appears in segments at
/// or above `WAL_VERSION_TX_FRAMING`.
pub(crate) const OP_BEGIN_TX: u8 = 13;

/// Serialize a label as length-prefixed UTF-8: `[len: u32-LE][utf8 bytes]`
/// (Issue #3506, WAL v13/v14).
///
/// Byte-identical to the property-key codec ([`crate::core::property::PropertyMap::serialize_into`]).
/// Persisting the string itself (rather than a raw interner id) makes the label
/// correct under ANY interner layout on replay: the reader re-interns it into
/// the process-global interner instead of trusting a persisted id whose meaning
/// is layout-relative. Returns [`StorageError::InconsistentState`] if the label
/// cannot be resolved from the interner (data corruption).
#[inline]
fn serialize_label_str(s: InternedString, buffer: &mut Vec<u8>) -> Result<()> {
    GLOBAL_INTERNER
        .resolve_with(s, |label_str| {
            let bytes = label_str.as_bytes();
            buffer.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buffer.extend_from_slice(bytes);
        })
        .ok_or_else(|| {
            crate::core::error::Error::Storage(
                crate::core::error::StorageError::InconsistentState {
                    reason: format!(
                        "Label {} not found in interner - data corruption detected",
                        s.as_u32()
                    ),
                },
            )
        })
}

/// Byte length of a label as serialized by [`serialize_label_str`]
/// (`4` length prefix + UTF-8 bytes). Falls back to `4` for an unresolvable id
/// (the write itself will error via [`serialize_label_str`]); this is only a
/// pre-allocation hint, so an underestimate merely costs a `Vec` realloc.
#[inline]
fn label_serialized_size(s: InternedString) -> usize {
    4 + GLOBAL_INTERNER
        .resolve_with(s, |label_str| label_str.len())
        .unwrap_or(0)
}

/// Serialize an optional tombstone/retraction `version_id` as a fixed 8-byte LE
/// u64 (Issue #3406). `None` is encoded as [`TOMBSTONE_VERSION_ID_ABSENT`], a
/// value that can never be a valid version id, so parsing round-trips it back
/// to `None` and drives the replay synthesis fallback.
#[inline]
fn serialize_tombstone_version_id(version_id: Option<VersionId>, buffer: &mut Vec<u8>) {
    let raw = version_id.map_or(TOMBSTONE_VERSION_ID_ABSENT, |v| v.as_u64());
    buffer.extend_from_slice(&raw.to_le_bytes());
}

/// Serialize an optional `&str`: `[1-byte presence][4-byte LE len][UTF-8 bytes]`.
#[inline]
fn serialize_opt_str(s: Option<&str>, buffer: &mut Vec<u8>) {
    match s {
        None => buffer.push(0),
        Some(s) => {
            buffer.push(1);
            let bytes = s.as_bytes();
            buffer.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buffer.extend_from_slice(bytes);
        }
    }
}

/// Serialize an optional `f64`: `[1-byte presence][8-byte LE value]`.
#[inline]
fn serialize_opt_f64(v: Option<f64>, buffer: &mut Vec<u8>) {
    match v {
        None => buffer.push(0),
        Some(v) => {
            buffer.push(1);
            buffer.extend_from_slice(&v.to_le_bytes());
        }
    }
}

/// Byte size of an optional `&str` as serialized by [`serialize_opt_str`].
#[inline]
fn opt_str_size(s: Option<&str>) -> usize {
    1 + s.map(|s| 4 + s.len()).unwrap_or(0)
}

/// Byte size of an optional `f64` as serialized by [`serialize_opt_f64`].
#[inline]
fn opt_f64_size(v: Option<f64>) -> usize {
    1 + if v.is_some() { 8 } else { 0 }
}

/// Serialize an optional [`Provenance`] bundle:
/// `[1-byte presence][source][confidence][note][correlation_id][principal]`.
///
/// The five fields are only present when the outer presence byte is nonzero;
/// each is itself independently optional (see [`serialize_opt_str`] /
/// [`serialize_opt_f64`]). Written unconditionally by writers on this format
/// version -- readers on older WAL versions never look for these bytes at
/// all (see `WAL_VERSION_PROVENANCE` / `WAL_VERSION_PROVENANCE_PRINCIPAL`
/// in `segment_reader.rs`; `principal` was added by Issue #3350).
#[inline]
fn serialize_provenance_into(provenance: Option<&Provenance>, buffer: &mut Vec<u8>) {
    match provenance {
        None => buffer.push(0),
        Some(p) => {
            buffer.push(1);
            serialize_opt_str(p.source(), buffer);
            serialize_opt_f64(p.confidence(), buffer);
            serialize_opt_str(p.note(), buffer);
            serialize_opt_str(p.correlation_id(), buffer);
            serialize_opt_str(p.principal(), buffer);
        }
    }
}

/// Byte size of an optional [`Provenance`] bundle as serialized by
/// [`serialize_provenance_into`].
#[inline]
fn provenance_serialized_size(provenance: Option<&Provenance>) -> usize {
    match provenance {
        None => 1,
        Some(p) => {
            1 + opt_str_size(p.source())
                + opt_f64_size(p.confidence())
                + opt_str_size(p.note())
                + opt_str_size(p.correlation_id())
                + opt_str_size(p.principal())
        }
    }
}

/// Estimate the required buffer capacity for serializing a WAL entry.
///
/// This function provides an upper-bound estimate of the buffer size needed
/// to serialize a WAL entry, allowing pre-allocation to avoid reallocations
/// during the hot-path serialization process.
///
/// # Why Estimate Instead of Exact Size?
///
/// Property maps contain nested, variable-length elements (Strings, Arrays, Vectors).
/// Calculating the *exact* byte-for-byte size requires iterating through the entire
/// property map. Instead of doing a full pass before serialization, we use a
/// conservative upper-bound estimate. Slightly over-allocating a `Vec` is vastly
/// cheaper (CPU cycles) than traversing the properties twice or hitting a memory
/// reallocation mid-serialization.
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
/// - `CreateNode`: 1 (op type) + 8 (node_id) + 4 (label ID) +
///   properties size + 12 (Timestamp)
/// - `CreateEdge`: 1 (op type) + 8 (edge_id) + 8 (source) + 8 (target) +
///   4 (label ID) + properties size + 12 (Timestamp)
/// - `UpdateNode`: 1 (op type) + 8 (node_id) + 8 (version_id) + 4 (label ID) +
///   properties size + 12 (Timestamp)
/// - `UpdateEdge`: 1 (op type) + 8 (edge_id) + 8 (version_id) + 4 (label ID) +
///   properties size + 12 (Timestamp)
/// - `DeleteNode`: 1 (op type) + 8 (node_id) + 12 (Timestamp) = 21 bytes
/// - `DeleteEdge`: 1 (op type) + 8 (edge_id) + 12 (Timestamp) = 21 bytes
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
///
/// # Examples
///
/// ```rust
/// use aletheiadb::storage::wal::entry::{WalOperation, LSN};
/// use aletheiadb::core::id::NodeId;
/// use aletheiadb::core::temporal::Timestamp;
///
/// // A lightweight operation without properties
/// // (Using timestamp from wallclock because internal constructor is private)
/// let delete_op = WalOperation::DeleteNode {
///     node_id: NodeId::try_from(1).unwrap(),
///     valid_from: Timestamp::from(12345),
///     version_id: None, // no logged tombstone id (synthesized on replay)
///     provenance: None, // no acting-principal attribution (#3427)
/// };
///
/// // Fixed overhead (24 bytes) + DeleteNode payload (21 bytes)
/// // Note: This function is pub(crate) so we can't directly call it in doctests
/// // let capacity = estimate_entry_capacity(&delete_op);
/// // assert_eq!(capacity, 45);
/// ```
pub(crate) fn estimate_entry_capacity(operation: &WalOperation) -> usize {
    // Fixed overhead: LSN (8) + Timestamp (12) + Checksum (4)
    const FIXED_OVERHEAD: usize = 24;
    // Timestamp (HybridTimestamp) is always 12 bytes (wallclock + logical)
    const TIMESTAMP_SIZE: usize = 12;

    let variable_size = match operation {
        WalOperation::CreateNode {
            label,
            properties,
            provenance,
            ..
        } => {
            // op type (1) + node_id (8) + label ([len:4][utf8], #3506) + properties + valid_from (12) + provenance
            let base = 1 + 8 + label_serialized_size(*label) + TIMESTAMP_SIZE;
            base + properties.serialized_size() + provenance_serialized_size(provenance.as_ref())
        }
        WalOperation::CreateEdge {
            label,
            properties,
            provenance,
            ..
        } => {
            // op type (1) + edge_id (8) + source (8) + target (8) + label ([len:4][utf8], #3506) + properties + valid_from (12) + provenance
            let base = 1 + 8 + 8 + 8 + label_serialized_size(*label) + TIMESTAMP_SIZE;
            base + properties.serialized_size() + provenance_serialized_size(provenance.as_ref())
        }
        WalOperation::UpdateNode {
            label,
            properties,
            provenance,
            ..
        } => {
            // op type (1) + node_id (8) + version_id (8) + label ([len:4][utf8], #3506) + properties + valid_from (12) + provenance
            let base = 1 + 8 + 8 + label_serialized_size(*label) + TIMESTAMP_SIZE;
            base + properties.serialized_size() + provenance_serialized_size(provenance.as_ref())
        }
        WalOperation::UpdateEdge {
            label,
            properties,
            provenance,
            ..
        } => {
            // op type (1) + edge_id (8) + version_id (8) + label ([len:4][utf8], #3506) + properties + valid_from (12) + provenance
            let base = 1 + 8 + 8 + label_serialized_size(*label) + TIMESTAMP_SIZE;
            base + properties.serialized_size() + provenance_serialized_size(provenance.as_ref())
        }
        WalOperation::DeleteNode { provenance, .. } => {
            // op type (1) + node_id (8) + valid_from (12) + version_id (8, #3406)
            // + provenance blob (#3427)
            1 + 8 + TIMESTAMP_SIZE + 8 + provenance_serialized_size(provenance.as_ref())
        }
        WalOperation::DeleteEdge { provenance, .. } => {
            // op type (1) + edge_id (8) + valid_from (12) + version_id (8, #3406)
            // + provenance blob (#3427)
            1 + 8 + TIMESTAMP_SIZE + 8 + provenance_serialized_size(provenance.as_ref())
        }
        WalOperation::RetractNode { provenance, .. } => {
            // op type (1) + node_id (8) + valid_to (12) + version_id (8, #3406)
            // + provenance blob (#3427)
            1 + 8 + TIMESTAMP_SIZE + 8 + provenance_serialized_size(provenance.as_ref())
        }
        WalOperation::RetractEdge { provenance, .. } => {
            // op type (1) + edge_id (8) + valid_to (12) + version_id (8, #3406)
            // + provenance blob (#3427)
            1 + 8 + TIMESTAMP_SIZE + 8 + provenance_serialized_size(provenance.as_ref())
        }
        WalOperation::Checkpoint { .. } => {
            // op type (1) + lsn (8) + timestamp (12)
            1 + 8 + 12
        }
        WalOperation::DeclareUniqueConstraint { label, property }
        | WalOperation::DropUniqueConstraint { label, property } => {
            // op type (1) + label ([len:4][utf8]) + property ([len:4][utf8], #3506)
            1 + label_serialized_size(*label) + label_serialized_size(*property)
        }
        WalOperation::BeginTx { .. } => {
            // op type (1) + tx_id (8)
            1 + 8
        }
        WalOperation::CommitTx { .. } => {
            // op type (1) + tx_id (8) + entry_count (4) + commit_timestamp (12)
            1 + 8 + 4 + TIMESTAMP_SIZE
        }
    };

    FIXED_OVERHEAD + variable_size
}

/// Serialize a WAL entry components into the provided buffer
///
/// This allows serialization without creating a `WalEntry` wrapper. By providing
/// a mutable buffer, callers can reuse memory across multiple serializations,
/// preventing the allocator from becoming a bottleneck during heavy write bursts.
///
/// # Memory Management
///
/// This function **appends** to the provided buffer. It is the caller's
/// responsibility to clear the buffer (`buffer.clear()`) before reuse to maintain
/// its capacity without retaining stale data.
///
/// # Format Structure
///
/// 1. `LSN` (8 bytes, little-endian)
/// 2. `Timestamp` (12 bytes, Phase 2 HybridTimestamp)
/// 3. `Checksum` Placeholder (4 bytes of zeroes)
/// 4. Operation Payload (`OP_*` tag + specific fields)
/// 5. (After payload is written, a CRC32 checksum is calculated and written into the placeholder).
///
/// # Examples
///
/// ```rust
/// use aletheiadb::storage::wal::entry::{WalOperation, LSN};
/// use aletheiadb::core::id::NodeId;
/// use aletheiadb::core::temporal::Timestamp;
///
/// let op = WalOperation::DeleteNode {
///     node_id: NodeId::try_from(42).unwrap(),
///     valid_from: Timestamp::from(100),
///     version_id: None, // no logged tombstone id (synthesized on replay)
///     provenance: None, // no acting-principal attribution (#3427)
/// };
///
/// // 1. Pre-allocate the buffer using our estimate
/// // let capacity = estimate_entry_capacity(&op);
/// // let mut buffer = Vec::with_capacity(capacity);
///
/// // 2. Serialize
/// // serialize_operation_into(
/// //     LSN(1000),
/// //     Timestamp::from(12345),
/// //     &op,
/// //     &mut buffer
/// // ).unwrap();
///
/// // The buffer now holds the exact binary representation
/// // assert_eq!(buffer.len(), 45); // Fixed length for DeleteNode
/// ```
pub(crate) fn serialize_operation_into(
    lsn: LSN,
    timestamp: Timestamp,
    operation: &WalOperation,
    buffer: &mut Vec<u8>,
) -> Result<()> {
    // Write LSN (8 bytes)
    buffer.extend_from_slice(&lsn.0.to_le_bytes());

    // Write timestamp (12 bytes: Phase 2 HybridTimestamp)
    timestamp.serialize_into(buffer);

    // Reserve space for checksum (4 bytes) - will fill in later
    let checksum_offset = buffer.len();
    buffer.extend_from_slice(&[0u8; 4]);

    // Write operation type and data with full serialization
    match operation {
        WalOperation::CreateNode {
            node_id,
            label,
            properties,
            valid_from,
            provenance,
        } => {
            buffer.push(OP_CREATE_NODE);
            buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
            serialize_label_str(*label, buffer)?;
            properties.serialize_into(buffer)?;
            valid_from.serialize_into(buffer);
            serialize_provenance_into(provenance.as_ref(), buffer);
        }
        WalOperation::CreateEdge {
            edge_id,
            source,
            target,
            label,
            properties,
            valid_from,
            provenance,
        } => {
            buffer.push(OP_CREATE_EDGE);
            buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
            buffer.extend_from_slice(&source.as_u64().to_le_bytes());
            buffer.extend_from_slice(&target.as_u64().to_le_bytes());
            serialize_label_str(*label, buffer)?;
            properties.serialize_into(buffer)?;
            valid_from.serialize_into(buffer);
            serialize_provenance_into(provenance.as_ref(), buffer);
        }
        WalOperation::UpdateNode {
            node_id,
            version_id,
            label,
            properties,
            valid_from,
            provenance,
        } => {
            buffer.push(OP_UPDATE_NODE);
            buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
            buffer.extend_from_slice(&version_id.as_u64().to_le_bytes());
            serialize_label_str(*label, buffer)?;
            properties.serialize_into(buffer)?;
            valid_from.serialize_into(buffer);
            serialize_provenance_into(provenance.as_ref(), buffer);
        }
        WalOperation::UpdateEdge {
            edge_id,
            version_id,
            label,
            properties,
            valid_from,
            provenance,
        } => {
            buffer.push(OP_UPDATE_EDGE);
            buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
            buffer.extend_from_slice(&version_id.as_u64().to_le_bytes());
            serialize_label_str(*label, buffer)?;
            properties.serialize_into(buffer)?;
            valid_from.serialize_into(buffer);
            serialize_provenance_into(provenance.as_ref(), buffer);
        }
        WalOperation::DeleteNode {
            node_id,
            valid_from,
            version_id,
            provenance,
        } => {
            buffer.push(OP_DELETE_NODE);
            buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
            valid_from.serialize_into(buffer);
            // Issue #3406: log the tombstone version id (v9+ format). New
            // segments always use the highest version, so this field is always
            // present on disk; the sentinel handles a `None` (never produced by
            // the live path) so the encoding is total.
            serialize_tombstone_version_id(*version_id, buffer);
            // Issue #3427: append the acting-principal provenance blob (v11+),
            // byte-identical to the create/update encoding.
            serialize_provenance_into(provenance.as_ref(), buffer);
        }
        WalOperation::DeleteEdge {
            edge_id,
            valid_from,
            version_id,
            provenance,
        } => {
            buffer.push(OP_DELETE_EDGE);
            buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
            valid_from.serialize_into(buffer);
            serialize_tombstone_version_id(*version_id, buffer);
            // Issue #3427: append the acting-principal provenance blob (v11+).
            serialize_provenance_into(provenance.as_ref(), buffer);
        }
        WalOperation::RetractNode {
            node_id,
            valid_to,
            version_id,
            provenance,
        } => {
            buffer.push(OP_RETRACT_NODE);
            buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
            valid_to.serialize_into(buffer);
            serialize_tombstone_version_id(*version_id, buffer);
            // Issue #3427: append the acting-principal provenance blob (v11+).
            serialize_provenance_into(provenance.as_ref(), buffer);
        }
        WalOperation::RetractEdge {
            edge_id,
            valid_to,
            version_id,
            provenance,
        } => {
            buffer.push(OP_RETRACT_EDGE);
            buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
            valid_to.serialize_into(buffer);
            serialize_tombstone_version_id(*version_id, buffer);
            // Issue #3427: append the acting-principal provenance blob (v11+).
            serialize_provenance_into(provenance.as_ref(), buffer);
        }
        WalOperation::Checkpoint { lsn, timestamp } => {
            buffer.push(OP_CHECKPOINT);
            buffer.extend_from_slice(&lsn.0.to_le_bytes());
            // Phase 2: Use HybridTimestamp serialization
            timestamp.serialize_into(buffer);
        }
        WalOperation::DeclareUniqueConstraint { label, property } => {
            buffer.push(OP_DECLARE_UNIQUE_CONSTRAINT);
            serialize_label_str(*label, buffer)?;
            serialize_label_str(*property, buffer)?;
        }
        WalOperation::DropUniqueConstraint { label, property } => {
            buffer.push(OP_DROP_UNIQUE_CONSTRAINT);
            serialize_label_str(*label, buffer)?;
            serialize_label_str(*property, buffer)?;
        }
        WalOperation::BeginTx { tx_id } => {
            buffer.push(OP_BEGIN_TX);
            buffer.extend_from_slice(&tx_id.to_le_bytes());
        }
        WalOperation::CommitTx {
            tx_id,
            entry_count,
            commit_timestamp,
        } => {
            buffer.push(OP_COMMIT_TX);
            buffer.extend_from_slice(&tx_id.to_le_bytes());
            buffer.extend_from_slice(&entry_count.to_le_bytes());
            commit_timestamp.serialize_into(buffer);
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

/// Serialize a WAL entry with CRC32 checksum into the provided buffer
///
/// This function reuses the provided buffer to avoid per-entry allocation.
/// The caller should clear the buffer before calling this function to maintain its capacity.
#[cfg(test)]
pub(crate) fn serialize_entry_into(entry: &WalEntry, buffer: &mut Vec<u8>) -> Result<()> {
    serialize_operation_into(entry.lsn, entry.timestamp, &entry.operation, buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::EdgeId;
    use crate::core::NodeId;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::Timestamp;
    use crate::storage::wal::entry::LSN; // Imported only for tests

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
        // DeleteNode: op type (1) + node_id (8) + valid_from (12) + version_id (8)
        // + absent-provenance presence byte (1, #3427) = 30 bytes
        // Fixed overhead: 24 bytes
        // Total: 54 bytes
        let op = WalOperation::DeleteNode {
            node_id: NodeId::new(1).unwrap(),
            valid_from: test_timestamp(),
            version_id: Some(VersionId::new(7).unwrap()),
            provenance: None,
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(estimated, 54, "DeleteNode should be exactly 54 bytes");

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
    fn test_estimate_capacity_retract_node() {
        // RetractNode: op type (1) + node_id (8) + valid_to (12) + version_id (8)
        // + absent-provenance presence byte (1, #3427) = 30 bytes
        // Fixed overhead: 24 bytes => 54 total (same shape as DeleteNode).
        let op = WalOperation::RetractNode {
            node_id: NodeId::new(1).unwrap(),
            valid_to: test_timestamp(),
            version_id: Some(VersionId::new(7).unwrap()),
            provenance: None,
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(estimated, 54, "RetractNode should be exactly 54 bytes");

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
    fn test_estimate_capacity_retract_edge() {
        // RetractEdge: op type (1) + edge_id (8) + valid_to (12) + version_id (8)
        // + absent-provenance presence byte (1, #3427) = 30 bytes
        // Fixed overhead: 24 bytes => 54 total (same shape as DeleteEdge).
        let op = WalOperation::RetractEdge {
            edge_id: EdgeId::new(1).unwrap(),
            valid_to: test_timestamp(),
            version_id: Some(VersionId::new(7).unwrap()),
            provenance: None,
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(estimated, 54, "RetractEdge should be exactly 54 bytes");

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
        // DeleteEdge: op type (1) + edge_id (8) + valid_from (12) + version_id (8)
        // + absent-provenance presence byte (1, #3427) = 30 bytes
        // Fixed overhead: 24 bytes
        // Total: 54 bytes
        let op = WalOperation::DeleteEdge {
            edge_id: EdgeId::new(1).unwrap(),
            valid_from: test_timestamp(),
            version_id: Some(VersionId::new(7).unwrap()),
            provenance: None,
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(estimated, 54, "DeleteEdge should be exactly 54 bytes");

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

    /// Issue #3427 (R5): `estimate_entry_capacity` for a destructive op carrying
    /// a LARGE provenance bundle (long principal/source/note) must be an upper
    /// bound on the actual serialized length, so the pre-allocated buffer never
    /// reallocs mid-serialization.
    #[test]
    fn test_estimate_capacity_delete_node_with_large_provenance() {
        let big = "x".repeat(4096);
        let provenance = Provenance::builder()
            .source(big.clone())
            .note(big.clone())
            .correlation_id(big.clone())
            .principal(big)
            .confidence(0.5)
            .build()
            .unwrap();
        let op = WalOperation::DeleteNode {
            node_id: NodeId::new(1).unwrap(),
            valid_from: test_timestamp(),
            version_id: Some(VersionId::new(7).unwrap()),
            provenance: Some(provenance),
        };

        let estimated = estimate_entry_capacity(&op);
        let entry = WalEntry::new(LSN(1), op);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();
        assert!(
            estimated >= buffer.len(),
            "estimate {} must be >= actual serialized length {} (no under-alloc)",
            estimated,
            buffer.len()
        );
    }

    #[test]
    fn test_estimate_capacity_create_node_empty_properties() {
        // CreateNode with empty properties, no provenance:
        // Fixed: 24 bytes (LSN + Timestamp + Checksum)
        // op type (1) + node_id (8) + label ([len:4]["test":4] = 8, #3506)
        // + properties (4 for empty count) + valid_from (12)
        // + provenance presence byte (1, absent)
        // = 24 + 1 + 8 + 8 + 4 + 12 + 1 = 58 bytes
        let op = WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: GLOBAL_INTERNER.intern("test").unwrap(),
            properties: PropertyMapBuilder::new().build(),
            valid_from: test_timestamp(),
            provenance: None,
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(
            estimated, 58,
            "CreateNode with empty properties (no provenance) should be 58 bytes \
             (label is now length-prefixed UTF-8, #3506)"
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
            valid_from: test_timestamp(),
            provenance: None,
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
            valid_from: test_timestamp(),
            provenance: None,
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
            valid_from: test_timestamp(),
            provenance: None,
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
            valid_from: test_timestamp(),
            provenance: None,
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

    // ===================================================================
    // Issue #3506 — labels serialized as length-prefixed UTF-8 (write side)
    // ===================================================================

    /// The serialized CreateNode payload must contain the label's UTF-8 bytes
    /// (`[len:u32-LE]["Person"]`), not a raw 4-byte interner id. This is the
    /// observable write-side change: the label string is now self-describing
    /// and survives any interner layout.
    #[test]
    fn create_node_serializes_label_as_utf8_string() {
        let op = WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: GLOBAL_INTERNER.intern("Person3506Node").unwrap(),
            properties: PropertyMapBuilder::new().build(),
            valid_from: test_timestamp(),
            provenance: None,
        };
        let mut buffer = Vec::new();
        serialize_operation_into(LSN(1), test_timestamp(), &op, &mut buffer).unwrap();

        let needle = b"Person3506Node";
        let contains = buffer.windows(needle.len()).any(|w| w == needle);
        assert!(
            contains,
            "serialized CreateNode must embed the UTF-8 label string, not a raw id"
        );

        // The length prefix (LE u32) for the label must precede the bytes.
        let label_bytes = needle;
        let len_prefix = (label_bytes.len() as u32).to_le_bytes();
        let mut framed = Vec::new();
        framed.extend_from_slice(&len_prefix);
        framed.extend_from_slice(label_bytes);
        assert!(
            buffer.windows(framed.len()).any(|w| w == framed.as_slice()),
            "label must be length-prefixed UTF-8 [len][bytes]"
        );
    }

    /// The two unique-constraint ops carry TWO label strings (label + property);
    /// both must be serialized as UTF-8.
    #[test]
    fn constraint_ops_serialize_both_labels_as_utf8() {
        for op in [
            WalOperation::DeclareUniqueConstraint {
                label: GLOBAL_INTERNER.intern("Cust3506").unwrap(),
                property: GLOBAL_INTERNER.intern("email3506").unwrap(),
            },
            WalOperation::DropUniqueConstraint {
                label: GLOBAL_INTERNER.intern("Cust3506").unwrap(),
                property: GLOBAL_INTERNER.intern("email3506").unwrap(),
            },
        ] {
            let mut buffer = Vec::new();
            serialize_operation_into(LSN(1), test_timestamp(), &op, &mut buffer).unwrap();
            for needle in [b"Cust3506".as_slice(), b"email3506".as_slice()] {
                assert!(
                    buffer.windows(needle.len()).any(|w| w == needle),
                    "constraint op must embed UTF-8 label/property strings"
                );
            }
        }
    }

    /// `estimate_entry_capacity` must account for the variable UTF-8 length of a
    /// long / multi-byte label — the pre-allocation must be >= the actual
    /// serialized size so the hot path never under-allocates (T6).
    #[test]
    fn estimate_capacity_covers_long_multibyte_label() {
        // A multi-byte (non-ASCII) label longer than the old fixed 4-byte slot.
        let long_label = "Ключ-Étiquette-🏷️-VeryLongLabelName-3506";
        let ops = [
            WalOperation::CreateNode {
                node_id: NodeId::new(1).unwrap(),
                label: GLOBAL_INTERNER.intern(long_label).unwrap(),
                properties: PropertyMapBuilder::new().build(),
                valid_from: test_timestamp(),
                provenance: None,
            },
            WalOperation::CreateEdge {
                edge_id: EdgeId::new(1).unwrap(),
                source: NodeId::new(1).unwrap(),
                target: NodeId::new(2).unwrap(),
                label: GLOBAL_INTERNER.intern(long_label).unwrap(),
                properties: PropertyMapBuilder::new().build(),
                valid_from: test_timestamp(),
                provenance: None,
            },
            WalOperation::UpdateNode {
                node_id: NodeId::new(1).unwrap(),
                version_id: VersionId::new(1).unwrap(),
                label: GLOBAL_INTERNER.intern(long_label).unwrap(),
                properties: PropertyMapBuilder::new().build(),
                valid_from: test_timestamp(),
                provenance: None,
            },
            WalOperation::UpdateEdge {
                edge_id: EdgeId::new(1).unwrap(),
                version_id: VersionId::new(1).unwrap(),
                label: GLOBAL_INTERNER.intern(long_label).unwrap(),
                properties: PropertyMapBuilder::new().build(),
                valid_from: test_timestamp(),
                provenance: None,
            },
            WalOperation::DeclareUniqueConstraint {
                label: GLOBAL_INTERNER.intern(long_label).unwrap(),
                property: GLOBAL_INTERNER.intern(long_label).unwrap(),
            },
        ];
        for op in ops {
            let estimated = estimate_entry_capacity(&op);
            let mut buffer = Vec::new();
            serialize_operation_into(LSN(1), test_timestamp(), &op, &mut buffer).unwrap();
            // estimate_entry_capacity includes the 24-byte fixed overhead
            // (LSN+ts+checksum) that serialize_operation_into also writes.
            assert!(
                estimated >= buffer.len(),
                "estimate {estimated} must cover actual serialized size {} for {op:?}",
                buffer.len()
            );
        }
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::id::{EdgeId, NodeId};
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
    use proptest::prelude::*;

    // Helper to generate InternedString
    fn arb_interned_string() -> impl Strategy<Value = InternedString> {
        "[a-zA-Z0-9_]{1,10}".prop_map(|s| GLOBAL_INTERNER.intern(&s).unwrap())
    }

    // Helper to generate PropertyValue
    fn arb_property_value() -> impl Strategy<Value = PropertyValue> {
        prop_oneof![
            Just(PropertyValue::Null),
            any::<bool>().prop_map(PropertyValue::Bool),
            any::<i64>().prop_map(PropertyValue::Int),
            any::<f64>().prop_map(PropertyValue::Float),
            "[a-zA-Z0-9]{0,20}".prop_map(|s| PropertyValue::string(&s)),
        ]
    }

    // Helper to generate PropertyMap
    fn arb_property_map() -> impl Strategy<Value = PropertyMap> {
        prop::collection::vec(
            (
                "[a-z]{1,10}",        // Key
                arb_property_value(), // Value
            ),
            0..10, // Size
        )
        .prop_map(|entries| {
            let mut builder = PropertyMapBuilder::new();
            for (k, v) in entries {
                builder = builder.insert(&k, v);
            }
            builder.build()
        })
    }

    // Helper to generate Timestamp
    fn arb_timestamp() -> impl Strategy<Value = Timestamp> {
        any::<i64>().prop_map(|t| HybridTimestamp::new_unchecked(t, 0))
    }

    // Helper to generate an OPTIONAL tombstone/retraction version id (Issue
    // #3406). Generates both `Some(id)` and `None` so the `u64::MAX` absent
    // sentinel is fuzzed alongside real ids. The `Some` range stays well below
    // both the sentinel and `MAX_VALID_ID`, so a fuzzed id is never mistaken for
    // the sentinel and always survives `VersionId::new` validation on parse.
    fn arb_opt_version_id() -> impl Strategy<Value = Option<VersionId>> {
        prop_oneof![
            Just(None),
            (1u64..1_000_000u64).prop_map(|id| Some(VersionId::new_unchecked(id))),
        ]
    }

    // Helper to generate WalOperation
    fn arb_wal_operation() -> impl Strategy<Value = WalOperation> {
        prop_oneof![
            // CreateNode
            (
                (1u64..u64::MAX).prop_map(|id| NodeId::new(id).unwrap()),
                arb_interned_string(),
                arb_property_map(),
                arb_timestamp()
            )
                .prop_map(|(node_id, label, properties, valid_from)| {
                    WalOperation::CreateNode {
                        node_id,
                        label,
                        properties,
                        valid_from,
                        provenance: None,
                    }
                }),
            // DeleteNode (Issue #3406: fuzz BOTH a logged tombstone id AND the
            // `None`/sentinel case, so the u64::MAX absent encoding is exercised
            // through the estimate/serialize path too).
            (
                (1u64..u64::MAX).prop_map(|id| NodeId::new(id).unwrap()),
                arb_timestamp(),
                arb_opt_version_id(),
            )
                .prop_map(|(node_id, valid_from, version_id)| {
                    WalOperation::DeleteNode {
                        node_id,
                        valid_from,
                        version_id,
                        provenance: None,
                    }
                }),
            // Checkpoint
            (any::<u64>().prop_map(LSN), arb_timestamp())
                .prop_map(|(lsn, timestamp)| { WalOperation::Checkpoint { lsn, timestamp } }),
            // DeclareUniqueConstraint
            (arb_interned_string(), arb_interned_string()).prop_map(|(label, property)| {
                WalOperation::DeclareUniqueConstraint { label, property }
            }),
            // DropUniqueConstraint
            (arb_interned_string(), arb_interned_string()).prop_map(|(label, property)| {
                WalOperation::DropUniqueConstraint { label, property }
            })
        ]
    }

    proptest! {
        #[test]
        fn test_estimate_capacity_is_upper_bound(
            op in arb_wal_operation(),
            lsn_val in any::<u64>()
        ) {
            let lsn = LSN(lsn_val);
            let timestamp = HybridTimestamp::new_unchecked(1000, 0); // Dummy timestamp for entry

            // Calculate estimate
            let estimated = estimate_entry_capacity(&op);

            // Perform actual serialization
            let mut buffer = Vec::new();
            serialize_operation_into(lsn, timestamp, &op, &mut buffer).unwrap();
            let actual = buffer.len();

            // Verify estimate >= actual
            // Note: Our estimate might be slightly larger due to conservative sizing, but never smaller
            prop_assert!(estimated >= actual, "Estimate {} < Actual {}", estimated, actual);

            // Verify estimate isn't wildly inaccurate (e.g. > 2x actual + constant overhead)
            // Small payloads might have high constant overhead relative to size, so be lenient
            if actual > 100 {
                prop_assert!(estimated <= actual * 2, "Estimate {} > 2x Actual {}", estimated, actual);
            }
        }

        /// Issue #3406: every delete/retract op's optional tombstone
        /// `version_id` — INCLUDING the `None` case (encoded as the u64::MAX
        /// sentinel) — round-trips serialize -> parse byte-for-byte. Fuzzing
        /// `None` alongside `Some(id)` ensures the sentinel is decoded back to
        /// `None` (not a spurious `Some(u64::MAX)`), and that the parser
        /// consumes exactly the bytes serialized.
        #[test]
        fn delete_retract_version_id_round_trips(
            id in 1u64..1_000_000u64,
            ts in 1i64..1_000_000_000_000i64,
            version_id in arb_opt_version_id(),
        ) {
            use crate::storage::wal::segment_reader::{parse_entry_at, WAL_VERSION_DESTRUCTIVE_PROVENANCE};
            let ts = HybridTimestamp::new_unchecked(ts, 0);
            let ops = [
                WalOperation::DeleteNode {
                    node_id: NodeId::new(id).unwrap(),
                    valid_from: ts,
                    version_id,
                    provenance: None,
                },
                WalOperation::DeleteEdge {
                    edge_id: EdgeId::new(id).unwrap(),
                    valid_from: ts,
                    version_id,
                    provenance: None,
                },
                WalOperation::RetractNode {
                    node_id: NodeId::new(id).unwrap(),
                    valid_to: ts,
                    version_id,
                    provenance: None,
                },
                WalOperation::RetractEdge {
                    edge_id: EdgeId::new(id).unwrap(),
                    valid_to: ts,
                    version_id,
                    provenance: None,
                },
            ];
            for op in ops {
                let entry = WalEntry::new(LSN(1), op.clone());
                let mut buffer = Vec::new();
                serialize_entry_into(&entry, &mut buffer).unwrap();
                let (parsed, consumed) =
                    parse_entry_at(&buffer, 0, WAL_VERSION_DESTRUCTIVE_PROVENANCE).unwrap();
                prop_assert_eq!(consumed, buffer.len());
                prop_assert_eq!(parsed.operation, op);
            }
        }
    }
}
