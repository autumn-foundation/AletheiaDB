use super::*;
use std::sync::atomic::AtomicUsize;
use std::thread;

#[test]
fn test_completion_notifier_success() {
    let notifier = CompletionNotifier::new();
    assert!(!notifier.is_complete());

    notifier.notify_success();
    assert!(notifier.is_complete());

    // Wait should return immediately
    assert!(notifier.wait().is_ok());
}

#[test]
fn test_backpressure_config_equal_spins() {
    let config = BackpressureConfig {
        initial_spins: 10,
        max_spins: 10, // Equal is valid
        base_sleep_us: 1,
        max_sleep_us: 10,
    };
    // Should not panic
    let _ = WalRingBuffer::with_config(1024, config);
}

#[test]
fn test_completion_notifier_error() {
    let notifier = CompletionNotifier::new();

    notifier.notify_error("test error");
    assert!(notifier.is_complete());

    let result = notifier.wait();
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "test error");
}

#[test]
fn test_completion_handle_wait() {
    let notifier = Arc::new(CompletionNotifier::new());
    let handle = CompletionHandle(Arc::clone(&notifier));

    // Spawn thread to notify after a short delay
    let notifier_clone = Arc::clone(&notifier);
    let t = thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(10));
        notifier_clone.notify_success();
    });

    // Wait should block until notified
    assert!(handle.wait().is_ok());
    t.join().unwrap();
}

#[test]
fn test_ring_buffer_capacity_rounding() {
    let buf = WalRingBuffer::new(100);
    // Should round up to 128 (next power of 2)
    assert_eq!(buf.capacity(), 128);

    let buf = WalRingBuffer::new(64);
    assert_eq!(buf.capacity(), 64);

    let buf = WalRingBuffer::new(1);
    assert_eq!(buf.capacity(), 1);
}

#[test]
fn test_ring_buffer_single_thread() {
    let buf = WalRingBuffer::new(16);

    // Append some entries
    for i in 0..10 {
        let entry = PendingEntry::new_async(LSN(i), vec![i as u8]);
        assert!(buf.try_append(entry).is_ok());
    }

    assert_eq!(buf.len_approx(), 10);

    // Drain entries
    let entries = buf.drain();
    assert_eq!(entries.len(), 10);

    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(entry.lsn, LSN(i as u64));
        assert_eq!(entry.data, vec![i as u8]);
    }

    assert!(buf.is_empty_approx());
}

#[test]
fn test_ring_buffer_full() {
    let buf = WalRingBuffer::new(4);

    // Fill the buffer
    for i in 0..4 {
        let entry = PendingEntry::new_async(LSN(i), vec![]);
        assert!(buf.try_append(entry).is_ok());
    }

    // Next append should fail
    let entry = PendingEntry::new_async(LSN(4), vec![]);
    assert!(buf.try_append(entry).is_err());
}

#[test]
fn test_ring_buffer_closed() {
    let buf = WalRingBuffer::new(16);

    buf.close();
    assert!(buf.is_closed());

    let entry = PendingEntry::new_async(LSN(0), vec![]);
    assert!(buf.try_append(entry).is_err());
}

#[test]
fn test_ring_buffer_drain_empty() {
    let buf = WalRingBuffer::new(16);

    let entries = buf.drain();
    assert!(entries.is_empty());
}

#[test]
fn test_ring_buffer_concurrent_producers() {
    let buf = Arc::new(WalRingBuffer::new(1024));
    let num_threads = 8;
    let entries_per_thread = 100;
    let total_appended = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let buf = Arc::clone(&buf);
            let total = Arc::clone(&total_appended);

            thread::spawn(move || {
                for i in 0..entries_per_thread {
                    let lsn = LSN((thread_id * entries_per_thread + i) as u64);
                    let entry = PendingEntry::new_async(lsn, vec![thread_id as u8, i as u8]);

                    // Use blocking append to handle potential full buffer
                    if buf.append_blocking(entry).is_ok() {
                        total.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();

    // Wait for all producers
    for h in handles {
        h.join().unwrap();
    }

    // Drain and verify
    let entries = buf.drain();
    let total = total_appended.load(Ordering::Relaxed);

    assert_eq!(entries.len(), total);
    assert_eq!(total, num_threads * entries_per_thread);
}

#[test]
fn test_pending_entry_sync_mode() {
    let (entry, handle) = PendingEntry::new_sync(LSN(42), vec![1, 2, 3]);

    assert_eq!(entry.lsn, LSN(42));
    assert_eq!(entry.data, vec![1, 2, 3]);
    assert!(entry.completion.is_some());
    assert!(!handle.is_complete());

    entry.notify_completion();
    assert!(handle.is_complete());
}

#[test]
fn test_ring_buffer_slot_reuse() {
    // Note: This tests SLOT reuse (cycling through buffer slots), NOT position
    // counter wraparound (u64 overflow). See module docs for position overflow
    // limitations.
    let buf = WalRingBuffer::new(4);

    // First cycle - fill and drain
    for i in 0..4 {
        let entry = PendingEntry::new_async(LSN(i), vec![i as u8]);
        assert!(buf.try_append(entry).is_ok());
    }
    let entries = buf.drain();
    assert_eq!(entries.len(), 4);

    // Second cycle - should reuse slots
    for i in 4..8 {
        let entry = PendingEntry::new_async(LSN(i), vec![i as u8]);
        assert!(buf.try_append(entry).is_ok());
    }
    let entries = buf.drain();
    assert_eq!(entries.len(), 4);

    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(entry.lsn, LSN((i + 4) as u64));
    }
}

#[test]
fn test_ring_buffer_many_cycles() {
    // Test extensive slot reuse to verify sequence number progression is correct
    // over many cycles. This doesn't test u64 position overflow (which would
    // require 2^64 operations), but validates the sequence logic works for
    // realistic long-running scenarios.
    let buf = WalRingBuffer::new(4);

    // Run 1000 cycles (4000 operations total)
    for cycle in 0..1000u64 {
        for i in 0..4 {
            let lsn = LSN(cycle * 4 + i);
            let entry = PendingEntry::new_async(lsn, vec![(lsn.0 % 256) as u8]);
            assert!(
                buf.try_append(entry).is_ok(),
                "Failed at cycle {}, entry {}",
                cycle,
                i
            );
        }

        let entries = buf.drain();
        assert_eq!(entries.len(), 4, "Drain failed at cycle {}", cycle);

        // Verify LSNs are correct
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(
                entry.lsn,
                LSN(cycle * 4 + i as u64),
                "Wrong LSN at cycle {}, entry {}",
                cycle,
                i
            );
        }
    }
}

#[test]
fn test_ring_buffer_interleaved_append_drain() {
    let buf = WalRingBuffer::new(8);

    // Interleave appends and drains
    for cycle in 0..10 {
        for i in 0..3 {
            let lsn = LSN((cycle * 3 + i) as u64);
            let entry = PendingEntry::new_async(lsn, vec![]);
            assert!(buf.try_append(entry).is_ok());
        }

        let entries = buf.drain();
        assert_eq!(entries.len(), 3);
    }
}

#[test]
fn test_backpressure_config_default() {
    let config = BackpressureConfig::default();
    assert_eq!(config.initial_spins, 10);
    assert_eq!(config.max_spins, 1000);
    assert_eq!(config.base_sleep_us, 1);
    assert_eq!(config.max_sleep_us, 1000);
}

#[test]
fn test_backpressure_config_presets() {
    let low_latency = BackpressureConfig::low_latency();
    assert!(low_latency.initial_spins > BackpressureConfig::default().initial_spins);
    assert!(low_latency.max_sleep_us < BackpressureConfig::default().max_sleep_us);

    let high_throughput = BackpressureConfig::high_throughput();
    assert!(high_throughput.initial_spins < BackpressureConfig::default().initial_spins);
    assert!(high_throughput.max_sleep_us > BackpressureConfig::default().max_sleep_us);
}

#[test]
fn test_ring_buffer_with_custom_backpressure() {
    let config = BackpressureConfig {
        initial_spins: 5,
        max_spins: 50,
        base_sleep_us: 10,
        max_sleep_us: 100,
    };
    let buf = WalRingBuffer::with_config(4, config);

    // Should work normally
    let entry = PendingEntry::new_async(LSN(1), vec![1, 2, 3]);
    assert!(buf.try_append(entry).is_ok());

    let entries = buf.drain();
    assert_eq!(entries.len(), 1);
}

#[test]
fn test_backpressure_exponential_spin() {
    // Create a buffer with very low spin limits to test backoff kicks in quickly
    let config = BackpressureConfig {
        initial_spins: 2,
        max_spins: 8, // 2 -> 4 -> 8 (3 rounds)
        base_sleep_us: 0,
        max_sleep_us: 0,
    };
    let buf = WalRingBuffer::with_config(2, config);

    // Fill the buffer
    buf.try_append(PendingEntry::new_async(LSN(1), vec![]))
        .unwrap();
    buf.try_append(PendingEntry::new_async(LSN(2), vec![]))
        .unwrap();

    // Next append should fail after exponential backoff
    let result = buf.try_append(PendingEntry::new_async(LSN(3), vec![]));
    assert!(result.is_err());
}

#[test]
fn test_concurrent_append_and_drain() {
    use std::sync::{Barrier, atomic::AtomicBool};

    // Setup: Small buffer to force contention and wraps
    let buf = Arc::new(WalRingBuffer::new(16));
    let num_producers = 4;
    let items_per_producer = 1000;
    let total_items = num_producers * items_per_producer;

    let barrier = Arc::new(Barrier::new(num_producers + 1));
    let producers_done = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::new();

    // Spawn producers
    for p in 0..num_producers {
        let buf = Arc::clone(&buf);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for i in 0..items_per_producer {
                let val = (p * items_per_producer + i) as u64;
                let entry = PendingEntry::new_async(LSN(val), val.to_le_bytes().to_vec());
                buf.append_blocking(entry).unwrap();
            }
        }));
    }

    // Spawn consumer
    let buf_clone = Arc::clone(&buf);
    let barrier_clone = Arc::clone(&barrier);
    let done_flag = Arc::clone(&producers_done);

    let consumer_handle = thread::spawn(move || {
        let mut drained_count = 0;
        let mut checksum = 0u64;

        barrier_clone.wait();

        while drained_count < total_items {
            let entries = buf_clone.drain();
            if entries.is_empty() {
                // Check if producers are done when buffer is empty to avoid infinite loop
                // if for some reason we missed items (though logic below expects total_items)
                // This usage of done_flag is effectively just a liveness check/safety valve in this loop
                if done_flag.load(Ordering::Acquire) && drained_count < total_items {
                    // In a real test we expect to get everything, so just yield.
                    // But we use the flag to silence the unused variable warning
                    // and logically it makes sense to check if we should stop.
                }
                thread::yield_now();
                continue;
            }

            for entry in entries {
                drained_count += 1;
                // Verify data integrity
                // Access data by reference since Drop implementation prevents moving fields
                let val = u64::from_le_bytes(entry.data.as_slice().try_into().unwrap());
                checksum = checksum.wrapping_add(val);
            }
        }
        (drained_count, checksum)
    });

    // Wait for producers
    for h in handles {
        h.join().unwrap();
    }
    producers_done.store(true, Ordering::Release);

    // Wait for consumer
    let (drained, checksum) = consumer_handle.join().unwrap();

    assert_eq!(drained, total_items);

    // Calculate expected checksum
    let expected_checksum = (0..total_items as u64).fold(0u64, |sum, i| sum.wrapping_add(i));
    assert_eq!(checksum, expected_checksum);
}

#[test]
fn test_drop_safety() {
    let buf = WalRingBuffer::new(16);
    // Fill partially
    for i in 0..5 {
        buf.try_append(PendingEntry::new_async(LSN(i as u64), vec![]))
            .unwrap();
    }
    assert_eq!(buf.len_approx(), 5);
    // Drop should not panic
    drop(buf);
}

#[test]
fn test_completion_notifier_multiple_waiters() {
    let notifier = Arc::new(CompletionNotifier::new());
    let handle = CompletionHandle(Arc::clone(&notifier));
    let num_waiters = 10;
    let mut handles = Vec::new();

    for _ in 0..num_waiters {
        let h = handle.clone();
        handles.push(thread::spawn(move || {
            h.wait().unwrap();
        }));
    }

    // Wait a bit to ensure they are all blocked
    thread::sleep(std::time::Duration::from_millis(10));

    // Notify
    notifier.notify_success();

    // Join all
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn test_ring_buffer_getters() {
    let buf = WalRingBuffer::new(1024);
    assert_eq!(buf.capacity(), 1024);
    assert!(!buf.is_closed());
    buf.close();
    assert!(buf.is_closed());
}

#[cfg(test)]
impl WalRingBuffer {
    /// Helper to inject wraparound state for testing.
    ///
    /// This method is only available in test builds and allows tests to
    /// set up specific wraparound scenarios by directly manipulating the
    /// internal write/read positions and slot sequences.
    ///
    /// # Safety
    ///
    /// This method should only be called on a buffer that is not being
    /// concurrently accessed. It uses `Ordering::Relaxed` and clears all
    /// slot entries.
    pub fn set_state_for_wraparound_test(&mut self, write_pos: u64, read_pos: u64) {
        // Set positions
        self.write_pos.store(write_pos, Ordering::Relaxed);
        self.read_pos.store(read_pos, Ordering::Relaxed);

        // Initialize slots to represent an empty buffer at this position.
        // For an empty buffer, each slot should be available for writing when
        // the write position reaches it.
        //
        // Key insight: For position P, slot[P % capacity] needs sequence == P
        // to be available for writing.
        //
        // We initialize the next `capacity` positions starting from write_pos,
        // ensuring proper handling of wraparound (e.g., u64::MAX -> 0).
        for i in 0..self.capacity {
            // Calculate the position that will write to this slot next
            let slot_pos = write_pos.wrapping_add(i as u64);

            // Determine which slot index this position maps to
            let slot_idx = (slot_pos % self.capacity as u64) as usize;

            // Make slot available for writing at its position
            self.slots[slot_idx]
                .sequence
                .store(slot_pos, Ordering::Relaxed);

            // Clear entries
            unsafe {
                *self.slots[slot_idx].entry.get() = None;
            }
        }
    }
}

#[test]
fn test_wraparound_append_no_panic() {
    // Test that appending near u64::MAX doesn't panic due to integer overflow
    let mut buf = WalRingBuffer::new(4);

    // Set write position to be just before u64::MAX
    let start_pos = u64::MAX - 2;
    buf.set_state_for_wraparound_test(start_pos, start_pos);

    // Appending should succeed and wrap around without panicking
    for i in 0..4 {
        let lsn = LSN(i);
        let entry = PendingEntry::new_async(lsn, vec![]);
        buf.try_append(entry)
            .expect("Append should not panic near wraparound");
    }

    // Verify write position has wrapped
    let expected_pos = start_pos.wrapping_add(4);
    assert_eq!(buf.write_pos.load(Ordering::Relaxed), expected_pos);
}

#[test]
fn test_wraparound_drain_no_panic() {
    // Test that drain uses wrapping_add (no panic in debug mode)
    let mut buf = WalRingBuffer::new(4);

    // Set read position to be just before u64::MAX
    let start_pos = u64::MAX - 2;
    buf.set_state_for_wraparound_test(start_pos, start_pos);

    // Fill the buffer
    for i in 0..4 {
        let lsn = LSN(i);
        let entry = PendingEntry::new_async(lsn, vec![]);
        buf.try_append(entry).expect("Should append");
    }

    // Draining should succeed and wrap around without panicking
    let entries = buf.drain();
    assert_eq!(entries.len(), 4, "Should drain all entries near wraparound");

    // Verify read position has wrapped
    let expected_pos = start_pos.wrapping_add(4);
    assert_eq!(buf.read_pos.load(Ordering::Relaxed), expected_pos);
}

#[test]
fn test_wraparound_logic() {
    // Test that wrapping arithmetic correctly handles sequence comparisons
    let capacity = 4;
    let mut buf = WalRingBuffer::new(capacity);

    // Set state to be near u64::MAX to force wraparound
    let start_pos = u64::MAX - (capacity as u64 / 2);
    buf.set_state_for_wraparound_test(start_pos, start_pos);

    // 1. Append `capacity` items, which should wrap around u64::MAX
    for i in 0..capacity {
        let lsn = LSN(i as u64);
        let entry = PendingEntry::new_async(lsn, vec![i as u8]);
        assert!(
            buf.try_append(entry).is_ok(),
            "Append should succeed when buffer has space"
        );
    }

    // 2. Buffer should now be full, next append should fail
    let full_entry = PendingEntry::new_async(LSN(capacity as u64), vec![]);
    assert!(
        buf.try_append(full_entry).is_err(),
        "Append should fail when buffer is full across wraparound"
    );

    // 3. Drain all `capacity` items, which should also wrap around
    let entries = buf.drain();
    assert_eq!(
        entries.len(),
        capacity,
        "Should drain all appended entries across wraparound"
    );
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(entry.data, vec![i as u8]);
    }

    // 4. Buffer should be empty now
    assert!(
        buf.drain().is_empty(),
        "Buffer should be empty after draining"
    );

    // 5. Test one more append/drain cycle to ensure state is correct
    let final_entry = PendingEntry::new_async(LSN(100), vec![100]);
    assert!(buf.try_append(final_entry).is_ok());
    let final_drained = buf.drain();
    assert_eq!(final_drained.len(), 1);
    assert_eq!(final_drained[0].data, vec![100]);
}

#[test]
fn test_havoc_ring_buffer_len_approx_wraparound() {
    // 👺 HAVOC: Trigger u64 wraparound to prove len_approx underflows and breaks.
    let capacity = 4;
    let mut buf = WalRingBuffer::new(capacity);

    // Simulate being near u64::MAX
    let start_pos = u64::MAX - 2;
    buf.set_state_for_wraparound_test(start_pos, start_pos);

    // Append 3 items. write_pos will wrap around.
    for i in 0..3 {
        buf.try_append(PendingEntry::new_async(LSN(i), vec![]))
            .unwrap();
    }

    // At this point:
    // read_pos = u64::MAX - 2
    // write_pos = (u64::MAX - 2) + 3 = u64::MAX + 1 = 0
    //
    // Using saturating_sub: 0.saturating_sub(u64::MAX - 2) == 0
    // BUT there are 3 items in the buffer!

    let len = buf.len_approx();
    assert_eq!(
        len, 3,
        "👺 HAVOC SUCCESS: len_approx() failed on wraparound! Expected 3, got {}",
        len
    );
}

#[test]
#[should_panic(expected = "Ring buffer capacity must be > 0")]
fn test_ring_buffer_zero_capacity() {
    let _ = WalRingBuffer::new(0);
}

#[test]
#[should_panic(expected = "Invalid BackpressureConfig: \"max_spins must be >= initial_spins\"")]
fn test_backpressure_invalid_config() {
    let config = BackpressureConfig {
        initial_spins: 100,
        max_spins: 10, // Invalid: max < initial
        base_sleep_us: 1,
        max_sleep_us: 10,
    };
    let _ = WalRingBuffer::with_config(1024, config);
}

#[test]
#[should_panic(
    expected = "Invalid BackpressureConfig: \"initial_spins must be > 0 to prevent infinite spin loops\""
)]
fn test_backpressure_zero_initial_spins() {
    let config = BackpressureConfig {
        initial_spins: 0, // Invalid
        max_spins: 10,
        base_sleep_us: 1,
        max_sleep_us: 10,
    };
    let _ = WalRingBuffer::with_config(1024, config);
}
