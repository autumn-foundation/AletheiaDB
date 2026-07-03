#![cfg(test)]

//! Integration tests for `AletheiaDB::open(path)` (Issue #3227).
//!
//! These tests exercise the durable one-liner constructor: creating the
//! directory tree if absent, round-tripping data across a drop + reopen,
//! matching the canonical `with_unified_config(durable_config_for_data_dir)`
//! shape exactly, and surfacing I/O failures as `Result::Err` rather than
//! panicking.
//!
//! `tests/http_persistence.rs` (feature-gated behind `http-server`) asserts
//! similar durability/layout invariants through `ServerConfig`'s config
//! construction path; the tests here cover the same invariants through the
//! `AletheiaDB::open()` entry point directly, so this crate's durability
//! contract is verified independent of whether the HTTP server is compiled
//! in.
//!
//! None of these tests need to wait after dropping a database: `Drop` for
//! `AletheiaDB` synchronously joins the background persistence thread (which
//! performs a final flush as its last action) before returning, so state is
//! guaranteed durable the moment a `db` handle's scope ends.

use aletheiadb::config::durable_config_for_data_dir;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use tempfile::tempdir;

/// `open` on a nonexistent nested path creates the directory tree, and data
/// written through one handle is visible after dropping it and reopening the
/// same path in a fresh handle (round-trip durability, AC 1 & 2).
#[test]
fn open_write_drop_reopen_read_round_trip() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("nested").join("db");
    assert!(!data_dir.exists(), "precondition: path must not pre-exist");

    let node_id = {
        let db = AletheiaDB::open(&data_dir).unwrap();
        let node_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        let other_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
        db.create_edge(
            node_id,
            other_id,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2024).build(),
        )
        .unwrap();
        node_id
        // db dropped here, synchronously persisting final state.
    };

    let db2 = AletheiaDB::open(&data_dir).unwrap();
    let node = db2.get_node(node_id).unwrap();
    let name = node.properties.get("name").and_then(|v| v.as_str());
    assert_eq!(name, Some("Alice"));

    let outgoing = db2.get_outgoing_edges(node_id);
    assert_eq!(outgoing.len(), 1);
}

/// The directory layout created by `open` matches the canonical
/// `{path}/wal` and `{path}/indexes` shape (AC 3, no forked config).
#[test]
fn open_creates_canonical_layout() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("db");

    {
        let db = AletheiaDB::open(&data_dir).unwrap();
        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        // db dropped here, synchronously persisting final state.
    }

    assert!(data_dir.join("wal").exists(), "WAL dir should be created");
    assert!(
        data_dir.join("indexes").exists(),
        "index persistence dir should be created"
    );
}

/// `AletheiaDB::open(p)` is semantically equivalent to
/// `with_unified_config(durable_config_for_data_dir(p))`: a data dir created
/// through the explicit config path opens unchanged through `open` (AC 3 & 7).
#[test]
fn open_is_equivalent_to_durable_config() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("db");

    let node_id = {
        let config = durable_config_for_data_dir(data_dir.clone());
        let db = AletheiaDB::with_unified_config(config).unwrap();
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Carol").build(),
        )
        .unwrap()
        // db dropped here, synchronously persisting final state.
    };

    let db2 = AletheiaDB::open(&data_dir).unwrap();
    let node = db2.get_node(node_id).unwrap();
    let name = node.properties.get("name").and_then(|v| v.as_str());
    assert_eq!(name, Some("Carol"));
}

/// Opening at a path whose parent component is a regular file cannot
/// possibly succeed; `open` must surface this as `Err`, never panic
/// (AC 5). A direct call (rather than `catch_unwind`) is sufficient: if
/// `open` panicked, the test runner would fail this test with a stack trace
/// on its own.
#[test]
fn open_surfaces_io_error_not_panic() {
    let dir = tempdir().unwrap();
    let blocking_file = dir.path().join("not_a_directory");
    std::fs::write(&blocking_file, b"i am a file, not a dir").unwrap();

    let data_dir = blocking_file.join("db");
    let result = AletheiaDB::open(&data_dir);
    assert!(
        result.is_err(),
        "opening under a path whose parent is a regular file must return Err"
    );
}
