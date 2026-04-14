//! Tests for the node scanning API (Issue #825).
//!
//! Validates the `scan_nodes_by_label()` method for iterating over nodes by label.

use aletheiadb::api::transaction::WriteOps;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

#[test]
fn test_scan_nodes_by_label_empty() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Scanning for nodes with a label that doesn't exist should return empty
    let person_ids: Vec<_> = db.scan_nodes_by_label("Person").collect();
    assert_eq!(person_ids.len(), 0);
}

#[test]
fn test_scan_nodes_by_label_single() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Create a single Person node
    let alice_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30)
                .build(),
        )
        .expect("Failed to create node");

    // Scan should find exactly one Person
    let person_ids: Vec<_> = db.scan_nodes_by_label("Person").collect();
    assert_eq!(person_ids.len(), 1);
    assert_eq!(person_ids[0], alice_id);
}

#[test]
fn test_scan_nodes_by_label_multiple() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Create multiple Person nodes
    let alice_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("Failed to create node");

    let bob_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .expect("Failed to create node");

    let carol_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Carol").build(),
        )
        .expect("Failed to create node");

    // Scan should find all three Person nodes
    let mut person_ids: Vec<_> = db.scan_nodes_by_label("Person").collect();
    person_ids.sort(); // DashMap iteration order is not guaranteed
    assert_eq!(person_ids.len(), 3);

    let mut expected = vec![alice_id, bob_id, carol_id];
    expected.sort();
    assert_eq!(person_ids, expected);
}

#[test]
fn test_scan_nodes_by_label_multiple_labels() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Create nodes with different labels
    let alice_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("Failed to create node");

    let rust_book_id = db
        .create_node(
            "Product",
            PropertyMapBuilder::new()
                .insert("title", "The Rust Programming Language")
                .insert("price", 39.99)
                .build(),
        )
        .expect("Failed to create node");

    let bob_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .expect("Failed to create node");

    let laptop_id = db
        .create_node(
            "Product",
            PropertyMapBuilder::new()
                .insert("title", "ThinkPad X1")
                .insert("price", 1299.99)
                .build(),
        )
        .expect("Failed to create node");

    // Scan for Person nodes
    let mut person_ids: Vec<_> = db.scan_nodes_by_label("Person").collect();
    person_ids.sort();
    assert_eq!(person_ids.len(), 2);

    let mut expected_persons = vec![alice_id, bob_id];
    expected_persons.sort();
    assert_eq!(person_ids, expected_persons);

    // Scan for Product nodes
    let mut product_ids: Vec<_> = db.scan_nodes_by_label("Product").collect();
    product_ids.sort();
    assert_eq!(product_ids.len(), 2);

    let mut expected_products = vec![rust_book_id, laptop_id];
    expected_products.sort();
    assert_eq!(product_ids, expected_products);

    // Scan for non-existent label
    let event_ids: Vec<_> = db.scan_nodes_by_label("Event").collect();
    assert_eq!(event_ids.len(), 0);
}

#[test]
fn test_scan_nodes_by_label_iterator_ops() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Create several Person nodes
    for i in 0..10 {
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("id", i as i64).build(),
        )
        .expect("Failed to create node");
    }

    // Test iterator operations

    // Count
    let count = db.scan_nodes_by_label("Person").count();
    assert_eq!(count, 10);

    // Take
    let first_five: Vec<_> = db.scan_nodes_by_label("Person").take(5).collect();
    assert_eq!(first_five.len(), 5);

    // Filter
    let filtered: Vec<_> = db
        .scan_nodes_by_label("Person")
        .filter(|node_id| {
            let node = db.get_node(*node_id).unwrap();
            let id_value = node.properties.get("id").unwrap();
            if let Some(id) = id_value.as_int() {
                id % 2 == 0
            } else {
                false
            }
        })
        .collect();
    assert_eq!(filtered.len(), 5); // 0, 2, 4, 6, 8
}

#[test]
fn test_scan_nodes_by_label_with_updates() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Create initial Person nodes
    let alice_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("Failed to create node");

    let bob_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .expect("Failed to create node");

    // Initial scan
    let person_ids: Vec<_> = db.scan_nodes_by_label("Person").collect();
    assert_eq!(person_ids.len(), 2);

    // Update a node's properties (label stays the same)
    db.write(|tx| {
        tx.update_node(
            alice_id,
            PropertyMapBuilder::new()
                .insert("name", "Alice Updated")
                .insert("age", 31)
                .build(),
        )
    })
    .expect("Failed to update node");

    // Scan should still find both nodes
    let person_ids: Vec<_> = db.scan_nodes_by_label("Person").collect();
    assert_eq!(person_ids.len(), 2);

    // Delete a node
    db.write(|tx| tx.delete_node(bob_id))
        .expect("Failed to delete node");

    // Scan should now find only Alice
    let person_ids: Vec<_> = db.scan_nodes_by_label("Person").collect();
    assert_eq!(person_ids.len(), 1);
    assert_eq!(person_ids[0], alice_id);
}

#[test]
fn test_scan_nodes_by_label_large_dataset() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Create a larger dataset with multiple labels
    const PERSON_COUNT: usize = 1000;
    const PRODUCT_COUNT: usize = 500;

    for i in 0..PERSON_COUNT {
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("id", i as i64).build(),
        )
        .expect("Failed to create node");
    }

    for i in 0..PRODUCT_COUNT {
        db.create_node(
            "Product",
            PropertyMapBuilder::new().insert("id", i as i64).build(),
        )
        .expect("Failed to create node");
    }

    // Verify counts
    let person_count = db.scan_nodes_by_label("Person").count();
    assert_eq!(person_count, PERSON_COUNT);

    let product_count = db.scan_nodes_by_label("Product").count();
    assert_eq!(product_count, PRODUCT_COUNT);

    // Total node count should match
    assert_eq!(db.node_count(), PERSON_COUNT + PRODUCT_COUNT);
}
