use aletheiadb::storage::wal::{LSN, stripe::WalStripe};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

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

    thread::spawn(move || {
        // Wait for the main thread to be ready to block
        barrier_clone.wait();

        // Sleep to ensure the main thread is actually blocked waiting
        thread::sleep(Duration::from_millis(100));

        // Drain the stripe to free up space
        let drained = stripe_clone.drain();
        assert_eq!(drained.len(), 2, "Background thread should drain 2 entries");
    });

    // Synchronize start
    barrier.wait();

    // 4. Call `append_sync_blocking` with a 3rd entry.
    // If logic is correct, this will BLOCK until the background thread drains.
    // If logic is mutated (non-blocking), this will return Err(PendingEntry) immediately.
    let result = stripe.append_sync_blocking(LSN(3), vec![3]);

    // 5. Assert success
    assert!(
        result.is_ok(),
        "append_sync_blocking failed to wait for space"
    );

    // Verify the new entry is in the buffer
    assert_eq!(
        stripe.pending_count(),
        1,
        "New entry should be in the buffer"
    );

    // Verify total appends
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

    thread::spawn(move || {
        barrier_clone.wait();
        thread::sleep(Duration::from_millis(100));
        stripe_clone.drain();
    });

    barrier.wait();

    // 4. Call `append_blocking` with a 3rd entry.
    // Should block until drain.
    let result = stripe.append_blocking(LSN(30), vec![30]);

    // 5. Assert success
    assert!(result.is_ok(), "append_blocking failed to wait for space");

    // Verify the new entry is in the buffer
    assert_eq!(
        stripe.pending_count(),
        1,
        "New entry should be in the buffer"
    );
}
