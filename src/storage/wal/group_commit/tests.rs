use super::*;
use std::sync::Arc;
use std::thread;

// ==================== Original Tests (Updated for Result-based API) ====================

#[test]
fn test_new_coordinator() {
    let coord = GroupCommitCoordinator::new(10, 200);
    assert_eq!(coord.current_epoch().unwrap(), 1); // Start at 1 (flushed_epoch=0 means nothing flushed)
    assert_eq!(coord.flushed_epoch().unwrap(), 0);
    assert_eq!(coord.current_batch_size().unwrap(), 0);
}

#[test]
fn test_register_transaction() {
    let coord = GroupCommitCoordinator::new(10, 5);

    // Register transactions
    for i in 0..4 {
        let (epoch, should_flush) = coord.register_transaction().unwrap();
        assert_eq!(epoch, 1); // All in epoch 1
        assert!(!should_flush, "should not flush at batch size {}", i + 1);
    }

    // Fifth transaction should trigger flush
    let (epoch, should_flush) = coord.register_transaction().unwrap();
    assert_eq!(epoch, 1); // Still in epoch 1
    assert!(should_flush, "should flush when batch is full");
}

#[test]
fn test_mark_flushed_advances_epoch() {
    let coord = GroupCommitCoordinator::new(10, 100);

    coord.register_transaction().unwrap();
    coord.register_transaction().unwrap();

    assert_eq!(coord.current_epoch().unwrap(), 1); // Start at epoch 1
    assert_eq!(coord.current_batch_size().unwrap(), 2);

    coord.mark_flushed(Ok(())).unwrap();

    assert_eq!(coord.current_epoch().unwrap(), 2); // Advances to epoch 2
    assert_eq!(coord.flushed_epoch().unwrap(), 1); // Epoch 1 has been flushed
    assert_eq!(coord.current_batch_size().unwrap(), 0);
}

#[test]
fn test_wait_for_flush_success() {
    let coord = Arc::new(GroupCommitCoordinator::new(100, 100));
    let coord_clone = Arc::clone(&coord);

    let (epoch, _) = coord.register_transaction().unwrap();

    // Spawn a thread to mark flushed after a short delay
    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        coord_clone.mark_flushed(Ok(())).unwrap();
    });

    // Wait should succeed
    let result = coord.wait_for_flush(epoch);
    assert!(result.is_ok());

    handle.join().unwrap();
}

#[test]
fn test_wait_for_flush_error_propagation() {
    let coord = Arc::new(GroupCommitCoordinator::new(100, 100));
    let coord_clone = Arc::clone(&coord);

    let (epoch, _) = coord.register_transaction().unwrap();

    // Spawn a thread to mark flushed with error
    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        coord_clone
            .mark_flushed(Err(Error::Storage(StorageError::WalError {
                reason: "disk full".to_string(),
            })))
            .unwrap();
    });

    // Wait should return the error
    let result = coord.wait_for_flush(epoch);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("disk full"));

    handle.join().unwrap();
}

#[test]
fn test_wait_for_flush_timeout() {
    // Use a config with very short timeout for testing
    let config = GroupCommitConfig {
        max_delay_ms: 10,
        max_batch_size: 100,
        timeout_multiplier: 2, // 10 * 2 = 20ms
        timeout_base_ms: 10,   // + 10 = 30ms
        timeout_min_ms: 20,    // clamp min
        timeout_max_ms: 100,   // clamp max
        recent_errors_capacity: 1024,
    };
    let coord = GroupCommitCoordinator::with_config(config);

    let (epoch, _) = coord.register_transaction().unwrap();

    // Wait without anyone calling mark_flushed - should timeout quickly
    let start = std::time::Instant::now();
    let result = coord.wait_for_flush(epoch);
    let elapsed = start.elapsed();

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("timeout"));

    // Verify it didn't take 10 seconds (default timeout)
    assert!(elapsed < Duration::from_millis(500));
}

#[test]
fn test_multiple_waiters() {
    let coord = Arc::new(GroupCommitCoordinator::new(100, 100));

    // Register multiple transactions
    let mut epochs = Vec::new();
    for _ in 0..5 {
        let (epoch, _) = coord.register_transaction().unwrap();
        epochs.push(epoch);
    }

    // All should be same epoch (epoch 1)
    assert!(epochs.iter().all(|&e| e == 1));

    // Spawn multiple waiting threads
    let mut handles = Vec::new();
    for _ in 0..5 {
        let coord_clone = Arc::clone(&coord);
        handles.push(thread::spawn(move || coord_clone.wait_for_flush(1))); // Wait for epoch 1
    }

    // Let them start waiting
    thread::sleep(Duration::from_millis(10));

    // Mark flushed
    coord.mark_flushed(Ok(())).unwrap();

    // All should succeed
    for handle in handles {
        let result = handle.join().unwrap();
        assert!(result.is_ok());
    }
}

#[test]
fn test_multiple_epochs() {
    let coord = GroupCommitCoordinator::new(10, 100);

    // First batch at epoch 1
    coord.register_transaction().unwrap();
    coord.register_transaction().unwrap();
    coord.mark_flushed(Ok(())).unwrap();

    assert_eq!(coord.current_epoch().unwrap(), 2); // Advanced to epoch 2

    // Second batch at epoch 2
    let (epoch, _) = coord.register_transaction().unwrap();
    assert_eq!(epoch, 2);

    coord.mark_flushed(Ok(())).unwrap();
    assert_eq!(coord.current_epoch().unwrap(), 3); // Advanced to epoch 3
}

#[test]
fn test_should_flush() {
    let coord = GroupCommitCoordinator::new(10, 3);

    assert!(!coord.should_flush().unwrap());

    coord.register_transaction().unwrap();
    assert!(!coord.should_flush().unwrap());

    coord.register_transaction().unwrap();
    assert!(!coord.should_flush().unwrap());

    coord.register_transaction().unwrap();
    assert!(coord.should_flush().unwrap());
}

#[test]
fn test_max_delay() {
    let coord = GroupCommitCoordinator::new(42, 100);
    assert_eq!(coord.max_delay(), Duration::from_millis(42));
}

#[test]
fn test_with_defaults() {
    let coord = GroupCommitCoordinator::with_defaults();
    assert_eq!(coord.max_delay(), Duration::from_millis(10));
    // Can't easily test max_batch_size without registering 200 transactions
}

#[test]
fn test_custom_timeout_config() {
    let config = GroupCommitConfig {
        max_delay_ms: 1,
        max_batch_size: 100,
        timeout_multiplier: 2,
        timeout_base_ms: 10,
        timeout_min_ms: 20,
        timeout_max_ms: 100,
        recent_errors_capacity: 1024,
    };
    let coord = GroupCommitCoordinator::with_config(config);

    let (epoch, _) = coord.register_transaction().unwrap();

    // Formula: clamp(1 * 2 + 10, 20, 100) = 20ms
    // Wait without anyone calling mark_flushed - should timeout quickly
    let start = std::time::Instant::now();
    let result = coord.wait_for_flush(epoch);
    let elapsed = start.elapsed();

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("timeout"));
    // Should be around 20ms. Let's check it's within a reasonable range.
    // We use a generous upper bound for slow CI.
    // We use a relaxed lower bound (5ms) to handle Windows CI scheduler jitter.
    assert!(elapsed >= Duration::from_millis(5));
    assert!(elapsed < Duration::from_millis(500));
}

#[test]
fn test_error_history_eviction() {
    // Config with very small history
    let config = GroupCommitConfig {
        max_delay_ms: 10,
        max_batch_size: 100,
        timeout_multiplier: 2,
        timeout_base_ms: 10,
        timeout_min_ms: 20,
        timeout_max_ms: 1000,
        recent_errors_capacity: 2, // Only keep 2 recent errors
    };
    let coord = GroupCommitCoordinator::with_config(config);

    // Epoch 1 fails
    let epoch1 = coord.start_flush().unwrap();
    coord
        .finish_flush(
            epoch1,
            Err(Error::Storage(StorageError::WalError {
                reason: "Fail 1".to_string(),
            })),
        )
        .unwrap();

    // Epoch 2 fails
    let epoch2 = coord.start_flush().unwrap();
    coord
        .finish_flush(
            epoch2,
            Err(Error::Storage(StorageError::WalError {
                reason: "Fail 2".to_string(),
            })),
        )
        .unwrap();

    // Epoch 3 fails (Evicts Epoch 1)
    let epoch3 = coord.start_flush().unwrap();
    coord
        .finish_flush(
            epoch3,
            Err(Error::Storage(StorageError::WalError {
                reason: "Fail 3".to_string(),
            })),
        )
        .unwrap();

    // Check Epoch 1 - Should be unknown/evicted
    let result1 = coord.wait_for_flush(epoch1);
    assert!(result1.is_err());
    assert!(
        result1
            .unwrap_err()
            .to_string()
            .contains("evicted from error history")
    );

    // Check Epoch 2 - Should be known error
    let result2 = coord.wait_for_flush(epoch2);
    assert!(result2.is_err());
    assert!(result2.unwrap_err().to_string().contains("Fail 2"));

    // Check Epoch 3 - Should be known error
    let result3 = coord.wait_for_flush(epoch3);
    assert!(result3.is_err());
    assert!(result3.unwrap_err().to_string().contains("Fail 3"));
}

#[test]
fn test_flush_race_condition() {
    let coord = GroupCommitCoordinator::new(100, 100);

    // 1. Transaction A registers (Epoch 1)
    let (epoch_a, _) = coord.register_transaction().unwrap();
    assert_eq!(epoch_a, 1);

    // 2. Flush starts (Epoch 1 is being flushed)
    // This advances current_epoch to 2
    let flushing_epoch = coord.start_flush().unwrap();
    assert_eq!(flushing_epoch, 1);
    assert_eq!(coord.current_epoch().unwrap(), 2);

    // 3. Transaction B registers (Should be Epoch 2)
    // Because start_flush advanced the epoch, B is not part of the current flush
    let (epoch_b, _) = coord.register_transaction().unwrap();
    assert_eq!(epoch_b, 2);

    // 4. Flush finishes successfully
    coord.finish_flush(flushing_epoch, Ok(())).unwrap();

    // 5. Transaction A should be done
    assert!(coord.wait_for_flush(epoch_a).is_ok());

    // 6. Transaction B should NOT be done (it needs Epoch 2 flush)
    // We expect it to timeout if we wait, but we can just check flushed_epoch
    assert_eq!(coord.flushed_epoch().unwrap(), 1);

    // 7. Flush Epoch 2
    let flushing_epoch_2 = coord.start_flush().unwrap();
    assert_eq!(flushing_epoch_2, 2);
    coord.finish_flush(flushing_epoch_2, Ok(())).unwrap();

    // 8. Transaction B should be done
    assert!(coord.wait_for_flush(epoch_b).is_ok());
}

#[test]
fn test_wait_for_flush_deadline_enforcement() {
    // Config: timeout ~100ms
    let config = GroupCommitConfig {
        max_delay_ms: 10,
        max_batch_size: 100,
        timeout_multiplier: 1, // 10ms
        timeout_base_ms: 10,   // + 10ms = 20ms
        timeout_min_ms: 50,    // clamp min -> 50ms
        timeout_max_ms: 200,   // clamp max -> 200ms
        recent_errors_capacity: 1024,
    };

    let coord = Arc::new(GroupCommitCoordinator::with_config(config));
    let coord_clone = Arc::clone(&coord);

    // Register a transaction for Epoch 1
    let (epoch, _) = coord.register_transaction().unwrap();

    // Spawn a thread that keeps triggering "spurious" wakeups every 10ms
    thread::spawn(move || {
        let start = std::time::Instant::now();
        // Run for 500ms (10x the timeout)
        while start.elapsed() < Duration::from_millis(500) {
            thread::sleep(Duration::from_millis(10));
            // Finish a future epoch (100) triggers notify_all but doesn't advance flushed_epoch
            let _ = coord_clone.finish_flush(100, Ok(()));
        }
    });

    let start = std::time::Instant::now();
    let result = coord.wait_for_flush(epoch);
    let elapsed = start.elapsed();

    // Should fail fast (~50ms) with timeout error
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("timeout"));

    // If bug exists, it will wait ~500ms (or whatever the thread runs for)
    // If fix works, it will timeout around 50ms.
    // We set threshold at 150ms to be safe for CI.
    assert!(
        elapsed < Duration::from_millis(150),
        "Wait took {:?}, expected < 150ms",
        elapsed
    );
}
