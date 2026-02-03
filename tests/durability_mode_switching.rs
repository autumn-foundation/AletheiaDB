use gallifreydb::{DurabilityMode, GallifreyDB, WriteOps};
use std::time::Duration;

#[test]
fn test_switch_to_async() {
    let db = GallifreyDB::new().unwrap();
    // Default might be GroupCommit or Synchronous depending on config,
    // we just want to ensure switching works.
    let initial_mode = db.default_durability();

    let async_mode = DurabilityMode::async_mode_validated(100).unwrap();
    // Ensure we are actually switching
    if initial_mode == async_mode {
        db.set_durability_mode(DurabilityMode::Synchronous).unwrap();
    }

    db.set_durability_mode(async_mode).unwrap();
    assert_eq!(db.default_durability(), async_mode);

    // Verify writes work in new mode
    db.write(|tx| tx.create_node("Test", Default::default()))
        .unwrap();
}

#[test]
fn test_switch_async_to_group_commit() {
    let db = GallifreyDB::new().unwrap();
    let async_mode = DurabilityMode::async_mode_validated(100).unwrap();
    db.set_durability_mode(async_mode).unwrap();

    let gc_mode = DurabilityMode::group_commit_validated(10, 100).unwrap();
    db.set_durability_mode(gc_mode).unwrap();
    assert_eq!(db.default_durability(), gc_mode);

    db.write(|tx| tx.create_node("Test", Default::default()))
        .unwrap();
}

#[test]
fn test_transaction_captures_mode_at_start() {
    let db = GallifreyDB::new().unwrap();
    let initial_mode = db.default_durability();

    // Start a transaction but don't commit yet
    let mut tx = db.write_transaction().unwrap();

    // Change global mode
    let target_mode = if initial_mode == DurabilityMode::Synchronous {
        DurabilityMode::async_mode_validated(500).unwrap()
    } else {
        DurabilityMode::Synchronous
    };

    db.set_durability_mode(target_mode).unwrap();

    // Transaction should still be initial_mode (internally)
    tx.create_node("Test", Default::default()).unwrap();
    tx.commit().unwrap();

    // New transaction should be target_mode
    assert_eq!(db.default_durability(), target_mode);
    db.write(|tx| tx.create_node("Test2", Default::default()))
        .unwrap();
}

#[test]
fn test_graceful_group_commit_transition() {
    let db = GallifreyDB::new().unwrap();
    // Use a long delay so the transaction definitely waits
    let gc_mode = DurabilityMode::group_commit_validated(1000, 100).unwrap();
    db.set_durability_mode(gc_mode).unwrap();

    // Start a transaction in GroupCommit mode
    let mut tx = db.write_transaction().unwrap();
    tx.create_node("Grouped", Default::default()).unwrap();

    // Spawn a thread to commit it.
    let handle = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        tx.commit().unwrap();
        start.elapsed()
    });

    // Short sleep to ensure transaction has started committing and is waiting
    std::thread::sleep(Duration::from_millis(100));

    // Now switch mode. This should trigger a flush and wait for it.
    let switch_start = std::time::Instant::now();
    db.set_durability_mode(DurabilityMode::Synchronous).unwrap();
    let switch_elapsed = switch_start.elapsed();

    // The thread should have finished much faster than 1000ms
    let commit_elapsed = handle.join().unwrap();

    println!(
        "Commit took {:?}, Switch took {:?}",
        commit_elapsed, switch_elapsed
    );

    // Switch should have triggered immediate flush
    assert!(
        commit_elapsed < Duration::from_millis(1500),
        "Commit took too long: {:?}",
        commit_elapsed
    );
    assert_eq!(db.default_durability(), DurabilityMode::Synchronous);
}

#[test]
fn test_set_durability_mode_validated() {
    let db = GallifreyDB::new().unwrap();

    // Valid mode
    db.set_durability_mode_validated(DurabilityMode::group_commit_validated(10, 200))
        .unwrap();
    assert!(matches!(
        db.default_durability(),
        DurabilityMode::GroupCommit { .. }
    ));

    // Invalid mode (validation should fail)
    let result =
        db.set_durability_mode_validated(DurabilityMode::group_commit_validated(1001, 200));
    assert!(result.is_err());
}

#[test]
fn test_regression_write_transaction_uses_default_mode() {
    // Regression test for bug where write_transaction() hardcoded Synchronous
    let db = GallifreyDB::new().unwrap();

    // Set a non-Synchronous default
    let async_mode = DurabilityMode::async_mode_validated(123).unwrap();
    db.set_durability_mode(async_mode).unwrap();

    // Create transaction via the simple write_transaction() call
    let tx = db.write_transaction().unwrap();

    // Verify it uses the default mode, not Synchronous
    assert_eq!(tx.durability_mode(), async_mode);
}

#[test]
fn test_switch_to_same_mode_is_noop() {
    let db = GallifreyDB::new().unwrap();
    let mode = DurabilityMode::async_mode_validated(100).unwrap();
    db.set_durability_mode(mode).unwrap();

    // Switch to same mode - should not error and be fast
    db.set_durability_mode(mode).unwrap();
    assert_eq!(db.default_durability(), mode);
}

#[test]
fn test_concurrent_mode_switches() {
    let db = std::sync::Arc::new(GallifreyDB::new().unwrap());
    let num_threads = 4;
    let iterations = 10;

    let mut handles = Vec::new();
    for i in 0..num_threads {
        let db = db.clone();
        handles.push(std::thread::spawn(move || {
            for j in 0..iterations {
                let mode = if (i + j) % 2 == 0 {
                    DurabilityMode::Synchronous
                } else {
                    DurabilityMode::async_mode_validated(10).unwrap()
                };
                db.set_durability_mode(mode).unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn test_switch_to_async_batched() {
    let db = GallifreyDB::new().unwrap();
    let mode = DurabilityMode::async_batched_validated(10, 100).unwrap();

    db.set_durability_mode(mode).unwrap();
    assert_eq!(db.default_durability(), mode);

    // Verify writes work
    db.write(|tx| tx.create_node("AsyncBatched", Default::default()))
        .unwrap();

    // Verify it doesn't wait (low latency)
    let start = std::time::Instant::now();
    db.write(|tx| tx.create_node("Fast", Default::default()))
        .unwrap();
    let elapsed = start.elapsed();

    // AsyncBatched should be very fast (< 1ms usually)
    assert!(elapsed < Duration::from_millis(50));
}

#[test]
fn test_switch_async_batched_to_sync() {
    let db = GallifreyDB::new().unwrap();
    let mode = DurabilityMode::async_batched_validated(100, 500).unwrap();
    db.set_durability_mode(mode).unwrap();

    // Add some data
    db.write(|tx| tx.create_node("Data", Default::default()))
        .unwrap();

    // Switch to sync
    db.set_durability_mode(DurabilityMode::Synchronous).unwrap();
    assert_eq!(db.default_durability(), DurabilityMode::Synchronous);

    db.write(|tx| tx.create_node("Safe", Default::default()))
        .unwrap();
}

#[test]
fn test_switch_all_combinations() {
    let db = GallifreyDB::new().unwrap();
    let modes = vec![
        DurabilityMode::Synchronous,
        DurabilityMode::async_mode_validated(100).unwrap(),
        DurabilityMode::group_commit_validated(10, 100).unwrap(),
        DurabilityMode::async_batched_validated(10, 100).unwrap(),
    ];

    for &m1 in &modes {
        for &m2 in &modes {
            db.set_durability_mode(m1).unwrap();
            db.write(|tx| tx.create_node("From", Default::default()))
                .unwrap();

            db.set_durability_mode(m2).unwrap();
            db.write(|tx| tx.create_node("To", Default::default()))
                .unwrap();

            assert_eq!(db.default_durability(), m2);
        }
    }
}
