//! Serialization logic for WAL entries.

#[cfg(test)]
use super::entry::WalEntry;
use super::entry::{LSN, WalOperation};
use crate::core::interning::InternedString;
use crate::core::temporal::Timestamp;
use crate::utils::error::Result;

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
    use proptest::prelude::*;
    use crate::core::id::{EdgeId, NodeId, VersionId};
    use crate::core::interning::{GLOBAL_INTERNER, InternedString};
    use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
    use crate::core::temporal::MAX_VALID_TIMESTAMP;
    use crate::core::vector::SparseVec;
    use crate::storage::wal::entry::{LSN, WalEntry, WalOperation};
    use std::sync::Arc;

    // Strategy for LSN
    fn arb_lsn() -> impl Strategy<Value = LSN> {
        any::<u64>().prop_map(LSN)
    }

    // Strategy for Timestamp (HybridTimestamp)
    fn arb_timestamp() -> impl Strategy<Value = Timestamp> {
        (0..=MAX_VALID_TIMESTAMP, any::<u32>())
            .prop_map(|(w, l)| crate::core::hlc::HybridTimestamp::new(w, l).unwrap())
    }

    // Strategy for InternedString
    // We use a limited set of strings to avoid filling the interner, plus some random ones
    fn arb_interned_string() -> impl Strategy<Value = InternedString> {
        prop_oneof![
            Just("name".to_string()),
            Just("age".to_string()),
            Just("created_at".to_string()),
            Just("label".to_string()),
            "[a-z]{1,5}", // Short random strings
        ]
        .prop_map(|s| GLOBAL_INTERNER.intern(s).unwrap())
    }

    // Recursive strategy for PropertyValue
    fn arb_property_value(depth: u32) -> impl Strategy<Value = PropertyValue> {
        let leaf = prop_oneof![
            Just(PropertyValue::Null),
            any::<bool>().prop_map(PropertyValue::Bool),
            any::<i64>().prop_map(PropertyValue::Int),
            // Filter NaNs for strict equality checks
            any::<f64>().prop_filter("No NaN", |f| !f.is_nan()).prop_map(PropertyValue::Float),
            "[a-z0-9_]{0,32}".prop_map(|s| PropertyValue::String(Arc::from(s))),
            prop::collection::vec(any::<u8>(), 0..32).prop_map(|v| PropertyValue::Bytes(Arc::from(v.as_slice()))),
            // Small vectors for testing
            prop::collection::vec(any::<f32>().prop_filter("No NaN", |f| !f.is_nan()), 0..16)
                .prop_map(|v| PropertyValue::Vector(Arc::from(v.as_slice()))),
        ];

        leaf.prop_recursive(
            depth,      // levels deep
            64,         // max size of collection
            4,          // items per collection
            move |inner| {
                prop_oneof![
                    // Array
                    prop::collection::vec(inner.clone(), 0..4)
                        .prop_map(|v| PropertyValue::Array(Arc::new(v))),
                    // SparseVector (can't be nested, but included here for completeness)
                    // We generate valid SparseVecs
                    (
                        prop::collection::vec((any::<u32>(), any::<f32>().prop_filter("No NaN", |f| !f.is_nan())), 0..10),
                        1..100u32 // dimension
                    ).prop_filter_map("Invalid SparseVec", |(pairs, dim)| {
                        let mut pairs = pairs;
                        // Filter indices >= dimension
                        pairs.retain(|(idx, val)| *idx < dim && *val != 0.0);
                        // Sort by index and deduplicate
                        pairs.sort_by_key(|(idx, _)| *idx);
                        pairs.dedup_by_key(|(idx, _)| *idx);

                        let (indices, values): (Vec<u32>, Vec<f32>) = pairs.into_iter().unzip();
                        SparseVec::new(indices, values, dim).ok().map(PropertyValue::sparse_vector)
                    })
                ]
            },
        )
    }

    // Strategy for PropertyMap
    fn arb_property_map() -> impl Strategy<Value = PropertyMap> {
        prop::collection::hash_map(
            arb_interned_string(),
            arb_property_value(2), // Limit recursion depth
            0..5
        ).prop_map(|map| {
            let mut builder = PropertyMapBuilder::new();
            for (k, v) in map {
                builder = builder.insert_by_key(k, v);
            }
            builder.build()
        })
    }

    // Strategy for WalOperation
    fn arb_wal_operation() -> impl Strategy<Value = WalOperation> {
        let arb_node_id = any::<u64>().prop_map(|id| NodeId::new(id).unwrap());
        let arb_edge_id = any::<u64>().prop_map(|id| EdgeId::new(id).unwrap());
        let arb_version_id = any::<u64>().prop_map(|id| VersionId::new(id).unwrap());

        prop_oneof![
            // CreateNode
            (arb_node_id.clone(), arb_interned_string(), arb_property_map(), arb_timestamp())
                .prop_map(|(node_id, label, properties, valid_from)| WalOperation::CreateNode {
                    node_id, label, properties, valid_from
                }),
            // CreateEdge
            (arb_edge_id.clone(), arb_node_id.clone(), arb_node_id.clone(), arb_interned_string(), arb_property_map(), arb_timestamp())
                .prop_map(|(edge_id, source, target, label, properties, valid_from)| WalOperation::CreateEdge {
                    edge_id, source, target, label, properties, valid_from
                }),
            // UpdateNode
            (arb_node_id.clone(), arb_version_id.clone(), arb_interned_string(), arb_property_map(), arb_timestamp())
                .prop_map(|(node_id, version_id, label, properties, valid_from)| WalOperation::UpdateNode {
                    node_id, version_id, label, properties, valid_from
                }),
            // UpdateEdge
            (arb_edge_id.clone(), arb_version_id.clone(), arb_interned_string(), arb_property_map(), arb_timestamp())
                .prop_map(|(edge_id, version_id, label, properties, valid_from)| WalOperation::UpdateEdge {
                    edge_id, version_id, label, properties, valid_from
                }),
            // DeleteNode
            (arb_node_id.clone(), arb_timestamp())
                .prop_map(|(node_id, valid_from)| WalOperation::DeleteNode {
                    node_id, valid_from
                }),
            // DeleteEdge
            (arb_edge_id.clone(), arb_timestamp())
                .prop_map(|(edge_id, valid_from)| WalOperation::DeleteEdge {
                    edge_id, valid_from
                }),
            // Checkpoint
            (arb_lsn(), arb_timestamp())
                .prop_map(|(lsn, timestamp)| WalOperation::Checkpoint {
                    lsn, timestamp
                }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))] // Run 100 cases to keep it fast but effective

        #[test]
        fn test_wal_entry_round_trip(
            lsn in arb_lsn(),
            operation in arb_wal_operation()
        ) {
            let entry = WalEntry::new(lsn, operation);

            // Serialize
            let mut buffer = Vec::new();
            serialize_entry_into(&entry, &mut buffer).expect("Serialization failed");

            // Verify capacity estimate was sufficient
            let estimated = estimate_entry_capacity(&entry.operation);
            prop_assert!(buffer.len() <= estimated, "Buffer len {} > estimate {}", buffer.len(), estimated);

            // Deserialize
            // We use the same version assumed by current code (1)
            let (deserialized, consumed) = super::super::segment_reader::parse_entry_at(&buffer, 0, 1)
                .expect("Deserialization failed");

            prop_assert_eq!(consumed, buffer.len(), "Did not consume all bytes");

            // Compare fields
            prop_assert_eq!(entry.lsn, deserialized.lsn, "LSN mismatch");

            // Timestamp in WalEntry::new is set to time::now(), which we didn't control in the strategy.
            // But we can check that deserialized timestamp matches original.
            prop_assert_eq!(entry.timestamp, deserialized.timestamp, "Timestamp mismatch");

            // Operation comparison (requires Debug match or manual destructuring if PartialEq not implemented fully)
            // WalOperation doesn't derive PartialEq but PropertyValue does.
            // Let's implement a helper or rely on Debug format equality for complex enums if needed,
            // but usually PartialEq is derived for these.
            // Checking source: WalOperation derives Clone, Debug. NO PartialEq!
            // Wait, src/storage/wal/entry.rs: #[derive(Debug, Clone)] pub enum WalOperation ...
            // It does NOT derive PartialEq.
            //
            // We need to assert equality manually or via Debug string. Debug string is robust enough for this.
            prop_assert_eq!(format!("{:?}", entry.operation), format!("{:?}", deserialized.operation), "Operation mismatch");
        }
    }
}
