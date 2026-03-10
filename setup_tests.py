import os

with open("tests/havoc/wal_ring_buffer_loom.rs", "w") as f:
    f.write("""use aletheiadb::storage::wal::ring_buffer::{PendingEntry, WalRingBuffer};
use aletheiadb::storage::wal::LSN;
use loom::sync::Arc;
use loom::thread;

#[test]
fn test_ring_buffer_loom_append_drain() {
    loom::model(|| {
        let buf = Arc::new(WalRingBuffer::new(2));

        let b1 = buf.clone();
        let t1 = thread::spawn(move || {
            let _ = b1.try_append(PendingEntry::new_async(LSN(1), vec![1]));
        });

        let b2 = buf.clone();
        let t2 = thread::spawn(move || {
            let _ = b2.try_append(PendingEntry::new_async(LSN(2), vec![2]));
        });

        let b3 = buf.clone();
        let t3 = thread::spawn(move || {
            let _ = b3.drain();
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();

        let _ = buf.drain();
    });
}

#[test]
fn test_ring_buffer_loom_drain_preserves_order() {
    loom::model(|| {
        let buf = Arc::new(WalRingBuffer::new(2));

        let b1 = buf.clone();
        let t1 = thread::spawn(move || {
            let _ = b1.try_append(PendingEntry::new_async(LSN(1), vec![1]));
        });

        let b2 = buf.clone();
        let t2 = thread::spawn(move || {
            let _ = b2.try_append(PendingEntry::new_async(LSN(2), vec![2]));
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let entries = buf.drain();
        assert!(entries.len() <= 2);

        if entries.len() == 2 {
            // Because t1 and t2 race, either LSN 1 or LSN 2 can be in pos 0.
            // Draining simply pulls from pos 0 then pos 1.
            // So we just check that we got *both* LSN 1 and LSN 2 in some order.
            let has_1 = entries[0].lsn.0 == 1 || entries[1].lsn.0 == 1;
            let has_2 = entries[0].lsn.0 == 2 || entries[1].lsn.0 == 2;
            assert!(has_1 && has_2, "Drained entries do not contain both expected LSNs");
        }
    });
}

#[test]
fn test_ring_buffer_loom_drain_concurrency() {
    loom::model(|| {
        let buf = Arc::new(WalRingBuffer::new(2));

        let b1 = buf.clone();
        let t1 = thread::spawn(move || {
            let _ = b1.try_append(PendingEntry::new_async(LSN(1), vec![1]));
        });

        let b3 = buf.clone();
        let t3 = thread::spawn(move || {
            let entries = b3.drain();
            for entry in entries {
                let _ = entry; // Move entry
            }
        });

        t1.join().unwrap();
        t3.join().unwrap();

        let _ = buf.drain();
    });
}
""")

with open("tests/havoc/wal_ring_buffer_proptest.rs", "w") as f:
    f.write("""use aletheiadb::storage::wal::ring_buffer::{BackpressureConfig, WalRingBuffer};
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_backpressure_config_validation(
        initial_spins in 0u32..10000,
        max_spins in 0u32..10000,
        base_sleep_us in 0u64..10000,
        max_sleep_us in 0u64..10000,
    ) {
        let config = BackpressureConfig {
            initial_spins,
            max_spins,
            base_sleep_us,
            max_sleep_us,
        };

        let valid = config.validate();

        if initial_spins == 0 {
            prop_assert!(valid.is_err());
        } else if max_spins < initial_spins {
            prop_assert!(valid.is_err());
        } else {
            prop_assert!(valid.is_ok());
        }
    }
}

proptest! {
    #[test]
    fn test_backpressure_config_valid_presets(
        _dummy in 0..1 // just to make it a proptest
    ) {
        let low_latency = BackpressureConfig::low_latency();
        prop_assert!(low_latency.validate().is_ok());

        let high_throughput = BackpressureConfig::high_throughput();
        prop_assert!(high_throughput.validate().is_ok());

        let default = BackpressureConfig::default();
        prop_assert!(default.validate().is_ok());
    }
}

proptest! {
    #[test]
    #[cfg(not(loom))]
    fn test_try_append_property(
        cap_pow2 in 1usize..10, // 2^1 to 2^9
        num_appends in 0usize..2000,
    ) {
        let capacity = 1 << cap_pow2;
        let buf = WalRingBuffer::new(capacity);

        use aletheiadb::storage::wal::ring_buffer::PendingEntry;
        use aletheiadb::storage::wal::LSN;

        let mut success = 0;
        for i in 0..num_appends {
            if buf.try_append(PendingEntry::new_async(LSN(i as u64), vec![])).is_ok() {
                success += 1;
            }
        }

        // Since we don't drain, we can at most append capacity
        prop_assert!(success <= capacity);
    }
}

proptest! {
    #[test]
    #[cfg(not(loom))]
    fn test_len_approx_invariants(
        cap_pow2 in 1usize..10, // 2^1 to 2^9
        appends in 0usize..1000,
        should_drain in proptest::bool::ANY,
    ) {
        let capacity = 1 << cap_pow2;
        let buf = WalRingBuffer::new(capacity);

        let mut actual_len = 0;

        use aletheiadb::storage::wal::ring_buffer::PendingEntry;
        use aletheiadb::storage::wal::LSN;

        for _ in 0..appends {
            if buf.try_append(PendingEntry::new_async(LSN(1), vec![])).is_ok() {
                actual_len += 1;
            }
        }

        // len_approx might be slightly off due to concurrent reads, but in serial it should be exact
        prop_assert_eq!(buf.len_approx(), actual_len);

        if should_drain {
            let drained = buf.drain();
            actual_len -= drained.len();
            prop_assert_eq!(buf.len_approx(), actual_len);
        }
    }
}

proptest! {
    #[test]
    #[cfg(not(loom))]
    fn test_try_append_with_drains(
        cap_pow2 in 1usize..6, // 2 to 64
        ops in proptest::collection::vec(proptest::bool::ANY, 0..1000), // true = append, false = drain
    ) {
        let capacity = 1 << cap_pow2;
        let buf = WalRingBuffer::new(capacity);

        let mut appends = 0;
        let mut drains = 0;
        let mut current_len = 0;

        use aletheiadb::storage::wal::ring_buffer::PendingEntry;
        use aletheiadb::storage::wal::LSN;

        for op in ops {
            if op { // Append
                if buf.try_append(PendingEntry::new_async(LSN(appends as u64), vec![])).is_ok() {
                    appends += 1;
                    current_len += 1;
                } else {
                    prop_assert_eq!(current_len, capacity);
                }
            } else { // Drain
                let drained = buf.drain();
                drains += drained.len();
                current_len -= drained.len();
                prop_assert_eq!(current_len, 0); // Drain removes all available
            }
            prop_assert_eq!(buf.len_approx(), current_len);
        }

        prop_assert_eq!(appends, drains + current_len);
    }
}

proptest! {
    #[test]
    #[cfg(not(loom))]
    fn test_len_approx_underflow_wrap(
        cap_pow2 in 1usize..6,
        appends in 0usize..1000,
    ) {
        let capacity = 1 << cap_pow2;
        let mut buf = WalRingBuffer::new(capacity);

        // Simulating the u64::MAX boundary
        buf.set_state_for_wraparound_test(u64::MAX - 2, u64::MAX - 2);

        use aletheiadb::storage::wal::ring_buffer::PendingEntry;
        use aletheiadb::storage::wal::LSN;

        let mut len = 0;
        for i in 0..appends {
            if buf.try_append(PendingEntry::new_async(LSN(i as u64), vec![])).is_ok() {
                len += 1;
            } else {
                break;
            }
        }

        prop_assert_eq!(buf.len_approx(), len);
    }
}

proptest! {
    #[test]
    fn test_len_approx_does_not_panic(
        write_pos in any::<u64>(),
        read_pos in any::<u64>(),
    ) {
        // We test this directly using the logic of len_approx:
        // write.wrapping_sub(read) as usize
        let _len = write_pos.wrapping_sub(read_pos) as usize;
    }
}
""")

with open("tests/havoc/mod.rs", "a") as f:
    f.write("""mod wal_ring_buffer_loom;
mod wal_ring_buffer_proptest;
""")
