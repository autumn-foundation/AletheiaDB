use aletheiadb::storage::wal::{LSN, stripe::WalStripe};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

/// 🤖 Sentinel Test: Kill potential mutants in WalStripe blocking behavior.
///
/// Goal: Verify that `append_sync_blocking` actually blocks when the buffer is full,
/// instead of returning an error or silently failing.
///
/// Mutants targeted:
/// - Replaced `append_entry_blocking` with `append_entry` (non-blocking) in `append_sync_blocking`.
/// - Incorrect backpressure logic in `WalRingBuffer`.
#[test]
fn test_stripe_append_sync_blocking_waits_when_full() {
    // 1. Create a stripe with very small capacity (2).
    // Note: WalStripe rounds up to power of 2, so 2 is valid.
    let stripe = Arc::new(WalStripe::with_capacity(0, 2));

    // 2. Fill the stripe with 2 entries.
    // We use async append for setup.
    stripe.append_async(LSN(1), vec![1]).unwrap();
    stripe.append_async(LSN(2), vec![2]).unwrap();

    assert_eq!(stripe.pending_count(), 2, "Stripe should be full");

    // 3. Spawn a background thread to drain the stripe after a delay.
    // This simulates the Flush Coordinator clearing space.
    let stripe_clone = Arc::clone(&stripe);
    let barrier = Arc::new(Barrier::new(2));
    let barrier_clone = Arc::clone(&barrier);

    // Tune sleep duration: large enough to survive scheduler jitter,
    // small enough to keep tests fast. 300ms is generous for CI.
    let sleep_duration = Duration::from_millis(300);

    let handle = thread::spawn(move || {
        // Wait for the main thread to be ready to block
        barrier_clone.wait();

        // Sleep to ensure the main thread is actually blocked waiting
        thread::sleep(sleep_duration);

        // Drain the stripe to free up space
        stripe_clone.drain().len()
    });

    // Synchronize start
    barrier.wait();

    // 4. Call `append_sync_blocking` with a 3rd entry.
    // If logic is correct, this will BLOCK until the background thread drains.
    // If logic is mutated (non-blocking), this will return Err(PendingEntry) immediately.
    let start = Instant::now();
    let result = stripe.append_sync_blocking(LSN(3), vec![3]);
    let duration = start.elapsed();

    // 5. Assert success
    assert!(result.is_ok(), "append_sync_blocking failed to wait for space");

    // 6. Verify blocking actually happened.
    // If the main thread wasn't scheduled for >300ms, the drain happens first,
    // and append succeeds immediately. This would be a false positive (mutant survives).
    // We assert duration > 50ms to catch this case (fail safe).
    // We expect duration ~300ms.
    assert!(duration >= Duration::from_millis(50),
        "Operation completed too fast ({:?}), suggesting it didn't block (or race condition occurred)",
        duration);

    // 7. Verify total items conservation
    let drained_count = handle.join().unwrap();
    let pending_count = stripe.pending_count();

    // We started with 2, added 1 -> Total 3.
    // They must be either drained or pending.
    assert_eq!(drained_count + pending_count, 3,
        "Total items should be 3. Drained: {}, Pending: {}", drained_count, pending_count);

    // Verify total appends recorded by stripe
    assert_eq!(stripe.total_appends(), 3);
}

/// 🤖 Sentinel Test: Kill potential mutants in WalStripe blocking behavior.
///
/// Goal: Verify that `append_blocking` actually blocks when the buffer is full.
///
/// Mutants targeted:
/// - Replaced `append_entry_blocking` with `append_entry` (non-blocking) in `append_blocking`.
#[test]
fn test_stripe_append_blocking_waits_when_full() {
    // 1. Create a stripe with very small capacity (2).
    let stripe = Arc::new(WalStripe::with_capacity(1, 2));

    // 2. Fill the stripe with 2 entries.
    stripe.append_async(LSN(10), vec![10]).unwrap();
    stripe.append_async(LSN(20), vec![20]).unwrap();

    assert_eq!(stripe.pending_count(), 2, "Stripe should be full");

    // 3. Spawn a background thread to drain.
    let stripe_clone = Arc::clone(&stripe);
    let barrier = Arc::new(Barrier::new(2));
    let barrier_clone = Arc::clone(&barrier);
    let sleep_duration = Duration::from_millis(300);

    let handle = thread::spawn(move || {
        barrier_clone.wait();
        thread::sleep(sleep_duration);
        stripe_clone.drain().len()
    });

    barrier.wait();

    // 4. Call `append_blocking` with a 3rd entry.
    // Should block until drain.
    let start = Instant::now();
    let result = stripe.append_blocking(LSN(30), vec![30]);
    let duration = start.elapsed();

    // 5. Assert success
    assert!(result.is_ok(), "append_blocking failed to wait for space");

    // 6. Verify blocking duration
    assert!(duration >= Duration::from_millis(50),
        "Operation completed too fast ({:?}), suggesting it didn't block (or race condition occurred)",
        duration);

    // 7. Verify total items
    let drained_count = handle.join().unwrap();
    let pending_count = stripe.pending_count();

    assert_eq!(drained_count + pending_count, 3,
        "Total items should be 3. Drained: {}, Pending: {}", drained_count, pending_count);
}
