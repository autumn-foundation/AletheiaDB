//! Tests for generic error types in transaction methods (Issue #826).
//!
//! Validates that read/write transaction methods support custom error types
//! while allowing the ? operator to work with AletheiaDB's internal errors.

use aletheiadb::api::transaction::{ReadOps, WriteOps};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

// Custom error type for repository layer
#[derive(Debug, PartialEq)]
enum RepositoryError {
    Database(String),
    NotFound,
    ValidationFailed(String),
}

impl From<aletheiadb::Error> for RepositoryError {
    fn from(e: aletheiadb::Error) -> Self {
        RepositoryError::Database(e.to_string())
    }
}

#[test]
fn test_read_with_custom_error_type() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Create a test node
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("Failed to create node");

    // Read with custom error type - should work with ? operator
    let result: Result<String, RepositoryError> = db.read(|tx| {
        let node = tx.get_node(node_id)?; // ? works with DB errors
        node.properties
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or(RepositoryError::NotFound) // Custom error
    });

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "Alice");
}

#[test]
fn test_read_with_custom_error_not_found() {
    let db = AletheiaDB::new().expect("Failed to create database");

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("age", 30).build(),
        )
        .expect("Failed to create node");

    // Read with custom error - returns custom error when property not found
    let result: Result<String, RepositoryError> = db.read(|tx| {
        let node = tx.get_node(node_id)?;
        node.properties
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or(RepositoryError::NotFound)
    });

    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), RepositoryError::NotFound);
}

#[test]
fn test_write_with_custom_error_type() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Write with custom error type - should work with ? operator
    let result: Result<String, RepositoryError> = db.write(|tx| {
        let node_id = tx.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert("age", 25)
                .build(),
        )?; // ? works with DB errors

        let node = tx.get_node(node_id)?;
        let age = node
            .properties
            .get("age")
            .and_then(|v| v.as_int())
            .ok_or(RepositoryError::NotFound)?;

        if age < 18 {
            return Err(RepositoryError::ValidationFailed(
                "Must be at least 18".to_string(),
            ));
        }

        Ok("Bob".to_string())
    });

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "Bob");
}

#[test]
fn test_write_with_validation_error() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Write with validation error
    let result: Result<String, RepositoryError> = db.write(|tx| {
        let node_id = tx.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Charlie")
                .insert("age", 15)
                .build(),
        )?;

        let node = tx.get_node(node_id)?;
        let age = node
            .properties
            .get("age")
            .and_then(|v| v.as_int())
            .ok_or(RepositoryError::NotFound)?;

        if age < 18 {
            return Err(RepositoryError::ValidationFailed(
                "Must be at least 18".to_string(),
            ));
        }

        Ok(node
            .properties
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    });

    assert!(result.is_err());
    match result.unwrap_err() {
        RepositoryError::ValidationFailed(msg) => {
            assert_eq!(msg, "Must be at least 18");
        }
        _ => panic!("Expected ValidationFailed error"),
    }
}

#[test]
fn test_write_with_timestamp_error() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Write with timestamp and custom error type
    let result: Result<(String, _), RepositoryError> = db.write_with_timestamp(|tx| {
        let node_id = tx.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Diana").build(),
        )?;

        let node = tx.get_node(node_id)?;
        let name = node
            .properties
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or(RepositoryError::NotFound)?;

        Ok(name)
    });

    assert!(result.is_ok());
    let (name, _timestamp) = result.unwrap();
    assert_eq!(name, "Diana");
}

#[test]
fn test_write_with_options_error() {
    use aletheiadb::storage::wal::{DurabilityMode, WriteOptions};

    let db = AletheiaDB::new().expect("Failed to create database");

    let mode = DurabilityMode::async_mode_validated(100).expect("Failed to create durability mode");
    let options = WriteOptions::new().with_durability(mode);

    // Write with options and custom error type
    let result: Result<usize, RepositoryError> = db.write_with_options(options, |tx| {
        for i in 0..10 {
            tx.create_node(
                "Item",
                PropertyMapBuilder::new().insert("id", i as i64).build(),
            )?; // ? works with DB errors
        }
        Ok(10)
    });

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 10);
}

#[test]
fn test_default_error_type_with_turbofish() {
    use aletheiadb::Error;

    let db = AletheiaDB::new().expect("Failed to create database");

    // Using turbofish syntax to specify error type explicitly
    let result = db.read::<_, _, Error>(|tx| {
        let count = tx.node_count();
        Ok(count)
    });

    assert!(result.is_ok());

    let result = db.write::<_, _, Error>(|tx| {
        let node_id = tx.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Eve").build(),
        )?;
        Ok(node_id)
    });

    assert!(result.is_ok());
}

#[test]
fn test_chained_operations_with_custom_errors() {
    let db = AletheiaDB::new().expect("Failed to create database");

    // Complex operation with multiple potential error points
    let result: Result<(String, i64), RepositoryError> = db.write(|tx| {
        // Create a person
        let person_id = tx.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Frank")
                .insert("age", 30)
                .build(),
        )?;

        // Create a company
        let company_id = tx.create_node(
            "Company",
            PropertyMapBuilder::new()
                .insert("name", "Acme Corp")
                .build(),
        )?;

        // Create employment edge
        tx.create_edge(
            person_id,
            company_id,
            "WORKS_AT",
            PropertyMapBuilder::new().insert("since", 2020).build(),
        )?;

        // Retrieve and validate
        let person = tx.get_node(person_id)?;
        let name = person
            .properties
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or(RepositoryError::NotFound)?;

        let age = person
            .properties
            .get("age")
            .and_then(|v| v.as_int())
            .ok_or(RepositoryError::NotFound)?;

        if age < 18 {
            return Err(RepositoryError::ValidationFailed(
                "Must be an adult".to_string(),
            ));
        }

        Ok((name.to_string(), age))
    });

    assert!(result.is_ok());
    let (name, age) = result.unwrap();
    assert_eq!(name, "Frank");
    assert_eq!(age, 30);
}
