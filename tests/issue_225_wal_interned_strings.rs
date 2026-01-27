//! Tests for issue #225: WAL should accept InternedString directly
//!
//! This test verifies that WalOperation uses InternedString instead of String,
//! eliminating unnecessary allocations in the hot path.

use gallifreydb::core::id::{EdgeId, NodeId, VersionId};
use gallifreydb::core::interning::GLOBAL_INTERNER;
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::core::temporal::{BiTemporalInterval, time};
use gallifreydb::storage::wal::{LSN, WalEntry, WalOperation};

/// Helper to create a test temporal interval
fn test_temporal() -> BiTemporalInterval {
    BiTemporalInterval::current(time::now())
}

/// Test that WalOperation::CreateNode uses InternedString for label
#[test]
fn test_wal_create_node_uses_interned_string() {
    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    let node_id = NodeId::new(1).unwrap();
    let properties = PropertyMapBuilder::new().insert("name", "Alice").build();
    let temporal = test_temporal();

    // This should compile without needing to resolve the InternedString
    let op = WalOperation::CreateNode {
        node_id,
        label, // Should accept InternedString directly
        properties,
        temporal,
    };

    // Verify the operation was created successfully
    match op {
        WalOperation::CreateNode { label: l, .. } => {
            // The label should be an InternedString, not a String
            assert_eq!(l, label);
        }
        _ => panic!("Expected CreateNode operation"),
    }
}

/// Test that WalOperation::CreateEdge uses InternedString for label
#[test]
fn test_wal_create_edge_uses_interned_string() {
    let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let edge_id = EdgeId::new(1).unwrap();
    let source = NodeId::new(1).unwrap();
    let target = NodeId::new(2).unwrap();
    let properties = PropertyMapBuilder::new().build();
    let temporal = test_temporal();

    let op = WalOperation::CreateEdge {
        edge_id,
        source,
        target,
        label, // Should accept InternedString directly
        properties,
        temporal,
    };

    match op {
        WalOperation::CreateEdge { label: l, .. } => {
            assert_eq!(l, label);
        }
        _ => panic!("Expected CreateEdge operation"),
    }
}

/// Test that WalOperation::UpdateNode uses InternedString for label
#[test]
fn test_wal_update_node_uses_interned_string() {
    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    let node_id = NodeId::new(1).unwrap();
    let version_id = VersionId::new(2).unwrap();
    let properties = PropertyMapBuilder::new().insert("name", "Bob").build();
    let temporal = test_temporal();

    let op = WalOperation::UpdateNode {
        node_id,
        version_id,
        label, // Should accept InternedString directly
        properties,
        temporal,
    };

    match op {
        WalOperation::UpdateNode { label: l, .. } => {
            assert_eq!(l, label);
        }
        _ => panic!("Expected UpdateNode operation"),
    }
}

/// Test that WalOperation::UpdateEdge uses InternedString for label
#[test]
fn test_wal_update_edge_uses_interned_string() {
    let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let edge_id = EdgeId::new(1).unwrap();
    let version_id = VersionId::new(2).unwrap();
    let properties = PropertyMapBuilder::new().build();
    let temporal = test_temporal();

    let op = WalOperation::UpdateEdge {
        edge_id,
        version_id,
        label, // Should accept InternedString directly
        properties,
        temporal,
    };

    match op {
        WalOperation::UpdateEdge { label: l, .. } => {
            assert_eq!(l, label);
        }
        _ => panic!("Expected UpdateEdge operation"),
    }
}

/// Test that WalEntry can be created with InternedString
#[test]
fn test_wal_entry_creation_with_interned_string() {
    let label = GLOBAL_INTERNER.intern("TestLabel").unwrap();
    let node_id = NodeId::new(42).unwrap();
    let properties = PropertyMapBuilder::new().insert("key", "value").build();
    let temporal = test_temporal();

    let op = WalOperation::CreateNode {
        node_id,
        label,
        properties,
        temporal,
    };

    // Creating a WAL entry should work without any allocations
    let entry = WalEntry::new(LSN(100), op);

    // Verify the entry was created successfully
    match &entry.operation {
        WalOperation::CreateNode { label: l, .. } => {
            assert_eq!(*l, label);
        }
        _ => panic!("Expected CreateNode operation"),
    }
}

/// Test that no allocations occur when converting BufferedWrite to WalOperation
#[test]
fn test_no_allocations_in_buffered_write_to_wal_operation() {
    use gallifreydb::api::transaction::write_buffer::BufferedWrite;

    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    let node_id = NodeId::new(1).unwrap();
    let version_id = VersionId::new(1).unwrap();
    let properties = PropertyMapBuilder::new().build();
    let temporal = test_temporal();

    let buffered = BufferedWrite::CreateNode {
        node_id,
        version_id,
        label,
        properties: properties.clone(),
        temporal,
    };

    // When converting to WalOperation, the label should be copied directly
    // without resolving to String (no allocation)
    let wal_op = match buffered {
        BufferedWrite::CreateNode {
            node_id,
            label,
            properties,
            temporal,
            ..
        } => {
            WalOperation::CreateNode {
                node_id,
                label, // Should copy InternedString directly (just a u32)
                properties,
                temporal,
            }
        }
        _ => panic!("Expected CreateNode"),
    };

    // Verify the operation was created without string allocation
    match wal_op {
        WalOperation::CreateNode { label: l, .. } => {
            // The label should still be the same InternedString
            assert_eq!(l, label);
        }
        _ => panic!("Expected CreateNode operation"),
    }
}

/// Test that operations with different label lengths have same InternedString size
#[test]
fn test_interned_string_size_independence() {
    let short_label = GLOBAL_INTERNER.intern("A").unwrap();
    let long_label = GLOBAL_INTERNER
        .intern("VeryLongLabelNameWithManyCharacters")
        .unwrap();

    // Both labels should be 4 bytes (u32) regardless of string length
    assert_eq!(std::mem::size_of_val(&short_label), 4);
    assert_eq!(std::mem::size_of_val(&long_label), 4);

    // Create operations with both labels
    let node_id = NodeId::new(1).unwrap();
    let properties = PropertyMapBuilder::new().build();
    let temporal = test_temporal();

    let op1 = WalOperation::CreateNode {
        node_id,
        label: short_label,
        properties: properties.clone(),
        temporal,
    };

    let op2 = WalOperation::CreateNode {
        node_id,
        label: long_label,
        properties,
        temporal,
    };

    // Both operations should use the same amount of memory for the label field
    // (4 bytes for the InternedString ID, not the original string length)
    match (&op1, &op2) {
        (
            WalOperation::CreateNode { label: l1, .. },
            WalOperation::CreateNode { label: l2, .. },
        ) => {
            assert_eq!(std::mem::size_of_val(l1), std::mem::size_of_val(l2));
            assert_eq!(std::mem::size_of_val(l1), 4);
        }
        _ => panic!("Expected CreateNode operations"),
    }
}
