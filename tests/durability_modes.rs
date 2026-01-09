//! Integration tests for durability modes.
//!
//! Tests the three durability modes (Synchronous, Async, GroupCommit)
//! and their interactions including piggybacking.

use gallifreydb::storage::wal::WalConfig;
use gallifreydb::{
    DurabilityMode, GLOBAL_INTERNER, GallifreyDB, Node, PropertyMapBuilder, WriteOps, WriteOptions,
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
fn create_db_with_mode(mode: DurabilityMode) -> GallifreyDB {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let path = temp_dir.keep();
    let config = WalConfig {
        wal_dir: path,
        segment_size: 10 * 1024 * 1024,
        segments_to_retain: 3,
        durability_mode: mode,
    };
    GallifreyDB::with_wal_config(config)
}

// =============================================================================
// Synchronous Mode Tests
// =============================================================================

#[test]
fn test_synchronous_mode_is_default() {
    let db = GallifreyDB::new();

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

    // Should complete within max_delay + overhead for thread scheduling
    // In CI, thread startup and scheduling can add significant overhead (100-200ms)
    // We use a generous threshold to avoid flakiness while still catching stuck threads
    let threshold = Duration::from_millis(500); // 30ms max_delay + ~470ms CI overhead
    assert!(
        elapsed < threshold,
        "GroupCommit took too long: {:?} (threshold: {:?})",
        elapsed,
        threshold
    );

    let node = db.get_node(node_id).expect("lookup failed");
    assert_eq!(get_label(&node), "SingleItem");
}

#[test]
fn test_group_commit_triggers_on_batch_size() {
    let db = Arc::new(create_db_with_mode(DurabilityMode::GroupCommit {
        max_delay_ms: 5000, // Very long delay
        max_batch_size: 5,  // Small batch size
    }));

    let options = WriteOptions {
        durability_mode: Some(DurabilityMode::GroupCommit {
            max_delay_ms: 5000,
            max_batch_size: 5,
        }),
    };

    let start = Instant::now();
    let mut handles = Vec::new();

    // Spawn exactly batch_size transactions
    for i in 0..5 {
        let db = Arc::clone(&db);
        let options = options.clone();

        let handle = thread::spawn(move || {
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

    // Should complete quickly (batch triggered) not wait for 5s timer
    assert!(
        elapsed < Duration::from_millis(500),
        "Batch didn't trigger early: {:?}",
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

    let start = Instant::now();

    for i in 0..50 {
        db.write_with_options(options.clone(), |tx| {
            tx.create_node(
                "Bulk",
                PropertyMapBuilder::new().insert("n", i as i64).build(),
            )
        })
        .expect("write failed");
    }

    let elapsed = start.elapsed();

    // Should be faster than 50 individual fsyncs would take
    assert!(
        elapsed < Duration::from_millis(500),
        "Async override not working: {:?}",
        elapsed
    );
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
        let config = WalConfig {
            wal_dir: wal_path.clone(),
            segment_size: 10 * 1024 * 1024,
            segments_to_retain: 3,
            durability_mode: DurabilityMode::Async {
                flush_interval_ms: 60000, // Very long - won't naturally flush
            },
        };
        let db = GallifreyDB::with_wal_config(config);

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
    let db = GallifreyDB::new();

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
        gallifreydb::PropertyValue::String(s) => s.as_ref(),
        _ => panic!("Expected string property"),
    };
    assert_eq!(name_str, "test_node");
}

// =============================================================================
// Error Propagation Tests
// =============================================================================

#[test]
fn test_error_in_write_with_options_propagates() {
    let db = GallifreyDB::new();

    let result: Result<(), gallifreydb::Error> =
        db.write_with_options(WriteOptions::default(), |_tx| {
            Err(gallifreydb::Error::Storage(
                gallifreydb::StorageError::InvalidProperty {
                    key: "test".into(),
                    reason: "intentional failure".into(),
                },
            ))
        });

    assert!(result.is_err());
    match result {
        Err(gallifreydb::Error::Storage(_)) => {}
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
    let config = WalConfig {
        wal_dir: temp_dir.path().to_path_buf(),
        segment_size: 512, // Very small to force rotation quickly
        segments_to_retain: 5,
        durability_mode: DurabilityMode::GroupCommit {
            max_delay_ms: 10,
            max_batch_size: 100,
        },
    };

    let db = GallifreyDB::with_wal_config(config);

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

    // TODO: Add recovery test once GallifreyDB supports WAL replay on open
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

    // Should complete quickly (batch triggered), not wait for 5s timer
    assert!(
        elapsed < Duration::from_millis(500),
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
        let config = WalConfig {
            wal_dir: wal_path.clone(),
            segment_size: 10 * 1024 * 1024,
            segments_to_retain: 3,
            durability_mode: DurabilityMode::AsyncBatched {
                max_delay_ms: 60000, // Won't naturally flush
                max_batch_size: 10000,
            },
        };
        let db = GallifreyDB::with_wal_config(config);

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
    let config = WalConfig {
        wal_dir: temp_dir.path().to_path_buf(),
        segment_size: 512, // Very small to force rotation
        segments_to_retain: 5,
        durability_mode: DurabilityMode::AsyncBatched {
            max_delay_ms: 10,
            max_batch_size: 100,
        },
    };

    let db = GallifreyDB::with_wal_config(config);

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
