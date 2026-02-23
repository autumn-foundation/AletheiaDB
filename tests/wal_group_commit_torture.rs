use aletheiadb::GLOBAL_INTERNER;
use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
use aletheiadb::storage::wal::{DurabilityMode, LSN, WalOperation};
use std::sync::Arc;
use std::thread;
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
fn test_group_commit_high_concurrency_consistency() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalSystemConfig::new(dir.path())
        .with_durability_mode(DurabilityMode::GroupCommit {
            max_batch_size: 10,
            max_delay_ms: 5,
        })
        .with_flush_interval_ms(5)
        .with_num_stripes(8);

    let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

    let num_threads = 10;
    let ops_per_thread = 50;

    let handles: Vec<_> = (0..num_threads)
        .map(|t| {
            let wal = Arc::clone(&wal);
            thread::spawn(move || {
                for i in 0..ops_per_thread {
                    let id = (t * ops_per_thread + i + 1) as u64;
                    let op = create_test_operation(id);

                    // 1. Append
                    let lsn = wal.append(op.clone()).expect("Append failed");

                    // 2. Commit (Wait for Group Commit)
                    // If append is async (GroupCommit mode), we must commit to ensure durability
                    if let Some(epoch) = wal.commit().expect("Commit failed") {
                        if let Some(gc) = wal.group_commit_coordinator() {
                            gc.wait_for_flush(epoch).expect("Wait for flush failed");
                        }
                    }

                    // 3. Verification: Immediately check if data is durable.
                    // If the race condition exists, wait_for_flush might return
                    // while data is still in the buffer (not yet on disk).
                    // We check that the entry with this LSN actually exists on disk.
                    // Note: This is an expensive check (I/O per op), but necessary for strict verification.
                    let recovered = wal.read_from(lsn).expect("Read failed");
                    assert!(
                        !recovered.is_empty(),
                        "Data loss detected: LSN {:?} not found on disk immediately after commit",
                        lsn
                    );
                    assert_eq!(recovered[0].lsn, lsn, "Read incorrect LSN");
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    // Final Verification
    assert!(wal.is_healthy(), "WAL reported flush errors");

    // Ensure all data is accounted for
    let total_expected = (num_threads * ops_per_thread) as u64;
    assert_eq!(
        wal.total_appends(),
        total_expected,
        "Total appends mismatch"
    );

    // Force flush ensures background thread is done
    wal.flush().unwrap();

    // Verify readable data count
    // read_from reads from disk. If data was lost (not flushed), count will be less.
    let recovered = wal.read_from(LSN(0)).unwrap();
    assert_eq!(
        recovered.len() as u64,
        total_expected,
        "Recovered entries count mismatch - DATA LOSS DETECTED"
    );

    // Check strict LSN sequence
    let mut lsns: Vec<u64> = recovered.iter().map(|e| e.lsn.0).collect();
    lsns.sort();

    // LSNs are allocated sequentially from 1
    // If any are missing, there's a gap.
    for (i, lsn) in lsns.iter().enumerate() {
        assert_eq!(
            *lsn,
            (i + 1) as u64,
            "Missing LSN in sequence: expected {}, got {}",
            i + 1,
            lsn
        );
    }
}
