//! Loom verification for WalRingBuffer.
//!
//! # HAVOC: CONCURRENCY TORTURE
//! This test uses `loom` to verify the synchronization logic of the ring buffer
//! under all possible thread interleavings.
//!
//! NOTE: This file duplicates the `WalRingBuffer` logic from `src/storage/wal/ring_buffer.rs`
//! adapted for `loom` primitives. This is necessary because `loom` requires special
//! atomic types and `UnsafeCell` wrappers that are not compatible with standard `std::sync`.
//! Ideally, the production code would use a trait or macro to support both `std` and `loom`,
//! but for now this "Model Test" ensures the algorithm itself is correct.

use loom::cell::UnsafeCell;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use loom::thread;

// Simplified PendingEntry - just holds some data
#[derive(Debug, Clone, Copy, PartialEq)]
struct PendingEntry {
    producer_id: usize,
    seq: usize,
}

struct Slot {
    entry: UnsafeCell<Option<PendingEntry>>,
    sequence: AtomicU64,
}

// SAFETY: Same as original
unsafe impl Send for Slot {}
unsafe impl Sync for Slot {}

struct WalRingBuffer {
    slots: Vec<Slot>,
    capacity: usize,
    mask: usize,
    write_pos: AtomicU64,
    read_pos: AtomicU64,
    closed: AtomicBool,
    // Backpressure config is fixed for test
    initial_spins: u32,
    max_spins: u32,
}

// SAFETY: Same as original
unsafe impl Send for WalRingBuffer {}
unsafe impl Sync for WalRingBuffer {}

impl WalRingBuffer {
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
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
            initial_spins: 1, // Minimize spinning for loom
            max_spins: 2,
        }
    }

    fn try_append(&self, entry: PendingEntry) -> Result<(), PendingEntry> {
        if self.closed.load(Ordering::Acquire) {
            return Err(entry);
        }

        let mut spin_count = 0;
        let mut current_spin_limit = self.initial_spins;

        // Loom loop limit to prevent infinite loops in model checking
        // In real code this is `loop`, but for loom we need to break if stuck
        // or just let loom handle context switches.
        for _ in 0..3 {
            // Reduced for performance
            let pos = self.write_pos.load(Ordering::Relaxed);
            let idx = (pos as usize) & self.mask;
            let slot = &self.slots[idx];

            let expected_seq = pos;
            let current_seq = slot.sequence.load(Ordering::Acquire);

            if current_seq == expected_seq {
                match self.write_pos.compare_exchange(
                    pos,
                    pos.wrapping_add(1),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: We have exclusive access to this slot after successful CAS.
                        slot.entry.with_mut(|ptr| unsafe { *ptr = Some(entry) });

                        slot.sequence
                            .store(expected_seq.wrapping_add(1), Ordering::Release);
                        return Ok(());
                    }
                    Err(_) => {
                        continue;
                    }
                }
            } else {
                let distance_behind = expected_seq.wrapping_sub(current_seq);
                if distance_behind > 0 && distance_behind <= self.capacity as u64 {
                    spin_count += 1;
                    if spin_count >= current_spin_limit {
                        if current_spin_limit >= self.max_spins {
                            return Err(entry);
                        }
                        current_spin_limit *= 2;
                        spin_count = 0;
                        thread::yield_now(); // Loom yield
                    }
                } else {
                    continue;
                }
            }
        }
        Err(entry) // Failed to append after some tries
    }

    fn drain(&self) -> Vec<PendingEntry> {
        let mut entries = Vec::new();
        // Limit drain loop for loom too
        for _ in 0..3 {
            // Reduced for performance
            let pos = self.read_pos.load(Ordering::Relaxed);
            let idx = (pos as usize) & self.mask;
            let slot = &self.slots[idx];

            let expected_seq = pos.wrapping_add(1);
            let current_seq = slot.sequence.load(Ordering::Acquire);

            if current_seq == expected_seq {
                match self.read_pos.compare_exchange(
                    pos,
                    pos.wrapping_add(1),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        let entry = slot.entry.with_mut(|ptr| unsafe { (*ptr).take() });

                        slot.sequence
                            .store(pos.wrapping_add(self.capacity as u64), Ordering::Release);

                        if let Some(e) = entry {
                            entries.push(e);
                        }
                    }
                    Err(_) => {
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
fn test_ring_buffer_concurrency() {
    let mut builder = loom::model::Builder::new();
    // Loom default preemption bound is sufficient with reduced loop counts

    builder.check(|| {
        let capacity = 2; // Smallest capacity
        let buffer = Arc::new(WalRingBuffer::new(capacity));

        let num_producers = 2;
        let items_per_producer = 1; // Keep very small for loom

        let mut producers = Vec::new();

        for p in 0..num_producers {
            let b = buffer.clone();
            producers.push(thread::spawn(move || {
                for i in 0..items_per_producer {
                    let entry = PendingEntry {
                        producer_id: p,
                        seq: i,
                    };
                    // Retry loop
                    let mut current_entry = entry;
                    loop {
                        match b.try_append(current_entry) {
                            Ok(_) => break,
                            Err(e) => {
                                current_entry = e;
                                thread::yield_now();
                            }
                        }
                    }
                }
            }));
        }

        let b = buffer.clone();
        let consumer = thread::spawn(move || {
            let mut received = Vec::new();
            let total_items = num_producers * items_per_producer;
            // Limit iterations to avoid infinite loop in loom
            for _ in 0..10 {
                if received.len() >= total_items {
                    break;
                }
                let batch = b.drain();
                received.extend(batch);
                thread::yield_now();
            }
            received
        });

        for p in producers {
            p.join().unwrap();
        }

        let received = consumer.join().unwrap();

        // Verify consistency
        // Each producer's items should be in order
        for p in 0..num_producers {
            let p_items: Vec<_> = received
                .iter()
                .filter(|e| e.producer_id == p)
                .map(|e| e.seq)
                .collect();

            let expected: Vec<_> = (0..items_per_producer).collect();
            assert_eq!(p_items, expected, "Producer {} items out of order", p);
        }
    });
}
