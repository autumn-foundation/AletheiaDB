//! Loom model checks for WAL concurrency invariants.
//!
//! These tests intentionally model core synchronization patterns used by the WAL
//! rather than exercising production structs directly. The goal is to prove key
//! invariants under exhaustive interleavings in a small state space.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use loom::thread;

#[test]
fn loom_wal_lsn_allocator_unique_and_monotonic() {
    loom::model(|| {
        let next_lsn = Arc::new(AtomicU64::new(1));
        let l1 = Arc::new(AtomicU64::new(0));
        let l2 = Arc::new(AtomicU64::new(0));
        let l3 = Arc::new(AtomicU64::new(0));

        let mut handles = Vec::new();
        for slot in [&l1, &l2, &l3] {
            let ctr = Arc::clone(&next_lsn);
            let out = Arc::clone(slot);
            handles.push(thread::spawn(move || {
                // Mirrors WAL allocator behavior: fetch_add with Relaxed ordering.
                let lsn = ctr.fetch_add(1, Ordering::Relaxed);
                out.store(lsn, Ordering::Relaxed);
            }));
        }

        for h in handles {
            h.join().expect("loom thread should not panic");
        }

        let a = l1.load(Ordering::Relaxed);
        let b = l2.load(Ordering::Relaxed);
        let c = l3.load(Ordering::Relaxed);

        // Uniqueness (no duplicates).
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);

        // Monotonic range: three allocations from starting LSN 1 -> {1,2,3}.
        let mut lsns = [a, b, c];
        lsns.sort_unstable();
        assert_eq!(lsns, [1, 2, 3]);
    });
}

#[test]
fn loom_wal_slot_publish_release_acquire_visibility() {
    loom::model(|| {
        let payload = Arc::new(AtomicU64::new(0));
        let published = Arc::new(AtomicBool::new(false));

        let p_payload = Arc::clone(&payload);
        let p_published = Arc::clone(&published);
        let producer = thread::spawn(move || {
            // Producer writes entry bytes/metadata first...
            p_payload.store(42, Ordering::Relaxed);
            // ...then publishes slot readiness with Release.
            p_published.store(true, Ordering::Release);
        });

        let c_payload = Arc::clone(&payload);
        let c_published = Arc::clone(&published);
        let consumer = thread::spawn(move || {
            // Consumer only reads payload after Acquire sees published=true.
            if c_published.load(Ordering::Acquire) {
                let seen = c_payload.load(Ordering::Relaxed);
                assert_eq!(seen, 42, "published slot must expose initialized payload");
            }
        });

        producer.join().expect("producer should not panic");
        consumer.join().expect("consumer should not panic");
    });
}

#[test]
fn loom_wal_batch_lsn_ranges_are_disjoint_and_cover_space() {
    loom::model(|| {
        let next_lsn = Arc::new(AtomicU64::new(1));
        let a_start = Arc::new(AtomicU64::new(0));
        let b_start = Arc::new(AtomicU64::new(0));

        let c1 = Arc::clone(&next_lsn);
        let a1 = Arc::clone(&a_start);
        let t1 = thread::spawn(move || {
            // Simulate allocate_batch(2)
            let start = c1.fetch_add(2, Ordering::Relaxed);
            a1.store(start, Ordering::Relaxed);
        });

        let c2 = Arc::clone(&next_lsn);
        let b1 = Arc::clone(&b_start);
        let t2 = thread::spawn(move || {
            // Simulate allocate_batch(3)
            let start = c2.fetch_add(3, Ordering::Relaxed);
            b1.store(start, Ordering::Relaxed);
        });

        t1.join().expect("thread 1 should not panic");
        t2.join().expect("thread 2 should not panic");

        let s1 = a_start.load(Ordering::Relaxed);
        let s2 = b_start.load(Ordering::Relaxed);

        let r1 = [s1, s1 + 1];
        let r2 = [s2, s2 + 1, s2 + 2];

        // Disjoint ranges (no duplicate LSN assignment between batches).
        for x in r1 {
            for y in r2 {
                assert_ne!(x, y);
            }
        }

        // Combined allocated set must be exactly {1,2,3,4,5}.
        let mut all = vec![r1[0], r1[1], r2[0], r2[1], r2[2]];
        all.sort_unstable();
        assert_eq!(all, vec![1, 2, 3, 4, 5]);
    });
}

#[test]
fn loom_wal_mpsc_publish_then_drain_reads_initialized_slots_in_order() {
    loom::model(|| {
        // Two-slot model of the ring buffer publish/drain protocol.
        let slot0 = Arc::new(AtomicU64::new(0));
        let slot1 = Arc::new(AtomicU64::new(0));
        let ready0 = Arc::new(AtomicBool::new(false));
        let ready1 = Arc::new(AtomicBool::new(false));

        // Producer A publishes slot 0.
        let p0_slot = Arc::clone(&slot0);
        let p0_ready = Arc::clone(&ready0);
        let p0 = thread::spawn(move || {
            p0_slot.store(11, Ordering::Relaxed);
            p0_ready.store(true, Ordering::Release);
        });

        // Producer B publishes slot 1.
        let p1_slot = Arc::clone(&slot1);
        let p1_ready = Arc::clone(&ready1);
        let p1 = thread::spawn(move || {
            p1_slot.store(22, Ordering::Relaxed);
            p1_ready.store(true, Ordering::Release);
        });

        // Single consumer drains in slot order (0 then 1), reading only when ready.
        let c_slot0 = Arc::clone(&slot0);
        let c_slot1 = Arc::clone(&slot1);
        let c_ready0 = Arc::clone(&ready0);
        let c_ready1 = Arc::clone(&ready1);
        let consumer = thread::spawn(move || {
            let mut out = Vec::new();

            if c_ready0.load(Ordering::Acquire) {
                out.push(c_slot0.load(Ordering::Relaxed));
            }
            if c_ready1.load(Ordering::Acquire) {
                out.push(c_slot1.load(Ordering::Relaxed));
            }

            out
        });

        p0.join().expect("producer 0 should not panic");
        p1.join().expect("producer 1 should not panic");
        let drained = consumer.join().expect("consumer should not panic");

        // If a slot is drained, its value must be fully initialized.
        for v in &drained {
            assert!(*v == 11 || *v == 22);
        }

        // Consumer preserves slot-ordering for whatever subset was visible.
        // Valid outputs: [], [11], [22], [11, 22]
        assert!(
            drained.is_empty()
                || drained == vec![11]
                || drained == vec![22]
                || drained == vec![11, 22]
        );
    });
}

#[test]
fn loom_wal_append_vs_close_observed_close_never_publishes() {
    loom::model(|| {
        let closed = Arc::new(AtomicBool::new(false));
        let payload = Arc::new(AtomicU64::new(0));
        let ready = Arc::new(AtomicBool::new(false));

        // Track what the appender observed/decided.
        let observed_closed = Arc::new(AtomicBool::new(false));
        let append_succeeded = Arc::new(AtomicBool::new(false));

        let a_closed = Arc::clone(&closed);
        let a_payload = Arc::clone(&payload);
        let a_ready = Arc::clone(&ready);
        let a_observed_closed = Arc::clone(&observed_closed);
        let a_append_succeeded = Arc::clone(&append_succeeded);
        let appender = thread::spawn(move || {
            // Fast closed check (similar to try-append paths).
            if a_closed.load(Ordering::Acquire) {
                a_observed_closed.store(true, Ordering::Relaxed);
                return;
            }

            // Simulate successful publish path.
            a_payload.store(7, Ordering::Relaxed);
            a_ready.store(true, Ordering::Release);
            a_append_succeeded.store(true, Ordering::Relaxed);
        });

        let c_closed = Arc::clone(&closed);
        let closer = thread::spawn(move || {
            c_closed.store(true, Ordering::Release);
        });

        appender.join().expect("appender should not panic");
        closer.join().expect("closer should not panic");

        // Core invariant: if append observed closed=true, it must not publish.
        if observed_closed.load(Ordering::Relaxed) {
            assert!(!append_succeeded.load(Ordering::Relaxed));
            assert!(!ready.load(Ordering::Acquire));
        }

        // If anything was published, payload must be initialized.
        if ready.load(Ordering::Acquire) {
            assert_eq!(payload.load(Ordering::Relaxed), 7);
        }
    });
}

#[test]
fn loom_wal_completion_signal_never_stays_pending_after_notify_race() {
    loom::model(|| {
        // 0 = Pending, 1 = Complete, 2 = Error
        let state = Arc::new(AtomicU64::new(0));
        let error_set = Arc::new(AtomicBool::new(false));

        let s_ok = Arc::clone(&state);
        let notify_ok = thread::spawn(move || {
            s_ok.store(1, Ordering::Release);
        });

        let s_err = Arc::clone(&state);
        let e_err = Arc::clone(&error_set);
        let notify_err = thread::spawn(move || {
            e_err.store(true, Ordering::Relaxed);
            s_err.store(2, Ordering::Release);
        });

        notify_ok.join().expect("success notifier should not panic");
        notify_err.join().expect("error notifier should not panic");

        let final_state = state.load(Ordering::Acquire);
        assert!(final_state == 1 || final_state == 2);

        if final_state == 2 {
            assert!(error_set.load(Ordering::Relaxed));
        }
    });
}

#[test]
fn loom_wal_batch_allocation_and_single_allocation_do_not_overlap() {
    loom::model(|| {
        let next_lsn = Arc::new(AtomicU64::new(1));
        let batch_start = Arc::new(AtomicU64::new(0));
        let single = Arc::new(AtomicU64::new(0));

        let ctr_a = Arc::clone(&next_lsn);
        let out_a = Arc::clone(&batch_start);
        let t_batch = thread::spawn(move || {
            let start = ctr_a.fetch_add(4, Ordering::Relaxed);
            out_a.store(start, Ordering::Relaxed);
        });

        let ctr_b = Arc::clone(&next_lsn);
        let out_b = Arc::clone(&single);
        let t_single = thread::spawn(move || {
            let lsn = ctr_b.fetch_add(1, Ordering::Relaxed);
            out_b.store(lsn, Ordering::Relaxed);
        });

        t_batch.join().expect("batch thread should not panic");
        t_single.join().expect("single thread should not panic");

        let s = batch_start.load(Ordering::Relaxed);
        let r = [s, s + 1, s + 2, s + 3];
        let one = single.load(Ordering::Relaxed);

        for x in r {
            assert_ne!(x, one, "single allocation overlapped batch range");
        }
    });
}

#[test]
fn loom_wal_multi_stripe_drain_merge_preserves_global_lsn_order() {
    loom::model(|| {
        // Model two stripes each publishing one entry.
        let s0_lsn = Arc::new(AtomicU64::new(0));
        let s1_lsn = Arc::new(AtomicU64::new(0));
        let s0_ready = Arc::new(AtomicBool::new(false));
        let s1_ready = Arc::new(AtomicBool::new(false));

        let p0_lsn = Arc::clone(&s0_lsn);
        let p0_ready = Arc::clone(&s0_ready);
        let p0 = thread::spawn(move || {
            p0_lsn.store(2, Ordering::Relaxed);
            p0_ready.store(true, Ordering::Release);
        });

        let p1_lsn = Arc::clone(&s1_lsn);
        let p1_ready = Arc::clone(&s1_ready);
        let p1 = thread::spawn(move || {
            p1_lsn.store(1, Ordering::Relaxed);
            p1_ready.store(true, Ordering::Release);
        });

        let c0_lsn = Arc::clone(&s0_lsn);
        let c1_lsn = Arc::clone(&s1_lsn);
        let c0_ready = Arc::clone(&s0_ready);
        let c1_ready = Arc::clone(&s1_ready);
        let flusher = thread::spawn(move || {
            let mut drained = Vec::new();
            if c0_ready.load(Ordering::Acquire) {
                drained.push(c0_lsn.load(Ordering::Relaxed));
            }
            if c1_ready.load(Ordering::Acquire) {
                drained.push(c1_lsn.load(Ordering::Relaxed));
            }
            drained.sort_unstable();
            drained
        });

        p0.join().expect("producer 0 should not panic");
        p1.join().expect("producer 1 should not panic");
        let merged = flusher.join().expect("flusher should not panic");

        // Merge/sort must produce monotonic order.
        for pair in merged.windows(2) {
            assert!(pair[0] <= pair[1]);
        }
        // Any observed LSN must be one of the published values.
        for v in merged {
            assert!(v == 1 || v == 2);
        }
    });
}
