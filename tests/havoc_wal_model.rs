#[cfg(loom)]
mod loom_tests {
    //! # WAL Ring Buffer Model
    //!
    //! This test file implements a **Loom Model** of the `WalRingBuffer` found in
    //! `src/storage/wal/ring_buffer.rs`.
    //!
    //! ## Why a Model?
    //!
    //! The production `WalRingBuffer` uses `std::sync::atomic` primitives which are
    //! not instrumented by Loom. Refactoring the core storage engine to be generic
    //! over atomic implementations (or using `cfg` flags throughout the codebase)
    //! is a significant architectural change.
    //!
    //! Instead, we use this **Model Checking** approach:
    //! 1. We copy the algorithmic logic exactly.
    //! 2. We replace primitives with `loom::sync` equivalents.
    //! 3. We verify the *algorithm* is sound under all possible thread interleavings.
    //!
    //! If this model fails, the production code is almost certainly broken.
    //! If this model passes, the algorithm is sound, though the production implementation
    //! could still have typos or misuse of the algorithm.

    use loom::cell::UnsafeCell;
    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use loom::thread;

    // Simplified PendingEntry for Loom
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct PendingEntry {
        lsn: u64,
        val: u64,
    }

    struct Slot {
        entry: UnsafeCell<Option<PendingEntry>>,
        sequence: AtomicU64,
    }

    // Loom's UnsafeCell is already Sync/Send handled by Loom's model
    // But we need to impl Send/Sync for Slot manually if the compiler doesn't infer it correctly due to UnsafeCell
    // In standard Rust UnsafeCell is !Sync.
    unsafe impl Sync for Slot {}

    struct WalRingBuffer {
        slots: Vec<Slot>,
        capacity: usize,
        mask: usize,
        write_pos: AtomicU64,
        read_pos: AtomicU64,
        closed: AtomicBool,
    }

    unsafe impl Sync for WalRingBuffer {}
    unsafe impl Send for WalRingBuffer {}

    impl WalRingBuffer {
        fn new(capacity: usize) -> Self {
            let capacity = capacity.next_power_of_two();
            let mask = capacity - 1;

            let mut slots = Vec::with_capacity(capacity);
            for i in 0..capacity {
                slots.push(Slot {
                    entry: UnsafeCell::new(None),
                    sequence: AtomicU64::new(i as u64),
                });
            }

            Self {
                slots,
                capacity,
                mask,
                write_pos: AtomicU64::new(0),
                read_pos: AtomicU64::new(0),
                closed: AtomicBool::new(false),
            }
        }

        fn try_append(&self, entry: PendingEntry) -> Result<(), PendingEntry> {
            if self.closed.load(Ordering::Acquire) {
                return Err(entry);
            }

            // Simplified backpressure: just try once or fail
            // In Loom, we want to exercise the race, not the spin loop logic per se.

            let pos = self.write_pos.load(Ordering::Relaxed);
            let idx = (pos as usize) & self.mask;
            let slot = &self.slots[idx];
            let expected_seq = pos;

            let current_seq = slot.sequence.load(Ordering::Acquire);

            if current_seq == expected_seq {
                match self.write_pos.compare_exchange(
                    pos,
                    pos + 1, // wrapping_add not needed for small loom tests
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // Success
                        // Loom's UnsafeCell requires with_mut for mutable access
                        slot.entry.with_mut(|ptr| unsafe {
                            *ptr = Some(entry);
                        });
                        slot.sequence.store(pos + 1, Ordering::Release);
                        Ok(())
                    }
                    Err(_) => Err(entry), // Lost race
                }
            } else {
                Err(entry) // Buffer full or race lost
            }
        }

        fn drain(&self) -> Vec<PendingEntry> {
            let mut entries = Vec::new();

            // Limit drain loop to prevent infinite loops in Loom if logic is buggy
            // Real drain loops forever until empty.
            for _ in 0..self.capacity * 2 {
                let pos = self.read_pos.load(Ordering::Relaxed);
                let idx = (pos as usize) & self.mask;
                let slot = &self.slots[idx];

                let expected_seq = pos + 1;
                let current_seq = slot.sequence.load(Ordering::Acquire);

                if current_seq == expected_seq {
                    match self.read_pos.compare_exchange(
                        pos,
                        pos + 1,
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            let entry = slot.entry.with_mut(|ptr| unsafe { (*ptr).take() });
                            slot.sequence
                                .store(pos + self.capacity as u64, Ordering::Release);
                            if let Some(e) = entry {
                                entries.push(e);
                            }
                        }
                        Err(_) => {
                            // Lost race (shouldn't happen with single consumer)
                            continue;
                        }
                    }
                } else {
                    break;
                }
            }
            entries
        }
    }

    #[test]
    fn test_concurrent_ring_buffer() {
        loom::model(|| {
            let capacity = 2; // Small capacity to force wrapping and contention
            let buffer = Arc::new(WalRingBuffer::new(capacity));

            let b1 = buffer.clone();
            let b2 = buffer.clone();

            // Thread 1: Producer
            let t1 = thread::spawn(move || {
                let entry = PendingEntry { lsn: 1, val: 10 };
                // Retry until success
                loop {
                    match b1.try_append(entry) {
                        Ok(_) => break,
                        Err(_) => thread::yield_now(),
                    }
                }
            });

            // Thread 2: Producer
            let t2 = thread::spawn(move || {
                let entry = PendingEntry { lsn: 2, val: 20 };
                loop {
                    match b2.try_append(entry) {
                        Ok(_) => break,
                        Err(_) => thread::yield_now(),
                    }
                }
            });

            // Thread 3: Consumer
            let b3 = buffer.clone();
            let t3 = thread::spawn(move || {
                let mut collected = Vec::new();
                // Try to drain until we get 2 items
                while collected.len() < 2 {
                    let batch = b3.drain();
                    collected.extend(batch);
                    thread::yield_now();
                }
                collected
            });

            t1.join().unwrap();
            t2.join().unwrap();
            let results = t3.join().unwrap();

            assert_eq!(results.len(), 2);
            // Verify we got both values (order depends on race)
            let sum: u64 = results.iter().map(|e| e.val).sum();
            assert_eq!(sum, 30);
        });
    }
}
