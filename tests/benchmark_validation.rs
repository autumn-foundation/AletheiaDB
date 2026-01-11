//! Validation tests for benchmark assumptions
//!
//! These tests verify that the benchmarks in benches/performance_targets.rs
//! are testing what they claim to test, specifically around anchor creation
//! and delta reconstruction.
//!
//! **Timestamp Semantics**: GallifreyDB uses wallclock microsecond timestamps
//! for tx_time and valid_time. These tests capture actual timestamps after
//! updates and use them for queries, ensuring we're testing real time-travel
//! functionality.

use gallifreydb::api::transaction::WriteOps;
use gallifreydb::{GallifreyDB, PropertyMapBuilder};

/// Test that anchors are created at expected positions with default anchor_interval=10
///
/// This validates the assumptions made in the time-travel benchmarks by:
/// - Creating 10 updates and capturing timestamps at key points
/// - Querying at anchor point (after update 10)
/// - Verifying correct historical state reconstruction
#[test]
fn test_anchor_creation_matches_benchmark_assumptions() {
    let db = GallifreyDB::new();

    // Create initial node
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create_node should succeed");

    // Create 10 updates and capture timestamp after update 10 (anchor)
    let mut timestamp_at_10 = 0i64;
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

        // Capture timestamp after 10th update (anchor point)
        if i == 10 {
            timestamp_at_10 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as i64;
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
    let db = GallifreyDB::new();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create_node should succeed");

    // Create 15 updates with incrementing values and capture timestamps
    // We capture timestamps AFTER each update commits to ensure we have
    // timestamps between committed transactions
    let mut timestamp_after_5 = 0i64; // State should be at value=5
    let mut timestamp_after_9 = 0i64; // State should be at value=9
    let mut timestamp_after_10 = 0i64; // State should be at value=10

    for i in 1..=15 {
        // Add small sleep before transaction to ensure different commit timestamps
        // Without this, all transactions complete in <30 microseconds
        std::thread::sleep(std::time::Duration::from_millis(2));

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

        // Capture timestamp AFTER commits 5, 9, 10
        // This gives us a timestamp between committed transactions
        if i == 5 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            timestamp_after_5 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as i64;
        } else if i == 9 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            timestamp_after_9 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as i64;
        } else if i == 10 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            timestamp_after_10 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as i64;
        }
    }

    // Capture timestamp after all updates
    std::thread::sleep(std::time::Duration::from_millis(1));
    let timestamp_after_15 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;

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

    // Query at different time points using actual timestamps
    // timestamp_after_5 should give us state at value=5 (after update 5 committed)
    let node_at_5 = db
        .get_node_at_time(node_id, timestamp_after_5, timestamp_after_5)
        .expect("Query after update 5 should succeed");
    let node_at_9 = db
        .get_node_at_time(node_id, timestamp_after_9, timestamp_after_9)
        .expect("Query after update 9 should succeed");
    let node_at_10 = db
        .get_node_at_time(node_id, timestamp_after_10, timestamp_after_10)
        .expect("Query after update 10 should succeed");
    let node_at_15 = db
        .get_node_at_time(node_id, timestamp_after_15, timestamp_after_15)
        .expect("Query after update 15 should succeed");

    // Verify each query returns the correct state for that time
    assert_eq!(
        node_at_5.properties.get("value"),
        Some(&5i64.into()),
        "Query after update 5 should show value=5"
    );
    assert_eq!(
        node_at_9.properties.get("value"),
        Some(&9i64.into()),
        "Query after update 9 should show value=9"
    );
    assert_eq!(
        node_at_10.properties.get("value"),
        Some(&10i64.into()),
        "Query after update 10 should show value=10"
    );
    assert_eq!(
        node_at_15.properties.get("value"),
        Some(&15i64.into()),
        "Query after update 15 should show value=15"
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
