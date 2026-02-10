//! Loom model checks for vector index concurrency invariants.
//!
//! Initial focus: prevent "phantom vectors" where an inner index key exists
//! without a corresponding NodeId mapping after add/remove races.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use loom::thread;

#[test]
fn loom_vector_add_remove_race_no_phantom_inner_key() {
    loom::model(|| {
        // Mapping state: 0 = none, 1 = key present for our NodeId.
        let id_mapping = Arc::new(AtomicU64::new(0));
        // Whether inner ANN index contains the key.
        let inner_key_present = Arc::new(AtomicBool::new(false));

        let add_mapping = Arc::clone(&id_mapping);
        let add_inner = Arc::clone(&inner_key_present);
        let add_thread = thread::spawn(move || {
            // Fixed-order add protocol (mirrors intended race-safe approach):
            // 1. Add to inner index first.
            add_inner.store(true, Ordering::Release);
            // 2. Try to claim mapping.
            let inserted = add_mapping
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            // 3. If mapping claim lost, rollback inner insertion.
            if !inserted {
                add_inner.store(false, Ordering::Release);
            }
        });

        let rm_mapping = Arc::clone(&id_mapping);
        let rm_inner = Arc::clone(&inner_key_present);
        let remove_thread = thread::spawn(move || {
            // Remove protocol: detach mapping first, then remove inner key if mapped.
            let removed = rm_mapping.swap(0, Ordering::AcqRel);
            if removed == 1 {
                rm_inner.store(false, Ordering::Release);
            }
        });

        add_thread.join().expect("add thread should not panic");
        remove_thread
            .join()
            .expect("remove thread should not panic");

        let mapping = id_mapping.load(Ordering::Acquire);
        let inner = inner_key_present.load(Ordering::Acquire);

        // Core invariant for phantom-vector prevention:
        // if no NodeId->key mapping exists, the key cannot remain in inner index.
        if mapping == 0 {
            assert!(
                !inner,
                "phantom vector detected: inner key exists without mapping"
            );
        }
    });
}

#[test]
fn loom_vector_double_add_same_node_winner_loser_no_zombie_or_phantom() {
    loom::model(|| {
        // Shared model state for a single NodeId.
        // mapping: 0 = absent, 1 = present
        let mapping = Arc::new(AtomicU64::new(0));
        // Inner index key count for this NodeId across concurrent attempts.
        // Two concurrent adds can temporarily produce count=2 before loser rollback.
        let inner_count = Arc::new(AtomicU64::new(0));

        // Thread A add attempt
        let m_a = Arc::clone(&mapping);
        let c_a = Arc::clone(&inner_count);
        let t_a = thread::spawn(move || {
            // Add to inner first.
            c_a.fetch_add(1, Ordering::AcqRel);
            // Try to claim mapping.
            let won = m_a
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            // Loser rolls back inner insertion.
            if !won {
                c_a.fetch_sub(1, Ordering::AcqRel);
            }
        });

        // Thread B add attempt
        let m_b = Arc::clone(&mapping);
        let c_b = Arc::clone(&inner_count);
        let t_b = thread::spawn(move || {
            // Add to inner first.
            c_b.fetch_add(1, Ordering::AcqRel);
            // Try to claim mapping.
            let won = m_b
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            // Loser rolls back inner insertion.
            if !won {
                c_b.fetch_sub(1, Ordering::AcqRel);
            }
        });

        t_a.join().expect("add thread A should not panic");
        t_b.join().expect("add thread B should not panic");

        let mapped = mapping.load(Ordering::Acquire);
        let inner = inner_count.load(Ordering::Acquire);

        // Impossible states:
        // 1) mapping absent while inner count > 0 => phantom vector
        // 2) mapping present while inner count == 0 => zombie mapping
        assert!(
            !((mapped == 0 && inner > 0) || (mapped == 1 && inner == 0)),
            "inconsistent state: mapped={}, inner_count={}",
            mapped,
            inner
        );

        // Count is bounded by number of concurrent attempts in this model.
        assert!(inner <= 2);
    });
}

#[test]
fn loom_vector_remove_then_readd_same_node_keeps_mapping_inner_consistent() {
    loom::model(|| {
        // Start with an existing mapping/key.
        let mapping = Arc::new(AtomicU64::new(1));
        let inner_count = Arc::new(AtomicU64::new(1));

        let rm_m = Arc::clone(&mapping);
        let rm_i = Arc::clone(&inner_count);
        let remover = thread::spawn(move || {
            let removed = rm_m.swap(0, Ordering::AcqRel);
            if removed == 1 {
                rm_i.fetch_sub(1, Ordering::AcqRel);
            }
        });

        let add_m = Arc::clone(&mapping);
        let add_i = Arc::clone(&inner_count);
        let readder = thread::spawn(move || {
            add_i.fetch_add(1, Ordering::AcqRel);
            let won = add_m
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            if !won {
                add_i.fetch_sub(1, Ordering::AcqRel);
            }
        });

        remover.join().expect("remover should not panic");
        readder.join().expect("re-adder should not panic");

        let mapped = mapping.load(Ordering::Acquire);
        let inner = inner_count.load(Ordering::Acquire);

        assert!(
            !((mapped == 0 && inner > 0) || (mapped == 1 && inner == 0)),
            "inconsistent state after remove/readd race: mapped={}, inner_count={}",
            mapped,
            inner
        );
        assert!(inner <= 2);
    });
}

#[test]
fn loom_vector_distinct_nodes_get_distinct_inner_keys() {
    loom::model(|| {
        let next_key = Arc::new(AtomicU64::new(1));
        let key_a = Arc::new(AtomicU64::new(0));
        let key_b = Arc::new(AtomicU64::new(0));

        let ctr_a = Arc::clone(&next_key);
        let out_a = Arc::clone(&key_a);
        let t_a = thread::spawn(move || {
            out_a.store(ctr_a.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
        });

        let ctr_b = Arc::clone(&next_key);
        let out_b = Arc::clone(&key_b);
        let t_b = thread::spawn(move || {
            out_b.store(ctr_b.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
        });

        t_a.join().expect("thread A should not panic");
        t_b.join().expect("thread B should not panic");

        let a = key_a.load(Ordering::Relaxed);
        let b = key_b.load(Ordering::Relaxed);
        assert_ne!(a, b, "distinct node adds must allocate distinct inner keys");
        assert!((a == 1 || a == 2) && (b == 1 || b == 2));
    });
}

#[test]
fn loom_vector_double_remove_same_node_decrements_inner_at_most_once() {
    loom::model(|| {
        // Start with exactly one mapped vector.
        let mapping = Arc::new(AtomicU64::new(1));
        let inner_count = Arc::new(AtomicU64::new(1));

        let m1 = Arc::clone(&mapping);
        let i1 = Arc::clone(&inner_count);
        let t1 = thread::spawn(move || {
            let removed = m1.swap(0, Ordering::AcqRel);
            if removed == 1 {
                i1.fetch_sub(1, Ordering::AcqRel);
            }
        });

        let m2 = Arc::clone(&mapping);
        let i2 = Arc::clone(&inner_count);
        let t2 = thread::spawn(move || {
            let removed = m2.swap(0, Ordering::AcqRel);
            if removed == 1 {
                i2.fetch_sub(1, Ordering::AcqRel);
            }
        });

        t1.join().expect("remove thread 1 should not panic");
        t2.join().expect("remove thread 2 should not panic");

        let mapped = mapping.load(Ordering::Acquire);
        let inner = inner_count.load(Ordering::Acquire);
        assert_eq!(mapped, 0);
        assert_eq!(inner, 0, "inner count must be decremented exactly once");
    });
}
