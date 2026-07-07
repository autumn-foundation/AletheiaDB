use super::*;
use crate::GLOBAL_INTERNER;
use crate::core::id::NodeId;
use crate::core::property::PropertyMap;
use crate::core::temporal::time;
use std::sync::Arc;
use std::thread;
use tempfile::tempdir;

fn test_operation() -> WalOperation {
    WalOperation::CreateNode {
        node_id: NodeId::new(1).unwrap(),
        label: GLOBAL_INTERNER.intern("Test").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    }
}

// ============================================================
// TDD Tests - Written FIRST to define expected behavior
// ============================================================

#[test]
fn test_concurrent_wal_creation() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    assert_eq!(wal.num_stripes(), DEFAULT_NUM_STRIPES);
    assert_eq!(wal.current_lsn(), LSN(1));
    assert_eq!(wal.total_appends(), 0);
}

#[test]
fn test_concurrent_wal_custom_stripes() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path()).with_num_stripes(8);
    let wal = ConcurrentWal::new(config).unwrap();

    assert_eq!(wal.num_stripes(), 8);
}

#[test]
fn test_concurrent_wal_stripe_rounding() {
    let dir = tempdir().unwrap();
    // 10 should round up to 16
    let config = ConcurrentWalConfig::new(dir.path()).with_num_stripes(10);
    let wal = ConcurrentWal::new(config).unwrap();

    assert_eq!(wal.num_stripes(), 16);
}

#[test]
fn test_append_async_allocates_lsn() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    let lsn1 = wal.append_async(test_operation()).unwrap();
    let lsn2 = wal.append_async(test_operation()).unwrap();
    let lsn3 = wal.append_async(test_operation()).unwrap();

    assert_eq!(lsn1, LSN(1));
    assert_eq!(lsn2, LSN(2));
    assert_eq!(lsn3, LSN(3));
    assert_eq!(wal.total_appends(), 3);
}

#[test]
fn test_append_with_handle() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    let (lsn, handle) = wal.append_with_handle(test_operation()).unwrap();

    assert_eq!(lsn, LSN(1));
    assert!(!handle.is_complete());
}

#[test]
fn test_drain_all_sorted_by_lsn() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path()).with_num_stripes(4);
    let wal = ConcurrentWal::new(config).unwrap();

    // Append from multiple threads to different stripes
    let wal = Arc::new(wal);
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let wal = Arc::clone(&wal);
            thread::spawn(move || {
                for _ in 0..10 {
                    wal.append_async(test_operation()).unwrap();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    // Drain should return entries sorted by LSN
    let entries = wal.drain_all();
    assert_eq!(entries.len(), 40);

    // Verify sorted order
    for i in 1..entries.len() {
        assert!(entries[i].lsn > entries[i - 1].lsn);
    }
}

#[test]
fn test_stripe_affinity() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path()).with_num_stripes(16);
    let wal = ConcurrentWal::new(config).unwrap();

    // Same thread should always use same stripe
    let stripe1 = wal.get_stripe().id();
    let stripe2 = wal.get_stripe().id();
    let stripe3 = wal.get_stripe().id();

    assert_eq!(stripe1, stripe2);
    assert_eq!(stripe2, stripe3);
}

#[test]
fn test_concurrent_appends() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path()).with_num_stripes(8);
    let wal = Arc::new(ConcurrentWal::new(config).unwrap());

    let num_threads = 8;
    let appends_per_thread = 100;

    let handles: Vec<_> = (0..num_threads)
        .map(|_| {
            let wal = Arc::clone(&wal);
            thread::spawn(move || {
                for _ in 0..appends_per_thread {
                    wal.append_async(test_operation()).unwrap();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(
        wal.total_appends(),
        (num_threads * appends_per_thread) as u64
    );
    assert_eq!(
        wal.current_lsn(),
        LSN((num_threads * appends_per_thread + 1) as u64)
    );
}

#[test]
fn test_close_prevents_appends() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    wal.close();
    assert!(wal.is_closed());

    let result = wal.append_async(test_operation());
    assert!(result.is_err());
}

#[test]
fn test_metrics() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path()).with_num_stripes(4);
    let wal = ConcurrentWal::new(config).unwrap();

    // Append some entries
    for _ in 0..10 {
        wal.append_async(test_operation()).unwrap();
    }

    let metrics = wal.metrics();
    assert_eq!(metrics.total_appends, 10);
    assert_eq!(metrics.current_lsn, LSN(11));
    assert_eq!(metrics.stripes.len(), 4);
    assert_eq!(metrics.total_pending, 10);
}

#[test]
fn test_set_next_lsn() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    wal.set_next_lsn(LSN(1000));
    assert_eq!(wal.current_lsn(), LSN(1000));

    let lsn = wal.append_async(test_operation()).unwrap();
    assert_eq!(lsn, LSN(1000));
}

#[test]
fn test_drain_stripe() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path()).with_num_stripes(4);
    let wal = ConcurrentWal::new(config).unwrap();

    // Append and check which stripe got it
    wal.append_async(test_operation()).unwrap();

    // One stripe should have an entry
    let total: usize = (0..4).map(|i| wal.drain_stripe(i).len()).sum();
    assert_eq!(total, 1);
}

#[test]
fn test_completion_notification_via_drain() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    let (_lsn, handle) = wal.append_with_handle(test_operation()).unwrap();
    assert!(!handle.is_complete());

    // Drain and notify
    let entries = wal.drain_all();
    for entry in &entries {
        entry.notify_completion();
    }

    assert!(handle.is_complete());
}

// ============================================================
// Batch Append Tests (Issue #219)
// ============================================================

#[test]
fn test_append_batch_allocates_consecutive_lsns() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    let ops = vec![test_operation(), test_operation(), test_operation()];

    let lsns = wal.append_batch(ops).unwrap();

    assert_eq!(lsns.len(), 3);
    assert_eq!(lsns[0], LSN(1));
    assert_eq!(lsns[1], LSN(2));
    assert_eq!(lsns[2], LSN(3));
    assert_eq!(wal.total_appends(), 3);
}

#[test]
fn test_append_batch_empty_operations() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    let ops: Vec<WalOperation> = vec![];
    let lsns = wal.append_batch(ops).unwrap();

    assert_eq!(lsns.len(), 0);
    assert_eq!(wal.total_appends(), 0);
}

#[test]
fn test_append_batch_single_operation() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    let ops = vec![test_operation()];
    let lsns = wal.append_batch(ops).unwrap();

    assert_eq!(lsns.len(), 1);
    assert_eq!(lsns[0], LSN(1));
    assert_eq!(wal.total_appends(), 1);
}

#[test]
fn test_append_batch_many_operations() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    // Create 100 operations to test batch efficiency
    let now = time::now();
    let ops: Vec<WalOperation> = (0..100)
        .map(|i| WalOperation::CreateNode {
            node_id: NodeId::new(i + 1).unwrap(),
            label: GLOBAL_INTERNER.intern(format!("Node{}", i)).unwrap(),
            properties: PropertyMap::new(),
            valid_from: now,
            provenance: None,
        })
        .collect();

    let lsns = wal.append_batch(ops).unwrap();

    assert_eq!(lsns.len(), 100);
    assert_eq!(lsns[0], LSN(1));
    assert_eq!(lsns[99], LSN(100));
    assert_eq!(wal.total_appends(), 100);
}

#[test]
fn test_append_batch_with_drain() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    let ops = vec![test_operation(), test_operation()];
    let lsns = wal.append_batch(ops).unwrap();

    let entries = wal.drain_all();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].lsn, lsns[0]);
    assert_eq!(entries[1].lsn, lsns[1]);
}

#[test]
fn test_append_batch_interleaved_with_single() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    // Single append
    let lsn1 = wal.append_async(test_operation()).unwrap();

    // Batch append
    let batch_lsns = wal
        .append_batch(vec![test_operation(), test_operation()])
        .unwrap();

    // Another single append
    let lsn4 = wal.append_async(test_operation()).unwrap();

    assert_eq!(lsn1, LSN(1));
    assert_eq!(batch_lsns[0], LSN(2));
    assert_eq!(batch_lsns[1], LSN(3));
    assert_eq!(lsn4, LSN(4));
    assert_eq!(wal.total_appends(), 4);
}

#[test]
fn test_concurrent_wal_accessors() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config.clone()).unwrap();

    assert_eq!(wal.wal_dir(), dir.path());
    assert_eq!(wal.config().num_stripes, config.num_stripes);
    assert!(wal.stripe(0).is_some());
    assert!(wal.stripe(1000).is_none());
}
