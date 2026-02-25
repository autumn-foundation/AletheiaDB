use aletheiadb::GLOBAL_INTERNER;
use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::wal::WalOperation;
use aletheiadb::storage::wal::concurrent::{ConcurrentWal, ConcurrentWalConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
fn test_shutdown_batch_atomicity() {
    // 🎯 Target: Verify that batched writes are atomic during shutdown using ConcurrentWal directly.
    // 💣 Risk: Race condition between shutdown and new batches.
    // 🧪 Strategy:
    //    1. Create ConcurrentWal.
    //    2. Spawn manual drainer thread (mimicking flush coordinator).
    //    3. Spawn multiple producer threads doing append_batch.
    //    4. Call shutdown_graceful on WAL from main thread.
    //    5. Verify accepted vs flushed.

    let dir = tempdir().unwrap();
    // Small buffer to force blocking/contention
    let config = ConcurrentWalConfig::new(dir.path())
        .with_num_stripes(4)
        .with_stripe_capacity(16);

    let wal = Arc::new(ConcurrentWal::new(config).unwrap());

    let accepted_items = Arc::new(AtomicU64::new(0));
    let flushed_items = Arc::new(AtomicU64::new(0));

    // 1. Spawn Drainer Thread
    let wal_drainer = wal.clone();
    let flushed_drainer = flushed_items.clone();

    let drainer_handle = thread::spawn(move || {
        loop {
            let entries = wal_drainer.drain_all();
            let count = entries.len() as u64;
            if count > 0 {
                flushed_drainer.fetch_add(count, Ordering::Relaxed);
                // Simulate I/O latency
                thread::yield_now();
            } else {
                if wal_drainer.is_closed() && wal_drainer.metrics().total_pending == 0 {
                    break;
                }
                // Sleep slightly to allow buffers to fill
                thread::sleep(Duration::from_micros(10));
            }
        }
    });

    // 2. Spawn Producer Threads
    let num_threads = 4;
    let batch_size = 10;
    let mut handles = Vec::new();
    let running = Arc::new(AtomicBool::new(true));

    for t in 0..num_threads {
        let wal_clone = wal.clone();
        let accepted_clone = accepted_items.clone();
        let running_clone = running.clone();

        handles.push(thread::spawn(move || {
            let mut i = 0;
            // Run until stopped
            while running_clone.load(Ordering::Relaxed) {
                i += 1;
                let start_id = (t * 1000000) + (i * batch_size);

                let ops: Vec<_> = (0..batch_size)
                    .map(|offset| create_test_operation(start_id as u64 + offset as u64))
                    .collect();

                // Call append_batch directly (no mutex)
                // This allows ConcurrentWal internal locking to manage concurrency
                let result = wal_clone.append_batch(ops);

                match result {
                    Ok(lsns) => {
                        assert_eq!(lsns.len(), batch_size);
                        accepted_clone.fetch_add(batch_size as u64, Ordering::Relaxed);
                    }
                    Err(_) => {
                        // WAL is shutting down, expected exit
                        break;
                    }
                }

                thread::yield_now();
            }
        }));
    }

    // 3. Let it run to generate load
    thread::sleep(Duration::from_millis(50));

    // 4. Trigger Shutdown
    println!("Triggering shutdown_graceful...");

    // Call shutdown_graceful
    // This should:
    // 1. Prevent NEW batches (append_batch returns Err)
    // 2. Wait for EXISTING batches to finish
    // 3. Close the WAL
    wal.shutdown_graceful();
    println!("Shutdown complete.");

    // Signal threads to stop if they haven't already (redundant if logic works)
    running.store(false, Ordering::Relaxed);

    // 5. Wait for threads
    for h in handles {
        let _ = h.join();
    }

    // Wait for drainer
    let _ = drainer_handle.join();

    // 6. Verify
    let accepted = accepted_items.load(Ordering::Relaxed);
    let flushed = flushed_items.load(Ordering::Relaxed);

    println!("Accepted Items: {}", accepted);
    println!("Total Flushed:  {}", flushed);

    assert_eq!(
        accepted, flushed,
        "Atomicity Mismatch! Accepted: {}, Flushed: {}",
        accepted, flushed
    );
}
