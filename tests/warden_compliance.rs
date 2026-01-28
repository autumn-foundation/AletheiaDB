use gallifreydb::storage::wal::ring_buffer::{WalRingBuffer, PendingEntry};
use gallifreydb::storage::wal::LSN;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_wal_ring_buffer_stress_dos() {
    // 🔒 Warden Compliance Test: DoS Resilience
    // Simulate a DoS attack where many threads try to fill the buffer
    // and the consumer is slow. This verifies the backpressure mechanism
    // (spinning/yielding) doesn't deadlock or consume 100% CPU forever
    // (it should yield).

    let buffer = Arc::new(WalRingBuffer::new(16)); // Small buffer to force contention
    let buffer_clone = Arc::clone(&buffer);
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    // Spawn consumer (slow)
    let consumer = thread::spawn(move || {
        let mut drained = 0;
        while running_clone.load(std::sync::atomic::Ordering::Acquire) {
            let entries = buffer_clone.drain();
            drained += entries.len();
            thread::sleep(Duration::from_millis(5)); // Slow consumer
        }
        // Final drain
        drained += buffer_clone.drain().len();
        drained
    });

    // Spawn producers (fast / many)
    let mut producers = vec![];
    for i in 0..10 {
        let buffer_clone = Arc::clone(&buffer);
        producers.push(thread::spawn(move || {
            let mut appended = 0;
            // Run for a short burst
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_millis(500) {
                // Try to append garbage data
                let entry = PendingEntry::new_async(LSN(i as u64), vec![0u8; 100]);
                // Use blocking append to test backpressure logic
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
    running.store(false, std::sync::atomic::Ordering::Release);
    let total_drained = consumer.join().unwrap();

    println!("Produced: {}, Drained: {}", total_produced, total_drained);

    // In a correct MPSC ring buffer, everything produced must be drained
    // because we didn't close the buffer until the end (and drain handles remaining).
    assert_eq!(total_produced, total_drained, "Data loss detected in WalRingBuffer!");
}
