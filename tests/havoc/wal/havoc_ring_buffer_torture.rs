use aletheiadb::storage::wal::LSN;
use aletheiadb::storage::wal::ring_buffer::{BackpressureConfig, PendingEntry, WalRingBuffer};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Barrier};
use std::thread;

#[test]
fn test_ring_buffer_torture() {
    // 👺 Havoc Configuration
    const NUM_PRODUCERS: usize = 4;
    const ENTRIES_PER_PRODUCER: usize = 100_000;
    const TOTAL_ENTRIES: usize = NUM_PRODUCERS * ENTRIES_PER_PRODUCER;
    const BUFFER_CAPACITY: usize = 1024; // Small to force wraps

    // Backpressure config with small limits to force logic
    let backpressure = BackpressureConfig {
        initial_spins: 10,
        max_spins: 100,
        base_sleep_us: 1,
        max_sleep_us: 10,
    };

    let buffer = Arc::new(WalRingBuffer::with_config(BUFFER_CAPACITY, backpressure));
    let barrier = Arc::new(Barrier::new(NUM_PRODUCERS + 1));
    let producers_done = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut producer_handles = Vec::new();

    // Spawn Producers
    for p_id in 0..NUM_PRODUCERS {
        let buffer = Arc::clone(&buffer);
        let barrier = Arc::clone(&barrier);
        producer_handles.push(thread::spawn(move || {
            barrier.wait();
            for i in 0..ENTRIES_PER_PRODUCER {
                // Generate unique value: p_id in high bits, sequence in low
                let val = ((p_id as u64) << 32) | (i as u64);
                let lsn = LSN(val);
                let entry = PendingEntry::new_async(lsn, val.to_le_bytes().to_vec());

                // Randomly sleep to simulate stalls
                if i % 1000 == 0 {
                    thread::yield_now();
                }

                // Retry until success (simulating aggressive writer)
                if let Err(returned_entry) = buffer.try_append(entry) {
                    // Manual backoff instead of blocking, to stress try_append
                    thread::yield_now();
                    // Use blocking append to finish if try_append keeps failing
                    // This exercises both code paths
                    buffer.append_blocking(returned_entry).unwrap();
                }
            }
        }));
    }

    // Spawn Consumer
    let buffer_clone = Arc::clone(&buffer);
    let barrier_clone = Arc::clone(&barrier);
    let producers_done_clone = Arc::clone(&producers_done);

    let consumer_handle = thread::spawn(move || {
        let mut received_counts = [0usize; NUM_PRODUCERS];
        let mut total_received = 0;
        let mut last_seq = [0u64; NUM_PRODUCERS]; // To check per-producer ordering

        barrier_clone.wait();

        while total_received < TOTAL_ENTRIES {
            let entries = buffer_clone.drain();

            if entries.is_empty() {
                if producers_done_clone.load(Ordering::Acquire) {
                    if total_received < TOTAL_ENTRIES {
                        panic!(
                            "Producers finished but only received {}/{} entries. Ring buffer dropped data?",
                            total_received, TOTAL_ENTRIES
                        );
                    }
                    break;
                }
                thread::yield_now();
                continue;
            }

            for entry in entries {
                total_received += 1;
                let val = u64::from_le_bytes(entry.data.as_slice().try_into().unwrap());
                let p_id = (val >> 32) as usize;
                let seq = val & 0xFFFFFFFF;

                // Verify ordering per producer
                if received_counts[p_id] > 0 {
                    assert_eq!(
                        seq,
                        last_seq[p_id] + 1,
                        "Out of order for producer {}: got {}, expected {}",
                        p_id,
                        seq,
                        last_seq[p_id] + 1
                    );
                } else {
                    assert_eq!(seq, 0, "First entry for producer {} was not 0", p_id);
                }

                last_seq[p_id] = seq;
                received_counts[p_id] += 1;
            }
        }

        received_counts
    });

    // Wait for producers
    for h in producer_handles {
        h.join().unwrap();
    }
    producers_done.store(true, Ordering::Release);

    // Wait for consumer
    let counts = consumer_handle.join().unwrap();

    // Verification
    for (p_id, count) in counts.iter().enumerate() {
        assert_eq!(
            *count, ENTRIES_PER_PRODUCER,
            "Producer {} count mismatch",
            p_id
        );
    }
}
