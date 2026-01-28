use gallifreydb::storage::wal::LSN;
use gallifreydb::storage::wal::ring_buffer::{PendingEntry, WalRingBuffer};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

#[test]
fn test_wal_ring_buffer_stress_dos() {
    // 🔒 Warden Compliance Test: DoS Resilience
    // Simulate a DoS attack where many threads try to fill the buffer
    // and the consumer is slow. This verifies the backpressure mechanism
    // (spinning/yielding) doesn't deadlock.

    // CONSTANTS & MAGIC NUMBERS
    const BUFFER_CAPACITY: usize = 16; // Small buffer to force contention/backpressure immediately
    const CONSUMER_DELAY_MS: u64 = 5; // Slow consumer to ensure buffer fills up
    const TEST_DURATION_MS: u64 = 500; // Run long enough to hit contention but keep test fast
    const NUM_PRODUCERS: usize = 10; // Enough threads to ensure contention on the atomic write_pos
    const PAYLOAD_SIZE: usize = 100; // Arbitrary payload size

    let buffer = Arc::new(WalRingBuffer::new(BUFFER_CAPACITY));
    let buffer_clone = Arc::clone(&buffer);
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    // Global LSN counter for unique LSNs across threads
    let lsn_counter = Arc::new(AtomicU64::new(1));

    // Spawn consumer (slow)
    let consumer = thread::spawn(move || {
        let mut drained = 0;
        // Use SeqCst for test simplicity/correctness
        while running_clone.load(Ordering::SeqCst) {
            let entries = buffer_clone.drain();
            drained += entries.len();
            thread::sleep(Duration::from_millis(CONSUMER_DELAY_MS));
        }
        // Final drain after stop signal
        drained += buffer_clone.drain().len();
        drained
    });

    // Spawn producers (fast / many)
    let mut producers = vec![];
    for _ in 0..NUM_PRODUCERS {
        let buffer_clone = Arc::clone(&buffer);
        let lsn_counter = Arc::clone(&lsn_counter);
        producers.push(thread::spawn(move || {
            let mut appended = 0;
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_millis(TEST_DURATION_MS) {
                // Generate unique LSN for each entry
                let lsn = lsn_counter.fetch_add(1, Ordering::SeqCst);
                let entry = PendingEntry::new_async(LSN(lsn), vec![0u8; PAYLOAD_SIZE]);

                // Use blocking append to test backpressure logic
                // This will spin and then sleep/yield if buffer is full
                if buffer_clone.append_blocking(entry).is_ok() {
                    appended += 1;
                }
            }
            appended
        }));
    }

    // Wait for producers
    let mut total_produced = 0;
    for p in producers {
        total_produced += p.join().unwrap();
    }

    // Stop consumer
    running.store(false, Ordering::SeqCst);
    let total_drained = consumer.join().unwrap();

    println!("Produced: {}, Drained: {}", total_produced, total_drained);

    // In a correct MPSC ring buffer, everything produced must be drained
    // because we didn't close the buffer until the end (and drain handles remaining).
    assert_eq!(
        total_produced, total_drained,
        "Data loss detected in WalRingBuffer!"
    );
}
