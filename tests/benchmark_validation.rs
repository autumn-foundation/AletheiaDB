//! Validation tests for benchmark assumptions
//!
//! These tests verify that the benchmarks in benches/performance_targets.rs
//! are testing what they claim to test, specifically around anchor creation
//! and delta reconstruction.
//!
//! **Timestamp Semantics**: AletheiaDB uses wallclock microsecond timestamps
//! for tx_time and valid_time. These tests capture actual timestamps after
//! updates and use them for queries, ensuring we're testing real time-travel
//! functionality.

use aletheiadb::Error;
use aletheiadb::api::transaction::WriteOps;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

/// Test that anchors are created at expected positions with default anchor_interval=10
///
/// This validates the assumptions made in the time-travel benchmarks by:
/// - Creating 10 updates and capturing timestamps at key points
/// - Querying at anchor point (after update 10)
/// - Verifying correct historical state reconstruction
#[test]
fn test_anchor_creation_matches_benchmark_assumptions() {
    let db = AletheiaDB::new().unwrap();

    // Create initial node
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create_node should succeed");

    // Create 10 updates and capture the commit timestamp at update 10 (anchor)
    let mut timestamp_at_10 = 0i64.into();
    for i in 1..=10 {
        let commit_ts = db
            .write_with_timestamp(|tx| {
                tx.update_node(
                    node_id,
                    PropertyMapBuilder::new()
                        .insert("name", "Alice")
                        .insert("version", i)
                        .build(),
                )?;
                Ok::<_, Error>(())
            })
            .expect("update_node should succeed")
            .1; // Extract commit timestamp

        // Capture actual commit timestamp at 10th update (anchor point)
        if i == 10 {
            timestamp_at_10 = commit_ts;
        }
    }

    // Verify historical stats
    let stats = db.historical_stats().expect("Should get stats");
    assert_eq!(
        stats.total_node_versions, 11,
        "Should have 11 versions (initial + 10 updates)"
    );
    assert_eq!(
        stats.node_anchor_count, 2,
        "Should have 2 anchors (at updates 1 and 11)"
    );
    assert_eq!(stats.node_delta_count, 9, "Should have 9 deltas");

    // Query at anchor point using actual timestamp
    let at_10 = db
        .get_node_at_time(node_id, timestamp_at_10, timestamp_at_10)
        .expect("Should be able to query at anchor timestamp");

    // Verify the version property is correct
    assert_eq!(
        at_10.properties.get("version"),
        Some(&10i64.into()),
        "Node at anchor should have version=10"
    );
}

/// Test that delta reconstruction actually happens for non-anchor queries
///
/// This test verifies that querying at different timestamps succeeds.
/// NOTE: Currently just validates that queries don't fail. Full temporal
/// validation requires deeper investigation of get_node_at_time semantics.
#[test]
fn test_delta_reconstruction_produces_correct_state() {
    let db = AletheiaDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create_node should succeed");

    // Create 15 updates with incrementing values and capture commit timestamps
    // This uses the actual commit timestamp from each transaction, ensuring
    // precise temporal semantics without relying on external timing coordination
    let mut commit_ts_5 = 0i64.into(); // Commit timestamp of update 5
    let mut commit_ts_9 = 0i64.into(); // Commit timestamp of update 9
    let mut commit_ts_10 = 0i64.into(); // Commit timestamp of update 10
    let mut commit_ts_15 = 0i64.into(); // Commit timestamp of update 15

    for i in 1..=15 {
        let commit_ts = db
            .write_with_timestamp(|tx| {
                tx.update_node(
                    node_id,
                    PropertyMapBuilder::new()
                        .insert("name", "Alice")
                        .insert("value", i)
                        .build(),
                )?;
                Ok::<_, Error>(())
            })
            .expect("update_node should succeed")
            .1; // Extract commit timestamp

        // Capture commit timestamps for specific updates
        if i == 5 {
            commit_ts_5 = commit_ts;
        } else if i == 9 {
            commit_ts_9 = commit_ts;
        } else if i == 10 {
            commit_ts_10 = commit_ts;
        } else if i == 15 {
            commit_ts_15 = commit_ts;
        }
    }

    // Verify historical stats
    let stats = db.historical_stats().expect("Should get stats");
    assert_eq!(
        stats.total_node_versions, 16,
        "Should have 16 versions (initial + 15 updates)"
    );
    assert_eq!(
        stats.node_anchor_count, 2,
        "Should have 2 anchors (at updates 1 and 11)"
    );
    assert_eq!(stats.node_delta_count, 14, "Should have 14 deltas");

    // Query at different time points using exact commit timestamps
    // This tests that temporal queries work correctly with the actual transaction_time
    let node_at_5 = db
        .get_node_at_time(node_id, commit_ts_5, commit_ts_5)
        .expect("Query at update 5 commit timestamp should succeed");
    let node_at_9 = db
        .get_node_at_time(node_id, commit_ts_9, commit_ts_9)
        .expect("Query at update 9 commit timestamp should succeed");
    let node_at_10 = db
        .get_node_at_time(node_id, commit_ts_10, commit_ts_10)
        .expect("Query at update 10 commit timestamp should succeed");
    let node_at_15 = db
        .get_node_at_time(node_id, commit_ts_15, commit_ts_15)
        .expect("Query at update 15 commit timestamp should succeed");

    // Verify each query returns the correct state at that exact commit timestamp
    assert_eq!(
        node_at_5.properties.get("value"),
        Some(&5i64.into()),
        "Query at commit_ts_5 should show value=5"
    );
    assert_eq!(
        node_at_9.properties.get("value"),
        Some(&9i64.into()),
        "Query at commit_ts_9 should show value=9"
    );
    assert_eq!(
        node_at_10.properties.get("value"),
        Some(&10i64.into()),
        "Query at commit_ts_10 should show value=10"
    );
    assert_eq!(
        node_at_15.properties.get("value"),
        Some(&15i64.into()),
        "Query at commit_ts_15 should show value=15"
    );
}

/// Test multiple updates in the same transaction
///
/// This tests the edge case where a transaction performs multiple updates to the
/// same entity. All operations in the transaction share the same commit_timestamp,
/// which requires explicit closing of previous versions before adding new ones.
#[test]
fn test_multiple_updates_same_transaction() {
    let db = AletheiaDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create_node should succeed");

    // Perform multiple updates in the SAME transaction
    // All updates will share the same commit_timestamp
    let (_result, commit_ts) = db
        .write_with_timestamp(|tx| {
            // First update in transaction
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .build(),
            )?;

            // Second update in same transaction
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 31i64)
                    .build(),
            )?;

            // Third update in same transaction
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 32i64)
                    .build(),
            )?;

            Ok::<_, Error>(())
        })
        .expect("Multiple updates in same transaction should succeed");

    // Query at the commit timestamp should show the final state (age=32)
    let node = db
        .get_node_at_time(node_id, commit_ts, commit_ts)
        .expect("Query at commit timestamp should succeed");

    assert_eq!(
        node.properties.get("age"),
        Some(&32i64.into()),
        "Query at commit_ts should show final update (age=32)"
    );

    // Verify historical stats
    // Initial version (age absent) + 3 updates = 4 versions total
    let stats = db.historical_stats().expect("Should get stats");
    assert_eq!(
        stats.total_node_versions, 4,
        "Should have 4 versions (initial + 3 updates in same tx)"
    );
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
    let db = AletheiaDB::new().unwrap();
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create_node should succeed");

    // Create 19 versions and capture commit timestamps for queries
    let mut timestamps = Vec::new();
    for i in 1..=19 {
        let (_result, commit_ts) = db
            .write_with_timestamp(|tx| {
                tx.update_node(
                    node_id,
                    PropertyMapBuilder::new()
                        .insert("name", "Alice")
                        .insert("version", i)
                        .build(),
                )
            })
            .expect("update_node should succeed");
        timestamps.push(commit_ts);
    }

    // Use actual commit timestamps for queries (not hardcoded 10, 5, 9)
    let ts_at_10 = timestamps[9]; // 10th update (0-indexed)
    let ts_at_5 = timestamps[4]; // 5th update
    let ts_at_9 = timestamps[8]; // 9th update

    // Perform queries (simulate benchmark iterations)
    for _ in 0..100 {
        let _ = db.get_node_at_time(node_id, ts_at_10, ts_at_10); // at_anchor
        let _ = db.get_node_at_time(node_id, ts_at_5, ts_at_5); // with_5_deltas
        let _ = db.get_node_at_time(node_id, ts_at_9, ts_at_9); // worst_case
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
