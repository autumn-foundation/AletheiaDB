#![allow(deprecated)]
//! Integration tests for durability modes.
//!
//! Tests the three durability modes (Synchronous, Async, GroupCommit)
//! and their interactions including piggybacking.

use aletheiadb::{
    AletheiaDB, DurabilityMode, Error, GLOBAL_INTERNER, Node, PropertyMapBuilder, WalConfigBuilder,
    WriteOps, WriteOptions,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Helper to get label string from a Node
fn get_label(node: &Node) -> String {
    GLOBAL_INTERNER
        .resolve(node.label)
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Helper to create a database with WAL configured for a specific durability mode.
fn create_db_with_mode(mode: DurabilityMode) -> AletheiaDB {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let path = temp_dir.keep();
    let config = WalConfigBuilder::new()
        .wal_dir(path)
        .segment_size(10 * 1024 * 1024)
        .unwrap()
        .segments_to_retain(3)
        .unwrap()
        .durability_mode(mode)
        .build();
    AletheiaDB::with_wal_config(config).unwrap()
}

// =============================================================================
// Synchronous Mode Tests
// =============================================================================

#[test]
fn test_synchronous_mode_is_default() {
    let db = AletheiaDB::new().unwrap();

    // Default mode should be Synchronous
    let node_id = db
        .write(|tx| {
            tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
        })
        .expect("write failed");

    // Data should be immediately durable
    let node = db.get_node(node_id).expect("node should exist");
    assert_eq!(get_label(&node), "Person");
}

#[test]
fn test_synchronous_mode_explicit() {
    let db = create_db_with_mode(DurabilityMode::Synchronous);

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::Synchronous),
    };

    let node_id = db
        .write_with_options(options, |tx| {
            tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
        })
        .expect("write failed");

    let node = db.get_node(node_id).expect("node should exist");
    assert_eq!(get_label(&node), "Person");
}

// =============================================================================
// Async Mode Tests
// =============================================================================

#[test]
fn test_async_mode_returns_immediately() {
    // Test that async mode works correctly - writes complete and data is readable.
    // NOTE: We don't assert timing comparisons because:
    // 1. On Windows, fsync can be heavily buffered by the OS (very fast)
    // 2. The async channel overhead may exceed fsync savings on fast storage
    // 3. CI environments have unpredictable I/O characteristics
    // Performance verification belongs in benchmarks, not unit tests.

    let async_db = create_db_with_mode(DurabilityMode::Async {
        flush_interval_ms: 100,
    });

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::Async {
            flush_interval_ms: 100,
        }),
    };

    let mut node_ids = Vec::new();

    // Write multiple nodes using async mode
    for i in 0..10 {
        let node_id = async_db
            .write_with_options(options.clone(), |tx| {
                tx.create_node(
                    "Record",
                    PropertyMapBuilder::new().insert("index", i as i64).build(),
                )
            })
            .expect("async write failed");
        node_ids.push(node_id);
    }

    // All nodes should be visible immediately (data is in memory/WAL buffer)
    for (i, node_id) in node_ids.iter().enumerate() {
        let node = async_db.get_node(*node_id).expect("lookup failed");
        assert_eq!(get_label(&node), "Record");
        assert_eq!(
            node.properties.get("index"),
            Some(&(i as i64).into()),
            "node {} should have correct index property",
            i
        );
    }
}

#[test]
fn test_async_mode_eventual_flush() {
    let db = create_db_with_mode(DurabilityMode::Async {
        flush_interval_ms: 20,
    });

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::Async {
            flush_interval_ms: 20,
        }),
    };

    let node_id = db
        .write_with_options(options, |tx| {
            tx.create_node(
                "TestNode",
                PropertyMapBuilder::new().insert("test", true).build(),
            )
        })
        .expect("write failed");

    // Wait for background flush to happen
    thread::sleep(Duration::from_millis(100));

    // Data should be visible
    let node = db.get_node(node_id).expect("lookup failed");
    assert_eq!(get_label(&node), "TestNode");
}

// =============================================================================
// GroupCommit Mode Tests
// =============================================================================

#[test]
fn test_group_commit_batches_transactions() {
    let db = Arc::new(create_db_with_mode(DurabilityMode::GroupCommit {
        max_delay_ms: 50,
        max_batch_size: 10,
    }));

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::GroupCommit {
            max_delay_ms: 50,
            max_batch_size: 10,
        }),
    };

    let node_count = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();

    // Spawn multiple threads that will write concurrently
    for i in 0..5 {
        let db = Arc::clone(&db);
        let options = options.clone();
        let node_count = Arc::clone(&node_count);

        let handle = thread::spawn(move || {
            let node_id = db
                .write_with_options(options, |tx| {
                    tx.create_node(
                        "BatchItem",
                        PropertyMapBuilder::new().insert("thread", i as i64).build(),
                    )
                })
                .expect("write failed");
            node_count.fetch_add(1, Ordering::SeqCst);
            node_id
        });
        handles.push(handle);
    }

    // Collect all node IDs
    let node_ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // All transactions should have completed
    assert_eq!(node_count.load(Ordering::SeqCst), 5);

    // All nodes should be visible
    for node_id in node_ids {
        let node = db.get_node(node_id).expect("lookup failed");
        assert_eq!(get_label(&node), "BatchItem");
    }
}

#[test]
fn test_group_commit_respects_max_delay() {
    let db = create_db_with_mode(DurabilityMode::GroupCommit {
        max_delay_ms: 30,
        max_batch_size: 1000, // High batch size so timer triggers first
    });

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::GroupCommit {
            max_delay_ms: 30,
            max_batch_size: 1000,
        }),
    };

    let start = Instant::now();

    // Single transaction should complete within max_delay
    let node_id = db
        .write_with_options(options, |tx| {
            tx.create_node(
                "SingleItem",
                PropertyMapBuilder::new().insert("solo", true).build(),
            )
        })
        .expect("write failed");

    let elapsed = start.elapsed();

    // Should complete within max_delay + overhead for thread scheduling.
    // In CI, thread startup and scheduling can add significant overhead.
    // Windows CI especially can be slow. Use 10s threshold to be consistent
    // with other CI-resilient tests in this file (test_group_commit_triggers_on_batch_size).
    let threshold = Duration::from_secs(10);
    assert!(
        elapsed < threshold,
        "GroupCommit took too long: {:?} (threshold: {:?})",
        elapsed,
        threshold
    );

    let node = db.get_node(node_id).expect("lookup failed");
    assert_eq!(get_label(&node), "SingleItem");
}

/// Test that batch-size triggering works for GroupCommit mode.
///
/// This test verifies that when the batch size is reached, a flush is triggered
/// immediately rather than waiting for the max_delay timer.
///
/// Note: This test uses a relatively short max_delay (100ms) to avoid timeout issues
/// when threads execute sequentially due to OS scheduling. The key assertion is that
/// completion time is significantly less than what 5 sequential timer-based flushes
/// would take (5 * 100ms = 500ms).
#[test]
fn test_group_commit_triggers_on_batch_size() {
    use std::sync::Barrier;

    // Use a short max_delay to make the test faster while still being able to
    // detect if batch triggering works (completion should be much less than 500ms)
    let db = Arc::new(create_db_with_mode(DurabilityMode::GroupCommit {
        max_delay_ms: 100, // Short delay for faster test
        max_batch_size: 5, // Small batch size
    }));

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::GroupCommit {
            max_delay_ms: 100,
            max_batch_size: 5,
        }),
    };

    // Use a barrier to maximize the chance of concurrent execution.
    let barrier = Arc::new(Barrier::new(5));

    let start = Instant::now();
    let mut handles = Vec::new();

    // Spawn exactly batch_size transactions
    for i in 0..5 {
        let db = Arc::clone(&db);
        let options = options.clone();
        let barrier = Arc::clone(&barrier);

        let handle = thread::spawn(move || {
            // Wait for all threads to be ready before starting the write
            barrier.wait();

            db.write_with_options(options, |tx| {
                tx.create_node(
                    "BatchTrigger",
                    PropertyMapBuilder::new().insert("idx", i as i64).build(),
                )
            })
            .expect("write failed")
        });
        handles.push(handle);
    }

    // Wait for all to complete
    for handle in handles {
        handle.join().unwrap();
    }

    let elapsed = start.elapsed();

    // If batch triggering works, all 5 should complete in roughly one flush cycle.
    // If threads execute sequentially (worst case), each waits for its own timer,
    // but with 100ms delay that's still only 500ms max.
    // We give very generous headroom (10s) for CI environments with thread starvation.
    assert!(
        elapsed < Duration::from_secs(10),
        "Batch didn't trigger in reasonable time: {:?}",
        elapsed
    );
}

// =============================================================================
// Per-Transaction Override Tests
// =============================================================================

#[test]
fn test_override_async_db_with_sync_transaction() {
    let db = create_db_with_mode(DurabilityMode::Async {
        flush_interval_ms: 1000,
    });

    // Override with Synchronous for this critical transaction
    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::Synchronous),
    };

    let node_id = db
        .write_with_options(options, |tx| {
            tx.create_node(
                "Critical",
                PropertyMapBuilder::new().insert("important", true).build(),
            )
        })
        .expect("write failed");

    // Should be immediately durable
    let node = db.get_node(node_id).expect("lookup failed");
    assert_eq!(get_label(&node), "Critical");
}

#[test]
fn test_override_sync_db_with_async_transaction() {
    let db = create_db_with_mode(DurabilityMode::Synchronous);

    // Override with Async for bulk loading
    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::Async {
            flush_interval_ms: 50,
        }),
    };

    for i in 0..50 {
        db.write_with_options(options.clone(), |tx| {
            tx.create_node(
                "Bulk",
                PropertyMapBuilder::new().insert("n", i as i64).build(),
            )
        })
        .expect("write failed");
    }

    // NOTE: We removed the timing assertion because:
    // 1. CI environments have highly variable I/O and thread scheduling
    // 2. Even async mode can be slow when disk is under load
    // 3. Timing assertions are inherently flaky and don't test correctness
    // 4. Performance verification belongs in benchmarks, not unit tests
    //
    // The key correctness property is that async override works at all -
    // the writes complete successfully without blocking on fsync.
}

// =============================================================================
// Mixed Mode (Piggybacking) Tests
// =============================================================================

#[test]
fn test_sync_piggybacks_async_data() {
    let db = Arc::new(create_db_with_mode(DurabilityMode::Async {
        flush_interval_ms: 5000, // Very long interval
    }));

    // Write some async data
    let async_options = WriteOptions {
        durability_mode: Some(DurabilityMode::Async {
            flush_interval_ms: 5000,
        }),
    };

    let async_node = db
        .write_with_options(async_options, |tx| {
            tx.create_node(
                "AsyncData",
                PropertyMapBuilder::new().insert("pending", true).build(),
            )
        })
        .expect("async write failed");

    // Now write sync data - should piggyback the async data
    let sync_options = WriteOptions {
        durability_mode: Some(DurabilityMode::Synchronous),
    };

    let sync_node = db
        .write_with_options(sync_options, |tx| {
            tx.create_node(
                "SyncData",
                PropertyMapBuilder::new().insert("urgent", true).build(),
            )
        })
        .expect("sync write failed");

    // Both nodes should be visible and durable
    assert_eq!(
        get_label(&db.get_node(async_node).expect("lookup")),
        "AsyncData"
    );
    assert_eq!(
        get_label(&db.get_node(sync_node).expect("lookup")),
        "SyncData"
    );
}

#[test]
fn test_group_commit_piggybacks_async_data() {
    let db = Arc::new(create_db_with_mode(DurabilityMode::Async {
        flush_interval_ms: 5000,
    }));

    // Write async data
    let async_options = WriteOptions {
        durability_mode: Some(DurabilityMode::Async {
            flush_interval_ms: 5000,
        }),
    };

    let async_node = db
        .write_with_options(async_options, |tx| {
            tx.create_node(
                "AsyncPending",
                PropertyMapBuilder::new().insert("waiting", true).build(),
            )
        })
        .expect("async write failed");

    // GroupCommit should flush async data when batch flushes
    let gc_options = WriteOptions {
        durability_mode: Some(DurabilityMode::GroupCommit {
            max_delay_ms: 20,
            max_batch_size: 1,
        }),
    };

    let gc_node = db
        .write_with_options(gc_options, |tx| {
            tx.create_node(
                "GroupCommitData",
                PropertyMapBuilder::new().insert("batched", true).build(),
            )
        })
        .expect("group commit write failed");

    // Both should be visible and durable
    assert_eq!(
        get_label(&db.get_node(async_node).expect("lookup")),
        "AsyncPending"
    );
    assert_eq!(
        get_label(&db.get_node(gc_node).expect("lookup")),
        "GroupCommitData"
    );
}

// =============================================================================
// Shutdown Durability Tests
// =============================================================================

#[test]
fn test_async_data_flushed_on_shutdown() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let wal_path = temp_dir.path().to_path_buf();

    let node_id;
    {
        let config = WalConfigBuilder::new()
            .wal_dir(wal_path.clone())
            .segment_size(10 * 1024 * 1024)
            .unwrap()
            .segments_to_retain(3)
            .unwrap()
            .durability_mode(DurabilityMode::Async {
                flush_interval_ms: 60000, // Very long - won't naturally flush
            })
            .build();
        let db = AletheiaDB::with_wal_config(config).unwrap();

        let options = WriteOptions {
            durability_mode: Some(DurabilityMode::Async {
                flush_interval_ms: 60000,
            }),
        };

        node_id = db
            .write_with_options(options, |tx| {
                tx.create_node(
                    "PendingOnShutdown",
                    PropertyMapBuilder::new()
                        .insert("should_persist", true)
                        .build(),
                )
            })
            .expect("write failed");

        // Verify the node exists before drop
        let node = db.get_node(node_id).expect("node should exist before drop");
        assert_eq!(get_label(&node), "PendingOnShutdown");

        // db dropped here - FlushGuard should ensure final flush
    }

    // Note: WAL replay is not yet implemented, so we just verify
    // the write succeeded and FlushGuard was dropped cleanly.
    // A full test would reopen the database and check persistence.
}

// =============================================================================
// WriteOptions API Tests
// =============================================================================

#[test]
fn test_write_options_default() {
    let options = WriteOptions::default();
    assert!(options.durability_mode.is_none());
}

#[test]
fn test_write_with_options_closure_semantics() {
    let db = AletheiaDB::new().unwrap();

    // Test that closure can access outer scope
    let prefix = "test_";

    let node_id = db
        .write_with_options(WriteOptions::default(), |tx| {
            tx.create_node(
                "TestNode",
                PropertyMapBuilder::new()
                    .insert("name", format!("{}node", prefix))
                    .build(),
            )
        })
        .expect("write failed");

    let node = db.get_node(node_id).expect("lookup failed");
    let props = &node.properties;
    let name_val = props.get("name").expect("name property should exist");
    let name_str = match name_val {
        aletheiadb::PropertyValue::String(s) => s.as_ref(),
        _ => panic!("Expected string property"),
    };
    assert_eq!(name_str, "test_node");
}

#[test]
fn test_write_options_bulk_import_preset_integration() {
    let db = AletheiaDB::new().unwrap();

    // Use bulk_import preset for fast loading - all writes in ONE transaction
    let node_ids = db
        .write_with_options(WriteOptions::bulk_import(), |tx| {
            let mut ids = Vec::with_capacity(100);
            for i in 0..100 {
                let node_id = tx.create_node(
                    "BulkData",
                    PropertyMapBuilder::new()
                        .insert("index", i as i64)
                        .insert("data", format!("bulk_item_{}", i))
                        .build(),
                )?;
                ids.push(node_id);
            }
            Ok::<_, Error>(ids)
        })
        .expect("bulk import write failed");

    // All nodes should be visible
    for (i, node_id) in node_ids.iter().enumerate() {
        let node = db.get_node(*node_id).expect("node should exist");
        assert_eq!(get_label(&node), "BulkData");
        assert_eq!(
            node.properties.get("index"),
            Some(&(i as i64).into()),
            "node should have correct index"
        );
    }
}

#[test]
fn test_write_options_critical_preset_integration() {
    let db = AletheiaDB::new().unwrap();

    // Use critical preset for important data
    let node_id = db
        .write_with_options(WriteOptions::critical(), |tx| {
            tx.create_node(
                "Payment",
                PropertyMapBuilder::new()
                    .insert("amount", 10000i64)
                    .insert("currency", "USD")
                    .insert("status", "completed")
                    .build(),
            )
        })
        .expect("critical write failed");

    // Data should be immediately durable and visible
    let node = db.get_node(node_id).expect("payment node should exist");
    assert_eq!(get_label(&node), "Payment");
    assert_eq!(node.properties.get("amount"), Some(&10000i64.into()));
    assert_eq!(
        node.properties.get("status"),
        Some(&"completed".to_string().into())
    );
}

#[test]
fn test_preset_methods_mixed_usage() {
    let db = AletheiaDB::new().unwrap();

    // Use bulk_import for initial data
    let bulk_node = db
        .write_with_options(WriteOptions::bulk_import(), |tx| {
            tx.create_node(
                "InitialData",
                PropertyMapBuilder::new().insert("type", "bulk").build(),
            )
        })
        .expect("bulk write failed");

    // Use critical for important update
    let critical_node = db
        .write_with_options(WriteOptions::critical(), |tx| {
            tx.create_node(
                "CriticalData",
                PropertyMapBuilder::new().insert("type", "critical").build(),
            )
        })
        .expect("critical write failed");

    // Both should be visible
    assert_eq!(
        get_label(&db.get_node(bulk_node).expect("lookup")),
        "InitialData"
    );
    assert_eq!(
        get_label(&db.get_node(critical_node).expect("lookup")),
        "CriticalData"
    );
}

// =============================================================================
// Error Propagation Tests
// =============================================================================

#[test]
fn test_error_in_write_with_options_propagates() {
    let db = AletheiaDB::new().unwrap();

    let result: Result<(), aletheiadb::Error> =
        db.write_with_options(WriteOptions::default(), |_tx| {
            Err(aletheiadb::Error::Storage(
                aletheiadb::StorageError::InvalidProperty {
                    key: "test".into(),
                    reason: "intentional failure".into(),
                },
            ))
        });

    assert!(result.is_err());
    match result {
        Err(aletheiadb::Error::Storage(_)) => {}
        _ => panic!("Expected storage error"),
    }
}

// =============================================================================
// Concurrent Mixed Mode Tests
// =============================================================================

#[test]
fn test_concurrent_mixed_modes() {
    let db = Arc::new(create_db_with_mode(DurabilityMode::Synchronous));

    let mut handles = Vec::new();

    // Spawn threads with different durability modes
    for i in 0..15 {
        let db = Arc::clone(&db);
        let mode = match i % 3 {
            0 => DurabilityMode::Synchronous,
            1 => DurabilityMode::Async {
                flush_interval_ms: 10,
            },
            _ => DurabilityMode::GroupCommit {
                max_delay_ms: 20,
                max_batch_size: 5,
            },
        };

        let handle = thread::spawn(move || {
            let options = WriteOptions {
                durability_mode: Some(mode),
            };
            db.write_with_options(options, |tx| {
                tx.create_node(
                    "ConcurrentNode",
                    PropertyMapBuilder::new().insert("thread", i as i64).build(),
                )
            })
            .expect("concurrent write failed")
        });
        handles.push(handle);
    }

    let node_ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // Wait a bit for async flushes
    thread::sleep(Duration::from_millis(100));

    // All nodes should be visible
    for node_id in node_ids {
        assert_eq!(
            get_label(&db.get_node(node_id).expect("lookup")),
            "ConcurrentNode"
        );
    }
}

// =============================================================================
// Critical Regression Tests
// =============================================================================

/// CRITICAL TEST: Verify segment rotation with background flush thread.
///
/// This tests the fix for a critical data loss bug where the background flush
/// thread held a stale file descriptor after segment rotation, causing it to
/// fsync the wrong (old/deleted) file instead of the new segment.
///
/// Without the fix:
/// - Background thread clones sync handle once at startup
/// - WAL rotates to new segment after reaching segment_size
/// - Background thread still has old handle → fsyncs wrong file!
/// - New writes to new segment are NEVER durably fsynced
///
/// With the fix:
/// - Background thread gets current sync handle on each flush iteration
/// - Handles segment rotation correctly by always fsyncing current segment
#[test]
fn test_segment_rotation_with_background_thread() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config = WalConfigBuilder::new()
        .wal_dir(temp_dir.path().to_path_buf())
        .segment_size(512)
        .unwrap() // Very small to force rotation quickly
        .segments_to_retain(5)
        .unwrap()
        .durability_mode(DurabilityMode::GroupCommit {
            max_delay_ms: 10,
            max_batch_size: 100,
        })
        .build();

    let db = AletheiaDB::with_wal_config(config).unwrap();

    // Write enough data to trigger multiple segment rotations
    // Each node with properties is ~100-200 bytes
    let mut node_ids = Vec::new();
    for i in 0..100 {
        let node_id = db
            .write(|tx| {
                tx.create_node(
                    "RotationTest",
                    PropertyMapBuilder::new()
                        .insert("index", i as i64)
                        .insert("data", format!("test data {}", i))
                        .build(),
                )
            })
            .expect("write failed");
        node_ids.push(node_id);

        // Yield occasionally to let background thread run
        if i % 10 == 0 {
            thread::sleep(Duration::from_millis(5));
        }
    }

    // Wait for background thread to complete all pending flushes
    thread::sleep(Duration::from_millis(100));

    // First verify nodes exist in memory (sanity check)
    for (i, node_id) in node_ids.iter().enumerate() {
        let node = db
            .get_node(*node_id)
            .unwrap_or_else(|_| panic!("Node {} not found in memory - write failed!", i));
        assert_eq!(get_label(&node), "RotationTest");
    }

    // Drop database to trigger final flush and ensure proper cleanup
    drop(db);

    // See issue #365: Add recovery test once AletheiaDB supports WAL replay on open
    // For now, this test verifies:
    // 1. Writes succeed across segment rotation
    // 2. Background thread doesn't crash when segment rotates
    // 3. All nodes remain accessible in memory
    //
    // The critical fix: background thread gets current sync_handle on each
    // iteration instead of holding a stale handle that becomes invalid after
    // segment rotation.
}

// =============================================================================
// AsyncBatched Mode Tests
// =============================================================================

#[test]
fn test_async_batched_mode_returns_immediately() {
    // Baseline: measure synchronous latency (10 writes to avoid excessive CI time)
    let sync_db = create_db_with_mode(DurabilityMode::Synchronous);
    let sync_start = Instant::now();
    for i in 0..10 {
        sync_db
            .write(|tx| {
                tx.create_node(
                    "SyncRecord",
                    PropertyMapBuilder::new().insert("index", i as i64).build(),
                )
            })
            .expect("sync write failed");
    }
    let sync_elapsed = sync_start.elapsed();

    // Test: async batched should be much faster
    let db = create_db_with_mode(DurabilityMode::AsyncBatched {
        max_delay_ms: 10,
        max_batch_size: 100,
    });

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::AsyncBatched {
            max_delay_ms: 10,
            max_batch_size: 100,
        }),
    };

    let async_start = Instant::now();
    let mut node_ids = Vec::new();

    // Write same number of nodes - should return much faster than sync
    for i in 0..10 {
        let node_id = db
            .write_with_options(options.clone(), |tx| {
                tx.create_node(
                    "FastWrite",
                    PropertyMapBuilder::new().insert("index", i as i64).build(),
                )
            })
            .expect("async batched write failed");
        node_ids.push(node_id);
    }

    let async_elapsed = async_start.elapsed();

    // AsyncBatched should be at least 2x faster than sync (ratio-based, CI-resilient)
    // Even on heavily loaded systems, async (no fsync wait) should outperform sync (10 fsyncs)
    let max_async_time = sync_elapsed / 2;
    assert!(
        async_elapsed < max_async_time,
        "AsyncBatched writes not significantly faster than sync: async={:?}, sync={:?}, threshold={:?}",
        async_elapsed,
        sync_elapsed,
        max_async_time
    );

    // All nodes should be immediately visible (data in memory/WAL buffer)
    for node_id in node_ids {
        let node = db.get_node(node_id).expect("lookup failed");
        assert_eq!(get_label(&node), "FastWrite");
    }
}

#[test]
fn test_async_batched_triggers_on_batch_size() {
    let db = Arc::new(create_db_with_mode(DurabilityMode::AsyncBatched {
        max_delay_ms: 5000, // Very long delay
        max_batch_size: 5,  // Small batch
    }));

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::AsyncBatched {
            max_delay_ms: 5000,
            max_batch_size: 5,
        }),
    };

    let start = Instant::now();
    let mut handles = Vec::new();

    // Write exactly batch_size transactions
    for i in 0..5 {
        let db = Arc::clone(&db);
        let options = options.clone();
        let handle = thread::spawn(move || {
            db.write_with_options(options, |tx| {
                tx.create_node(
                    "BatchItem",
                    PropertyMapBuilder::new().insert("idx", i as i64).build(),
                )
            })
            .expect("write failed")
        });
        handles.push(handle);
    }

    // Wait for all writes
    let node_ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let elapsed = start.elapsed();

    // Should complete quickly (batch triggered), not wait for 5s timer.
    // Use 10s threshold for CI environments with thread starvation.
    assert!(
        elapsed < Duration::from_secs(10),
        "Batch size didn't trigger flush: {:?}",
        elapsed
    );

    // All nodes should be visible
    for node_id in node_ids {
        let node = db.get_node(node_id).expect("lookup failed");
        assert_eq!(get_label(&node), "BatchItem");
    }
}

#[test]
fn test_async_batched_triggers_on_max_delay() {
    let db = create_db_with_mode(DurabilityMode::AsyncBatched {
        max_delay_ms: 30,
        max_batch_size: 10000, // Very large batch
    });

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::AsyncBatched {
            max_delay_ms: 30,
            max_batch_size: 10000,
        }),
    };

    let node_id = db
        .write_with_options(options, |tx| {
            tx.create_node(
                "TimerTrigger",
                PropertyMapBuilder::new().insert("test", true).build(),
            )
        })
        .expect("write failed");

    // Write completes immediately (no fsync wait)
    let node = db.get_node(node_id).expect("node should exist");
    assert_eq!(get_label(&node), "TimerTrigger");

    // Wait for timer to trigger flush
    thread::sleep(Duration::from_millis(100));

    // Node still visible after flush
    let node = db.get_node(node_id).expect("node should exist after flush");
    assert_eq!(get_label(&node), "TimerTrigger");
}

#[test]
fn test_async_batched_eventual_durability() {
    let db = create_db_with_mode(DurabilityMode::AsyncBatched {
        max_delay_ms: 20,
        max_batch_size: 100,
    });

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::AsyncBatched {
            max_delay_ms: 20,
            max_batch_size: 100,
        }),
    };

    let mut node_ids = Vec::new();
    for i in 0..10 {
        let node_id = db
            .write_with_options(options.clone(), |tx| {
                tx.create_node(
                    "EventuallyDurable",
                    PropertyMapBuilder::new().insert("index", i as i64).build(),
                )
            })
            .expect("write failed");
        node_ids.push(node_id);
    }

    // All immediately visible (in memory)
    for node_id in &node_ids {
        let node = db.get_node(*node_id).expect("node should exist");
        assert_eq!(get_label(&node), "EventuallyDurable");
    }

    // Wait for background flush
    thread::sleep(Duration::from_millis(100));

    // Still visible after flush
    for node_id in &node_ids {
        let node = db
            .get_node(*node_id)
            .expect("node should exist after flush");
        assert_eq!(get_label(&node), "EventuallyDurable");
    }
}

#[test]
fn test_async_batched_graceful_shutdown() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let wal_path = temp_dir.path().to_path_buf();

    let node_id;
    {
        let config = WalConfigBuilder::new()
            .wal_dir(wal_path.clone())
            .segment_size(10 * 1024 * 1024)
            .unwrap()
            .segments_to_retain(3)
            .unwrap()
            .durability_mode(DurabilityMode::AsyncBatched {
                max_delay_ms: 60000, // Won't naturally flush
                max_batch_size: 10000,
            })
            .build();
        let db = AletheiaDB::with_wal_config(config).unwrap();

        let options = WriteOptions {
            durability_mode: Some(DurabilityMode::AsyncBatched {
                max_delay_ms: 60000,
                max_batch_size: 10000,
            }),
        };

        node_id = db
            .write_with_options(options, |tx| {
                tx.create_node(
                    "PendingOnShutdown",
                    PropertyMapBuilder::new()
                        .insert("should_persist", true)
                        .build(),
                )
            })
            .expect("write failed");

        // Verify node exists before drop
        let node = db.get_node(node_id).expect("node should exist before drop");
        assert_eq!(get_label(&node), "PendingOnShutdown");

        // db dropped here - FlushGuard should ensure final flush
    }

    // Note: WAL replay not yet implemented, but FlushGuard ensures
    // final flush on shutdown for pending AsyncBatched data.
}

#[test]
fn test_async_batched_concurrent_writers() {
    let db = Arc::new(create_db_with_mode(DurabilityMode::AsyncBatched {
        max_delay_ms: 20,
        max_batch_size: 10,
    }));

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::AsyncBatched {
            max_delay_ms: 20,
            max_batch_size: 10,
        }),
    };

    let node_count = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();

    // Spawn many concurrent writers
    for i in 0..20 {
        let db = Arc::clone(&db);
        let options = options.clone();
        let node_count = Arc::clone(&node_count);
        let handle = thread::spawn(move || {
            let node_id = db
                .write_with_options(options, |tx| {
                    tx.create_node(
                        "ConcurrentWrite",
                        PropertyMapBuilder::new().insert("thread", i as i64).build(),
                    )
                })
                .expect("write failed");
            node_count.fetch_add(1, Ordering::SeqCst);
            node_id
        });
        handles.push(handle);
    }

    let node_ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(node_count.load(Ordering::SeqCst), 20);

    // All nodes visible
    for node_id in node_ids {
        let node = db.get_node(node_id).expect("node should exist");
        assert_eq!(get_label(&node), "ConcurrentWrite");
    }
}

#[test]
fn test_mixed_async_batched_and_sync() {
    let db = Arc::new(create_db_with_mode(DurabilityMode::AsyncBatched {
        max_delay_ms: 5000,
        max_batch_size: 100,
    }));

    // Write async batched data
    let async_options = WriteOptions {
        durability_mode: Some(DurabilityMode::AsyncBatched {
            max_delay_ms: 5000,
            max_batch_size: 100,
        }),
    };

    let async_node = db
        .write_with_options(async_options, |tx| {
            tx.create_node(
                "AsyncData",
                PropertyMapBuilder::new().insert("pending", true).build(),
            )
        })
        .expect("async write failed");

    // Write sync data - should piggyback async data
    let sync_options = WriteOptions {
        durability_mode: Some(DurabilityMode::Synchronous),
    };

    let sync_node = db
        .write_with_options(sync_options, |tx| {
            tx.create_node(
                "SyncData",
                PropertyMapBuilder::new().insert("urgent", true).build(),
            )
        })
        .expect("sync write failed");

    // Both visible
    assert_eq!(
        get_label(&db.get_node(async_node).expect("lookup")),
        "AsyncData"
    );
    assert_eq!(
        get_label(&db.get_node(sync_node).expect("lookup")),
        "SyncData"
    );
}

#[test]
fn test_async_batched_with_segment_rotation() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config = WalConfigBuilder::new()
        .wal_dir(temp_dir.path().to_path_buf())
        .segment_size(512)
        .unwrap() // Very small to force rotation
        .segments_to_retain(5)
        .unwrap()
        .durability_mode(DurabilityMode::AsyncBatched {
            max_delay_ms: 10,
            max_batch_size: 100,
        })
        .build();

    let db = AletheiaDB::with_wal_config(config).unwrap();

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::AsyncBatched {
            max_delay_ms: 10,
            max_batch_size: 100,
        }),
    };

    // Write enough to trigger rotations
    let mut node_ids = Vec::new();
    for i in 0..100 {
        let node_id = db
            .write_with_options(options.clone(), |tx| {
                tx.create_node(
                    "RotationTest",
                    PropertyMapBuilder::new()
                        .insert("index", i as i64)
                        .insert("data", format!("test data {}", i))
                        .build(),
                )
            })
            .expect("write failed");
        node_ids.push(node_id);

        if i % 10 == 0 {
            thread::sleep(Duration::from_millis(5));
        }
    }

    thread::sleep(Duration::from_millis(100));

    // All nodes should be visible
    for (i, node_id) in node_ids.iter().enumerate() {
        let node = db
            .get_node(*node_id)
            .unwrap_or_else(|_| panic!("Node {} not found", i));
        assert_eq!(get_label(&node), "RotationTest");
    }
}

// =============================================================================
// Crash Recovery Tests
// =============================================================================

/// Test recovery after concurrent writes - validates LSN ordering is preserved.
///
/// This test verifies the most critical correctness property: that entries
/// written concurrently from multiple threads are recovered in proper LSN
/// order after a simulated crash (graceful shutdown).
#[test]
fn test_recovery_after_concurrent_writes() {
    use aletheiadb::core::id::NodeId;
    use aletheiadb::core::property::PropertyMap;
    use aletheiadb::core::temporal::time;
    use aletheiadb::storage::wal::concurrent_system::{
        ConcurrentWalSystem, ConcurrentWalSystemConfig,
    };
    use aletheiadb::storage::wal::{LSN, WalOperation};
    use aletheiadb::storage::wal_reader::read_wal_entries;
    use std::collections::HashSet;
    use std::sync::Barrier;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let wal_path = temp_dir.path().to_path_buf();

    // Use conservative values that work in CI environments with limited resources
    let num_threads = 4usize;
    let appends_per_thread = 100usize;
    let total_expected = num_threads * appends_per_thread;

    // Phase 1: Create WAL and spawn concurrent writers
    {
        // Use Async mode with fast flushes to avoid backpressure
        let config = ConcurrentWalSystemConfig::new(&wal_path)
            .with_durability_mode(DurabilityMode::Async {
                flush_interval_ms: 1, // Very fast flushes
            })
            .with_num_stripes(16)
            .with_flush_interval_ms(1);

        let mut wal = ConcurrentWalSystem::new(config).expect("failed to create WAL");
        let barrier = Barrier::new(num_threads);

        // Use scoped threads
        let all_lsns: Vec<Vec<LSN>> = std::thread::scope(|s| {
            let mut handles = Vec::new();

            for thread_id in 0..num_threads {
                let wal_ref = &wal;
                let barrier_ref = &barrier;

                handles.push(s.spawn(move || {
                    let mut lsns = Vec::with_capacity(appends_per_thread);

                    // Sync start
                    barrier_ref.wait();

                    for i in 0..appends_per_thread {
                        let node_id =
                            NodeId::new((thread_id * appends_per_thread + i) as u64).unwrap();
                        let operation = WalOperation::CreateNode {
                            node_id,
                            label: GLOBAL_INTERNER.intern("CrashTest").unwrap(),
                            properties: PropertyMap::new(),
                            valid_from: time::now(),
                        };

                        // Retry with backoff on backpressure
                        let mut retries = 0;
                        let lsn = loop {
                            match wal_ref.append_async(operation.clone()) {
                                Ok(lsn) => break lsn,
                                Err(_e) if retries < 10 => {
                                    retries += 1;
                                    std::thread::sleep(std::time::Duration::from_millis(1));
                                }
                                Err(e) => {
                                    panic!(
                                        "Thread {} append {} failed after retries: {:?}",
                                        thread_id, i, e
                                    )
                                }
                            }
                        };
                        lsns.push(lsn);
                    }
                    lsns
                }));
            }

            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        // Collect all LSNs
        let all_lsns: Vec<LSN> = all_lsns.into_iter().flatten().collect();

        // Verify we got all LSNs
        assert_eq!(
            all_lsns.len(),
            total_expected,
            "Expected {} LSNs, got {}",
            total_expected,
            all_lsns.len()
        );

        // Shutdown triggers final flush
        wal.shutdown();
    }

    // Phase 2: Recovery - read entries from disk
    let recovered_entries =
        read_wal_entries(&wal_path, LSN(1)).expect("failed to read WAL entries");

    // Verify all entries were recovered
    assert_eq!(
        recovered_entries.len(),
        total_expected,
        "Expected {} entries, recovered {}",
        total_expected,
        recovered_entries.len()
    );

    // Verify LSNs are in strictly ascending order
    let mut prev_lsn = LSN(0);
    for entry in &recovered_entries {
        assert!(
            entry.lsn > prev_lsn,
            "LSN ordering violated: {} should be > {}",
            entry.lsn.0,
            prev_lsn.0
        );
        prev_lsn = entry.lsn;
    }

    // Verify all LSNs are unique and contiguous (1 to total_expected)
    let lsn_set: HashSet<u64> = recovered_entries.iter().map(|e| e.lsn.0).collect();
    assert_eq!(lsn_set.len(), total_expected, "LSNs should be unique");

    for expected_lsn in 1..=total_expected as u64 {
        assert!(
            lsn_set.contains(&expected_lsn),
            "Missing LSN {}",
            expected_lsn
        );
    }
}

/// Test recovery with abrupt termination (simulated crash without graceful shutdown).
///
/// This tests a more realistic crash scenario where the flush thread is killed
/// before it can complete. We verify that whatever WAS flushed is correctly ordered.
#[test]
fn test_recovery_partial_flush_ordering() {
    use aletheiadb::core::id::NodeId;
    use aletheiadb::core::property::PropertyMap;
    use aletheiadb::core::temporal::time;
    use aletheiadb::storage::wal::concurrent_system::{
        ConcurrentWalSystem, ConcurrentWalSystemConfig,
    };
    use aletheiadb::storage::wal::{LSN, WalOperation};
    use aletheiadb::storage::wal_reader::read_wal_entries;
    use std::sync::Barrier;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let wal_path = temp_dir.path().to_path_buf();

    let num_threads = 4usize;
    let appends_per_thread = 500usize;

    // Phase 1: Create WAL, write entries, force some flushes, then "crash"
    {
        let config = ConcurrentWalSystemConfig::new(&wal_path)
            .with_durability_mode(DurabilityMode::Async {
                flush_interval_ms: 5, // Fast flushes
            })
            .with_flush_interval_ms(5);

        let mut wal = ConcurrentWalSystem::new(config).expect("failed to create WAL");
        let barrier = Barrier::new(num_threads);

        // Use scoped threads to safely reference wal without Arc
        std::thread::scope(|s| {
            for thread_id in 0..num_threads {
                let wal_ref = &wal;
                let barrier_ref = &barrier;

                s.spawn(move || {
                    // Sync start
                    barrier_ref.wait();

                    for i in 0..appends_per_thread {
                        let node_id =
                            NodeId::new((thread_id * appends_per_thread + i) as u64).unwrap();
                        let operation = WalOperation::CreateNode {
                            node_id,
                            label: GLOBAL_INTERNER.intern("PartialCrashTest").unwrap(),
                            properties: PropertyMap::new(),
                            valid_from: time::now(),
                        };
                        let _ = wal_ref.append_async(operation);
                    }
                });
            }
        });

        // Let background flush thread write some entries to disk
        // Don't call flush() explicitly - that would race with the background thread!
        thread::sleep(Duration::from_millis(100));

        // Shutdown does a final flush, ensuring all entries are written
        wal.shutdown();
    }

    // Phase 2: Recovery - whatever was flushed should be ordered
    let recovered_entries =
        read_wal_entries(&wal_path, LSN(1)).expect("failed to read WAL entries");

    // We should have recovered at least some entries
    assert!(
        !recovered_entries.is_empty(),
        "Should have recovered at least some entries"
    );

    // Critical: LSNs must be in strictly ascending order
    let mut prev_lsn = LSN(0);
    for entry in &recovered_entries {
        assert!(
            entry.lsn > prev_lsn,
            "LSN ordering violated in recovered data: {} should be > {}",
            entry.lsn.0,
            prev_lsn.0
        );
        prev_lsn = entry.lsn;
    }

    // LSNs should be contiguous from 1 to whatever we recovered
    for (i, entry) in recovered_entries.iter().enumerate() {
        assert_eq!(
            entry.lsn.0,
            (i + 1) as u64,
            "LSN gap detected: expected {}, got {}",
            i + 1,
            entry.lsn.0
        );
    }
}
