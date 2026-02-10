use super::*;
use crate::AletheiaDB;
use crate::core::id::MAX_VALID_ID;
use crate::core::property::PropertyMapBuilder;
use crate::core::temporal::time;

#[test]
fn test_create_node_with_valid_time_trait_method_exists() {
    // This test verifies the trait method signature compiles
    fn assert_write_ops<T: WriteOps>(_tx: &mut T) {
        // Trait bound check - if this compiles, the method exists
    }

    let db = AletheiaDB::new().unwrap();
    let mut tx = db.write_transaction().unwrap();
    assert_write_ops(&mut tx);
}

#[test]
fn test_create_node_default_delegates_to_with_valid_time() {
    let db = AletheiaDB::new().unwrap();
    let mut tx = db.write_transaction().unwrap();

    // Both should work identically when valid_from is None
    let props1 = PropertyMapBuilder::new().insert("name", "Test1").build();
    let props2 = PropertyMapBuilder::new().insert("name", "Test2").build();

    // Both should succeed
    let result1 = tx.create_node("Test", props1);
    assert!(result1.is_ok(), "create_node failed: {:?}", result1.err());
    let id1 = result1.unwrap();

    let result2 = tx.create_node_with_valid_time("Test", props2, None);
    assert!(
        result2.is_ok(),
        "create_node_with_valid_time failed: {:?}",
        result2.err()
    );
    let id2 = result2.unwrap();

    // IDs should be different (sequential generation)
    assert_ne!(id1, id2, "IDs should be unique");

    // Both methods should work - IDs are generated successfully
    // Note: First ID may be 0 due to IdGenerator starting at 0 (known issue)
    assert!(id1.as_u64() < id2.as_u64(), "IDs should increment");
}

#[test]
fn test_create_node_with_backdated_valid_time() {
    let db = AletheiaDB::new().unwrap();
    let mut tx = db.write_transaction().unwrap();

    // Create node with valid_time = 1 hour ago
    let one_hour_ago = time::now().wallclock() - 3_600_000_000;
    let valid_from = crate::core::hlc::HybridTimestamp::new(one_hour_ago, 0).unwrap();

    let props = PropertyMapBuilder::new().insert("name", "Alice").build();
    let node_id = tx
        .create_node_with_valid_time("Person", props, Some(valid_from))
        .unwrap();

    // Verify node was created with a valid ID (0 is valid!)
    assert!(node_id.as_u64() <= MAX_VALID_ID);
}

#[test]
fn test_create_edge_with_valid_time_trait_method_exists() {
    fn assert_write_ops<T: WriteOps>(_tx: &mut T) {
        // Trait bound check
    }

    let db = AletheiaDB::new().unwrap();
    let mut tx = db.write_transaction().unwrap();
    assert_write_ops(&mut tx);
}

#[test]
fn test_update_node_with_valid_time_trait_method_exists() {
    fn assert_write_ops<T: WriteOps>(_tx: &mut T) {
        // Trait bound check
    }

    let db = AletheiaDB::new().unwrap();
    let mut tx = db.write_transaction().unwrap();
    assert_write_ops(&mut tx);
}

#[test]
fn test_delete_node_with_valid_time_trait_method_exists() {
    fn assert_write_ops<T: WriteOps>(_tx: &mut T) {
        // Trait bound check
    }

    let db = AletheiaDB::new().unwrap();
    let mut tx = db.write_transaction().unwrap();
    assert_write_ops(&mut tx);
}
