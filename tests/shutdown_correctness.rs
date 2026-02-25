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
