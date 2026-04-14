use aletheiadb::GLOBAL_INTERNER;
use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
use aletheiadb::storage::wal::{DurabilityMode, WalOperation};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

fn create_test_operation(id: u64) -> WalOperation {
    WalOperation::CreateNode {
        node_id: NodeId::new(id).unwrap(),
        label: GLOBAL_INTERNER.intern(format!("Node{}", id)).unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
    }
}

#[test]
fn test_shutdown_data_consistency() {
    // Regression test for "Zombie Write" bug.
    // Verifies that all accepted writes are flushed, even during shutdown.

    let dir = tempdir().unwrap();
    let mut config = ConcurrentWalSystemConfig::new(dir.path())
        .with_durability_mode(DurabilityMode::Async {
            flush_interval_ms: 5,
        })
        .with_num_stripes(1);
    config.stripe_capacity = 4; // Small capacity to force contention

    let wal = Arc::new(Mutex::new(ConcurrentWalSystem::new(config).unwrap()));
    let accepted_writes = Arc::new(AtomicU64::new(0));

    // Spawn producer
    let wal_clone = wal.clone();
    let accepted_clone = accepted_writes.clone();

    let producer = thread::spawn(move || {
        let mut i = 0;
        loop {
            i += 1;
            // Acquire lock and append
            let result = {
                if let Ok(guard) = wal_clone.lock() {
                    guard.append_async(create_test_operation(i))
                } else {
                    // Lock poisoned (shouldn't happen)
                    break;
                }
            };

            match result {
                Ok(_) => {
                    accepted_clone.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    // WAL closed or error. Stop producing.
                    break;
                }
            }

            // Yield to allow contention
            if i % 10 == 0 {
                thread::yield_now();
            }
        }
    });

    // Let it run to build up state
    thread::sleep(Duration::from_millis(200));

    // Shutdown
    {
        let mut guard = wal.lock().unwrap();
        guard.shutdown();
    } // Unlock releases mutex

    // Wait for producer to hit the closed error and exit
    producer.join().unwrap();

    // Verify consistency
    let guard = wal.lock().unwrap();
    let total_appends = guard.total_appends();

    // Rely on our external counter `accepted_writes` which tracks Ok() returns.
    // This strictly verifies that every write acknowledged by the system is durably flushed.
    let accepted = accepted_writes.load(Ordering::Relaxed);
    let flushed = guard.total_flushed();

    // If bug exists (flush thread dies before close):
    // 1. Flush thread drains X items.
    // 2. Producer writes item Y. Returns Ok. `accepted` increments.
    // 3. Wal closes.
    // 4. Item Y is never flushed.
    // 5. flushed < accepted.

    println!("Accepted: {}, Flushed: {}", accepted, flushed);
    assert_eq!(
        accepted, flushed,
        "Data loss detected: Accepted writes were not flushed"
    );

    // Also verify internal counters match
    assert_eq!(total_appends, flushed, "Internal counters mismatch");
}

#[test]
fn test_shutdown_batch_atomicity() {
    // Verifies that a batch append either fully succeeds or is fully rejected,
    // and never results in partial writes during shutdown.
    // This specifically tests the `active_batches` wait logic.

    let dir = tempdir().unwrap();

    // Use ConcurrentWal directly to allow simultaneous append and shutdown
    // (ConcurrentWalSystem shutdown requires &mut self which forces Mutex serialization)
    // We simulate the flush thread with a background drainer.
    use aletheiadb::storage::wal::concurrent::{ConcurrentWal, ConcurrentWalConfig};

    let config = ConcurrentWalConfig::new(dir.path())
        .with_stripe_capacity(2) // Capacity 2
        .with_num_stripes(1); // Single stripe

    let wal = Arc::new(ConcurrentWal::new(config).unwrap());

    // Background drainer (simulates flush thread)
    let wal_drain = Arc::clone(&wal);
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let running_clone = Arc::clone(&running);
    let collected_entries = Arc::new(Mutex::new(Vec::new()));
    let collected_clone = Arc::clone(&collected_entries);

    let drainer = thread::spawn(move || {
        while running_clone.load(Ordering::SeqCst) {
            let mut entries = wal_drain.drain_all();
            if !entries.is_empty() {
                collected_clone.lock().unwrap().append(&mut entries);
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Final drain
        let mut entries = wal_drain.drain_all();
        if !entries.is_empty() {
            collected_clone.lock().unwrap().append(&mut entries);
        }
    });

    // Thread 1: Append batch
    let wal_clone = Arc::clone(&wal);
    let writer = thread::spawn(move || {
        let ops = vec![
            create_test_operation(1),
            create_test_operation(2),
            create_test_operation(3),
            create_test_operation(4),
        ];
        wal_clone.append_batch(ops)
    });

    // Wait for writer to block
    thread::sleep(Duration::from_millis(50));

    // Thread 2: Shutdown
    wal.shutdown_graceful();

    // Stop drainer
    running.store(false, Ordering::SeqCst);
    drainer.join().unwrap();

    // Verify
    let result = writer.join().unwrap();
    assert!(
        result.is_ok(),
        "append_batch should succeed with graceful shutdown"
    );

    let entries = collected_entries.lock().unwrap();
    assert_eq!(entries.len(), 4, "Expected all 4 entries (full batch)");
}
