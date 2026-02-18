//! Serialization logic for WAL entries.

#[cfg(test)]
use super::entry::WalEntry;
use super::entry::{LSN, WalOperation};
use crate::core::error::Result;
use crate::core::interning::InternedString;
use crate::core::temporal::Timestamp;

/// Helper to serialize an InternedString into the buffer (4-byte ID)
#[inline(always)]
fn serialize_interned_string(s: InternedString, buffer: &mut Vec<u8>) {
    buffer.extend_from_slice(&s.as_u32().to_le_bytes());
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
pub(crate) fn estimate_entry_capacity(operation: &WalOperation) -> usize {
    // Fixed overhead: LSN (8) + Timestamp (12) + Checksum (4)
    const FIXED_OVERHEAD: usize = 24;
    // Timestamp (HybridTimestamp) is always 12 bytes (wallclock + logical)
    const TIMESTAMP_SIZE: usize = 12;

    let variable_size = match operation {
        WalOperation::CreateNode { properties, .. } => {
            // op type (1) + node_id (8) + label (4-byte InternedString ID) + properties + valid_from (12)
            let base = 1 + 8 + 4 + TIMESTAMP_SIZE;
            base + properties.serialized_size()
        }
        WalOperation::CreateEdge { properties, .. } => {
            // op type (1) + edge_id (8) + source (8) + target (8) + label (4-byte InternedString ID) + properties + valid_from (12)
            let base = 1 + 8 + 8 + 8 + 4 + TIMESTAMP_SIZE;
            base + properties.serialized_size()
        }
        WalOperation::UpdateNode { properties, .. } => {
            // op type (1) + node_id (8) + version_id (8) + label (4-byte InternedString ID) + properties + valid_from (12)
            let base = 1 + 8 + 8 + 4 + TIMESTAMP_SIZE;
            base + properties.serialized_size()
        }
        WalOperation::UpdateEdge { properties, .. } => {
            // op type (1) + edge_id (8) + version_id (8) + label (4-byte InternedString ID) + properties + valid_from (12)
            let base = 1 + 8 + 8 + 4 + TIMESTAMP_SIZE;
            base + properties.serialized_size()
        }
        WalOperation::DeleteNode { .. } => {
            // op type (1) + node_id (8) + valid_from (12)
            1 + 8 + TIMESTAMP_SIZE
        }
        WalOperation::DeleteEdge { .. } => {
            // op type (1) + edge_id (8) + valid_from (12)
            1 + 8 + TIMESTAMP_SIZE
        }
        WalOperation::Checkpoint { .. } => {
            // op type (1) + lsn (8) + timestamp (12)
            1 + 8 + 12
        }
    };

    FIXED_OVERHEAD + variable_size
}

/// Serialize a WAL entry components into the provided buffer
///
/// This allows serialization without creating a WalEntry wrapper.
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
        } => {
            buffer.push(1); // operation type
            buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
            serialize_interned_string(*label, buffer);
            properties.serialize_into(buffer)?;
            valid_from.serialize_into(buffer);
        }
        WalOperation::CreateEdge {
            edge_id,
            source,
            target,
            label,
            properties,
            valid_from,
        } => {
            buffer.push(2); // operation type
            buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
            buffer.extend_from_slice(&source.as_u64().to_le_bytes());
            buffer.extend_from_slice(&target.as_u64().to_le_bytes());
            serialize_interned_string(*label, buffer);
            properties.serialize_into(buffer)?;
            valid_from.serialize_into(buffer);
        }
        WalOperation::UpdateNode {
            node_id,
            version_id,
            label,
            properties,
            valid_from,
        } => {
            buffer.push(3); // operation type
            buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
            buffer.extend_from_slice(&version_id.as_u64().to_le_bytes());
            serialize_interned_string(*label, buffer);
            properties.serialize_into(buffer)?;
            valid_from.serialize_into(buffer);
        }
        WalOperation::UpdateEdge {
            edge_id,
            version_id,
            label,
            properties,
            valid_from,
        } => {
            buffer.push(4); // operation type
            buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
            buffer.extend_from_slice(&version_id.as_u64().to_le_bytes());
            serialize_interned_string(*label, buffer);
            properties.serialize_into(buffer)?;
            valid_from.serialize_into(buffer);
        }
        WalOperation::DeleteNode {
            node_id,
            valid_from,
        } => {
            buffer.push(6); // operation type
            buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
            valid_from.serialize_into(buffer);
        }
        WalOperation::DeleteEdge {
            edge_id,
            valid_from,
        } => {
            buffer.push(7); // operation type
            buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
            valid_from.serialize_into(buffer);
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
        // DeleteNode: op type (1) + node_id (8) + valid_from (12) = 21 bytes
        // Fixed overhead: 24 bytes
        // Total: 45 bytes
        let op = WalOperation::DeleteNode {
            node_id: NodeId::new(1).unwrap(),
            valid_from: test_timestamp(),
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(estimated, 45, "DeleteNode should be exactly 45 bytes");

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
        // DeleteEdge: op type (1) + edge_id (8) + valid_from (12) = 21 bytes
        // Fixed overhead: 24 bytes
        // Total: 45 bytes
        let op = WalOperation::DeleteEdge {
            edge_id: EdgeId::new(1).unwrap(),
            valid_from: test_timestamp(),
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(estimated, 45, "DeleteEdge should be exactly 45 bytes");

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
        // CreateNode with empty properties:
        // Fixed: 24 bytes (LSN + Timestamp + Checksum)
        // op type (1) + node_id (8) + label (4-byte InternedString) + properties (4 for empty count) + valid_from (12)
        // = 24 + 1 + 8 + 4 + 4 + 12 = 53 bytes
        let op = WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: GLOBAL_INTERNER.intern("test").unwrap(),
            properties: PropertyMapBuilder::new().build(),
            valid_from: test_timestamp(),
        };

        let estimated = estimate_entry_capacity(&op);
        assert_eq!(
            estimated, 53,
            "CreateNode with empty properties should be 53 bytes"
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
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::id::NodeId;
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
                    }
                }),
            // DeleteNode
            (
                (1u64..u64::MAX).prop_map(|id| NodeId::new(id).unwrap()),
                arb_timestamp()
            )
                .prop_map(|(node_id, valid_from)| {
                    WalOperation::DeleteNode {
                        node_id,
                        valid_from,
                    }
                }),
            // Checkpoint
            (any::<u64>().prop_map(LSN), arb_timestamp())
                .prop_map(|(lsn, timestamp)| { WalOperation::Checkpoint { lsn, timestamp } })
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
    }
}
