use super::*;
use crate::GLOBAL_INTERNER;
use crate::core::id::NodeId;
use crate::core::property::PropertyMap;
use crate::core::temporal::time;
use tempfile::tempdir;

fn create_test_operation(id: u64) -> WalOperation {
    WalOperation::CreateNode {
        node_id: NodeId::new(id).unwrap(),
        label: GLOBAL_INTERNER.intern(format!("Node{}", id)).unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    }
}

#[test]
fn test_concurrent_wal_system_creation() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalSystemConfig::new(dir.path());
    let wal = ConcurrentWalSystem::new(config).unwrap();

    assert_eq!(wal.total_appends(), 0);
    assert_eq!(wal.current_lsn(), LSN(1));
}

#[test]
fn test_append_sync_mode() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalSystemConfig::new(dir.path())
        .with_durability_mode(DurabilityMode::Synchronous);
    let wal = ConcurrentWalSystem::new(config).unwrap();

    let lsn = wal.append(create_test_operation(1)).unwrap();
    assert_eq!(lsn, LSN(1));
    assert_eq!(wal.total_appends(), 1);
}

#[test]
fn test_append_sync_mode_handles_more_than_stripe_capacity() {
    let dir = tempdir().unwrap();
    let mut config = ConcurrentWalSystemConfig::new(dir.path())
        .with_durability_mode(DurabilityMode::Synchronous);
    // Keep capacity intentionally tiny to regression-test the benchmark footgun:
    // mode-aware `append()` must continue making progress even when the buffered
    // async path would hit backpressure quickly.
    config.stripe_capacity = 8;
    let wal = ConcurrentWalSystem::new(config).unwrap();

    for i in 1..=64 {
        let lsn = wal.append(create_test_operation(i)).unwrap();
        assert_eq!(lsn, LSN(i));
    }

    assert_eq!(wal.total_appends(), 64);
    assert_eq!(wal.total_flushed(), 64);
}

#[test]
fn test_append_async_mode() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalSystemConfig::new(dir.path())
        .with_flush_interval_ms(10_000) // Explicitly set config interval to avoid racing with default 10ms
        .with_durability_mode(DurabilityMode::Async {
            flush_interval_ms: 10_000,
        });
    let mut wal = ConcurrentWalSystem::new(config).unwrap();

    // Append several entries
    for i in 1..=10 {
        let lsn = wal.append(create_test_operation(i)).unwrap();
        assert_eq!(lsn, LSN(i));
    }

    assert_eq!(wal.total_appends(), 10);

    // Explicit flush - ensure all entries are durable.
    // Note: The background flush thread may have already flushed some/all
    // entries, so we check total_flushed() rather than the return stats.
    // This makes the test deterministic regardless of timing.
    wal.flush().unwrap();

    // Wait for flush to complete (handle race with background thread)
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(5);
    while wal.total_flushed() < 10 {
        // LCOV_EXCL_START
        if start.elapsed() > timeout {
            break;
        }
        // LCOV_EXCL_STOP
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(wal.total_flushed(), 10, "All 10 entries should be flushed");

    wal.shutdown();
    assert_eq!(wal.total_flushed(), 10, "All 10 entries should be flushed");
}

#[test]
fn test_concurrent_appends() {
    use std::sync::Arc;
    use std::thread;

    let dir = tempdir().unwrap();
    let config = ConcurrentWalSystemConfig::new(dir.path())
        .with_durability_mode(DurabilityMode::Async {
            flush_interval_ms: 100,
        })
        .with_num_stripes(4);
    let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

    let num_threads = 4;
    let ops_per_thread = 100;

    let handles: Vec<_> = (0..num_threads)
        .map(|t| {
            let wal = Arc::clone(&wal);
            thread::spawn(move || {
                for i in 0..ops_per_thread {
                    let id = (t * ops_per_thread + i + 1) as u64;
                    wal.append_async(create_test_operation(id)).unwrap();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(wal.total_appends(), (num_threads * ops_per_thread) as u64);
}

#[test]
fn test_flush_persists_entries() {
    let dir = tempdir().unwrap();
    // Use Synchronous mode to avoid background flush thread interference
    let config = ConcurrentWalSystemConfig::new(dir.path())
        .with_durability_mode(DurabilityMode::Synchronous);
    let mut wal = ConcurrentWalSystem::new(config).unwrap();

    // Append entries (append_async in Sync mode still buffers)
    for i in 1..=5 {
        wal.append_async(create_test_operation(i)).unwrap();
    }

    // Force flush - since no background thread, all 5 entries should be flushed here
    let stats = wal.flush().unwrap();
    assert_eq!(stats.entries_flushed, 5);

    // Verify flushed count
    assert_eq!(wal.total_flushed(), 5);

    wal.shutdown();
}

#[test]
fn test_group_commit_mode() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalSystemConfig::new(dir.path())
        .with_durability_mode(DurabilityMode::GroupCommit {
            max_batch_size: 10,
            max_delay_ms: 10,
        })
        .with_flush_interval_ms(5);
    let mut wal = ConcurrentWalSystem::new(config).unwrap();

    // Append entries
    for i in 1..=5 {
        wal.append(create_test_operation(i)).unwrap();
    }

    // Wait for background flush with polling (more resilient than single sleep)
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(5); // Increased timeout for CI
    let mut flushed = false;
    while start.elapsed() < timeout {
        if wal.total_flushed() >= 1 {
            flushed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    // Should have been flushed by background thread
    assert!(
        flushed,
        "Expected at least 1 entry to be flushed within {}ms, but got {} flushed",
        timeout.as_millis(),
        wal.total_flushed()
    );

    wal.shutdown();
}

#[test]
fn test_shutdown_flushes_remaining() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
        DurabilityMode::Async {
            flush_interval_ms: 100,
        },
    );
    let mut wal = ConcurrentWalSystem::new(config).unwrap();

    // Append entries without explicit flush
    for i in 1..=5 {
        wal.append_async(create_test_operation(i)).unwrap();
    }

    // Shutdown should flush remaining
    wal.shutdown();

    // All entries should be flushed
    assert_eq!(wal.total_flushed(), 5);
}

// ============================================================
// Batch Append Tests (Issue #219)
// ============================================================

#[test]
fn test_append_batch_async() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
        DurabilityMode::Async {
            flush_interval_ms: 10_000,
        },
    );
    let wal = ConcurrentWalSystem::new(config).unwrap();

    let ops = vec![
        create_test_operation(1),
        create_test_operation(2),
        create_test_operation(3),
    ];

    let lsns = wal.append_batch(ops).unwrap();

    assert_eq!(lsns.len(), 3);
    assert_eq!(lsns[0], LSN(1));
    assert_eq!(lsns[1], LSN(2));
    assert_eq!(lsns[2], LSN(3));
    assert_eq!(wal.total_appends(), 3);
}

#[test]
fn test_append_batch_sync() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalSystemConfig::new(dir.path())
        .with_durability_mode(DurabilityMode::Synchronous);
    let wal = ConcurrentWalSystem::new(config).unwrap();

    let ops = vec![create_test_operation(1), create_test_operation(2)];

    let lsns = wal.append_batch(ops).unwrap();

    assert_eq!(lsns.len(), 2);
    assert_eq!(lsns[0], LSN(1));
    assert_eq!(lsns[1], LSN(2));
    assert_eq!(wal.total_appends(), 2);
}

#[test]
fn test_append_batch_empty() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalSystemConfig::new(dir.path());
    let wal = ConcurrentWalSystem::new(config).unwrap();

    let lsns = wal.append_batch(vec![]).unwrap();

    assert_eq!(lsns.len(), 0);
    assert_eq!(wal.total_appends(), 0);
}

#[test]
fn test_append_batch_large() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
        DurabilityMode::Async {
            flush_interval_ms: 10_000,
        },
    );
    let wal = ConcurrentWalSystem::new(config).unwrap();

    // Create 100 operations
    let ops: Vec<_> = (1..=100).map(create_test_operation).collect();

    let lsns = wal.append_batch(ops).unwrap();

    assert_eq!(lsns.len(), 100);
    assert_eq!(lsns[0], LSN(1));
    assert_eq!(lsns[99], LSN(100));
    assert_eq!(wal.total_appends(), 100);
}

#[test]
fn test_append_sync_persistence_guarantee() {
    // This test verifies that append_sync actually waits for the flush.
    // While we can't easily deterministic race condition, we can verify basic
    // persistence guarantee: immediately after append_sync returns, total_flushed
    // must be incremented.

    let dir = tempdir().unwrap();
    // Use Synchronous mode
    let config = ConcurrentWalSystemConfig::new(dir.path())
        .with_durability_mode(DurabilityMode::Synchronous);
    let wal = ConcurrentWalSystem::new(config).unwrap();

    // 1. Initial state
    assert_eq!(wal.total_flushed(), 0);

    // 2. Perform append_sync
    let lsn = wal.append_sync(create_test_operation(1)).unwrap();

    // 3. Immediately assert flushed count
    // If append_sync didn't wait, and flush was async/delayed, this might fail.
    // But since it's sync, it MUST be 1.
    assert_eq!(
        wal.total_flushed(),
        1,
        "Should be flushed immediately after return"
    );
    assert_eq!(lsn, LSN(1));

    // 4. Batch append sync
    let ops = vec![create_test_operation(2), create_test_operation(3)];
    let lsns = wal.append_batch(ops).unwrap();

    // 5. Assert flushed count increased by 2
    assert_eq!(
        wal.total_flushed(),
        3,
        "Batch should be flushed immediately"
    );
    assert_eq!(lsns.len(), 2);
}
