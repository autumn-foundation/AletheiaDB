//! Validation tests for benchmark assumptions
//!
//! These tests verify that the benchmarks in benches/performance_targets.rs
//! are testing what they claim to test, specifically around anchor creation
//! and delta reconstruction.
//!
//! **IMPORTANT DISCOVERY**: GallifreyDB uses wallclock microsecond timestamps
//! for tx_time and valid_time, NOT sequential transaction numbers. Querying
//! at `tx_time=10` looks for data from 10 microseconds after Unix epoch (1970),
//! not the 10th transaction. This calls into question whether the benchmarks
//! are actually testing time-travel correctly, as they query with small integer
//! values like 5, 9, and 10.
//!
//! Current status: Tests are marked as #[ignore] pending investigation of
//! proper temporal query semantics.

use gallifreydb::api::transaction::WriteOps;
use gallifreydb::{GallifreyDB, PropertyMapBuilder};

/// Test that anchors are created at expected positions with default anchor_interval=10
///
/// This validates the assumptions made in the time-travel benchmarks:
/// - create_node creates tx_time=0 (anchor)
/// - Updates create tx_time 1, 2, 3, ... (deltas until next anchor)
/// - Anchor created at tx_time=10
/// - tx_time 5 and 9 are deltas, not anchors
#[test]
#[ignore] // TODO: Fix timestamp semantics - tx_time is wallclock microseconds, not sequential
fn test_anchor_creation_matches_benchmark_assumptions() {
    let db = GallifreyDB::new();

    // Create initial node
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create_node should succeed");

    // Create 10 updates (matching the benchmark exactly)
    for i in 1..=10 {
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("version", i)
                    .build(),
            )?;
            Ok(())
        })
        .expect("update_node should succeed");
    }

    // First, verify we can get the current state
    println!("Node ID: {:?}", node_id);
    let current_node = db
        .get_node(node_id)
        .expect("Should be able to get current node");
    println!(
        "Current node version property: {:?}",
        current_node.properties.get("version")
    );

    // Check historical stats
    let stats = db.historical_stats().expect("Should get stats");
    println!(
        "Historical stats - node versions: {}, anchors: {}, deltas: {}",
        stats.total_node_versions, stats.node_anchor_count, stats.node_delta_count
    );

    // Maybe the issue is that tx_time and valid_time need to be actual timestamps, not sequential numbers?
    // Let me try querying with a very large timestamp (like "now")
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;

    println!("Trying to query with timestamp 'now': {}", now);
    let at_now = db.get_node_at_time(node_id, now, now);
    println!(
        "Query at 'now' result: {:?}",
        at_now.as_ref().map(|n| n.properties.get("version"))
    );

    // Also try with timestamp 10
    println!("Trying to query with timestamp 10");
    let at_10 = db.get_node_at_time(node_id, 10, 10);
    println!("Query at 10 result: {:?}", at_10.as_ref().err());

    assert!(
        at_now.is_ok() || at_10.is_ok(),
        "At least one query should succeed"
    );

    // Verify the version property is correct
    let node_at_10 = at_10.unwrap();
    assert_eq!(
        node_at_10.properties.get("version"),
        Some(&10i64.into()),
        "Node at tx_time=10 should have version=10"
    );
}

/// Test that delta reconstruction actually happens for non-anchor queries
///
/// This test verifies that querying at tx_time 5 (delta) returns different
/// state than querying at tx_time 10 (anchor), proving that delta
/// reconstruction is working correctly.
#[test]
#[ignore] // TODO: Fix timestamp semantics - tx_time is wallclock microseconds, not sequential
fn test_delta_reconstruction_produces_correct_state() {
    let db = GallifreyDB::new();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create_node should succeed");

    // Create 15 versions with incrementing values (matching benchmark pattern)
    for i in 1..=15 {
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("value", i)
                    .build(),
            )?;
            Ok(())
        })
        .expect("update_node should succeed");
    }

    // Query at different time points (same as benchmarks)
    let node_at_5 = db
        .get_node_at_time(node_id, 5, 5)
        .expect("Query at 5 should succeed");
    let node_at_9 = db
        .get_node_at_time(node_id, 9, 9)
        .expect("Query at 9 should succeed");
    let node_at_10 = db
        .get_node_at_time(node_id, 10, 10)
        .expect("Query at 10 should succeed");
    let node_at_15 = db
        .get_node_at_time(node_id, 15, 15)
        .expect("Query at 15 should succeed");

    // Verify each query returns the correct state for that time
    assert_eq!(node_at_5.properties.get("value"), Some(&5i64.into()));
    assert_eq!(node_at_9.properties.get("value"), Some(&9i64.into()));
    assert_eq!(node_at_10.properties.get("value"), Some(&10i64.into()));
    assert_eq!(node_at_15.properties.get("value"), Some(&15i64.into()));
}

/// Test performance targets benchmark runtime
///
/// This is a smoke test to ensure the benchmark suite completes in reasonable time.
/// The benchmarks claim to run in <30 seconds for CI, so this test verifies
/// that assumption by running a minimal version.
#[test]
#[ignore] // Run with `cargo test --ignored` to check benchmark runtime
fn test_performance_targets_benchmark_runtime() {
    use std::time::Instant;

    let start = Instant::now();

    // Simulate the benchmark setup (without criterion overhead)
    let db = GallifreyDB::new();
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create_node should succeed");

    // Create 19 versions (worst case benchmark)
    for i in 1..=19 {
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("version", i)
                    .build(),
            )
        })
        .expect("update_node should succeed");
    }

    // Perform queries (simulate benchmark iterations)
    for _ in 0..100 {
        let _ = db.get_node_at_time(node_id, 10, 10); // at_anchor
        let _ = db.get_node_at_time(node_id, 5, 5); // with_5_deltas
        let _ = db.get_node_at_time(node_id, 9, 9); // worst_case
    }

    let elapsed = start.elapsed();
    println!("Benchmark simulation took: {:?}", elapsed);

    // Verify it completes in reasonable time (should be <<1s without criterion overhead)
    assert!(
        elapsed.as_secs() < 1,
        "Benchmark simulation should complete in <1s, took {:?}",
        elapsed
    );
}
