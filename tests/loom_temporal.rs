//! Loom model checks for temporal hook/observer ordering invariants.
//!
//! Goal: model the VS-047 hybrid pattern where pre-anchor hook data is attached
//! before commit publication, and observers consume only after commit visibility.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use loom::thread;

#[test]
fn loom_temporal_pre_anchor_snapshot_visible_before_observer_consumes_commit() {
    loom::model(|| {
        // Modeled anchor payload written by pre-anchor hook path.
        // 0 = no snapshot id, >0 = snapshot id set.
        let snapshot_id = Arc::new(AtomicU64::new(0));
        // Commit publication flag observed by post-commit observers.
        let committed = Arc::new(AtomicBool::new(false));
        // Observer read-out (0 means observer didn't run/see commit in this interleaving).
        let observer_seen_snapshot = Arc::new(AtomicU64::new(0));
        let observer_consumed_commit = Arc::new(AtomicBool::new(false));

        let w_snapshot = Arc::clone(&snapshot_id);
        let w_committed = Arc::clone(&committed);
        let writer = thread::spawn(move || {
            // 1) Pre-anchor hook computes snapshot id and stores it in version data.
            w_snapshot.store(123, Ordering::Relaxed);
            // 2) Storage/commit publishes event with Release ordering.
            w_committed.store(true, Ordering::Release);
        });

        let r_snapshot = Arc::clone(&snapshot_id);
        let r_committed = Arc::clone(&committed);
        let r_seen = Arc::clone(&observer_seen_snapshot);
        let r_consumed = Arc::clone(&observer_consumed_commit);
        let observer = thread::spawn(move || {
            // Observer path: only acts when commit is visible (Acquire).
            if r_committed.load(Ordering::Acquire) {
                r_consumed.store(true, Ordering::Relaxed);
                let sid = r_snapshot.load(Ordering::Relaxed);
                r_seen.store(sid, Ordering::Relaxed);
            }
        });

        writer.join().expect("writer should not panic");
        observer.join().expect("observer should not panic");

        let consumed = observer_consumed_commit.load(Ordering::Relaxed);
        let seen_sid = observer_seen_snapshot.load(Ordering::Relaxed);

        // Core invariant: if observer consumed a committed anchor event, snapshot id
        // must already be visible and initialized (non-zero).
        if consumed {
            assert_eq!(seen_sid, 123);
        }
    });
}

#[test]
fn loom_temporal_hook_failure_allows_commit_and_observer_processing() {
    loom::model(|| {
        let snapshot_id = Arc::new(AtomicU64::new(0));
        let hook_failed = Arc::new(AtomicBool::new(false));
        let committed = Arc::new(AtomicBool::new(false));
        let observer_processed = Arc::new(AtomicBool::new(false));
        let observer_seen_snapshot = Arc::new(AtomicU64::new(0));

        // Controls whether hook fails in this execution (race-selected).
        let fail_switch = Arc::new(AtomicBool::new(false));
        let switch = Arc::clone(&fail_switch);
        let controller = thread::spawn(move || {
            // Either ordering is valid under Loom interleavings.
            switch.store(true, Ordering::Relaxed);
        });

        let w_snapshot = Arc::clone(&snapshot_id);
        let w_failed = Arc::clone(&hook_failed);
        let w_committed = Arc::clone(&committed);
        let w_switch = Arc::clone(&fail_switch);
        let writer = thread::spawn(move || {
            // Pre-anchor hook phase.
            if w_switch.load(Ordering::Relaxed) {
                // Graceful degradation path.
                w_failed.store(true, Ordering::Relaxed);
            } else {
                // Snapshot created successfully.
                w_snapshot.store(456, Ordering::Relaxed);
            }
            // Anchor commit always proceeds.
            w_committed.store(true, Ordering::Release);
        });

        let r_committed = Arc::clone(&committed);
        let r_processed = Arc::clone(&observer_processed);
        let r_seen = Arc::clone(&observer_seen_snapshot);
        let r_snapshot = Arc::clone(&snapshot_id);
        let observer = thread::spawn(move || {
            if r_committed.load(Ordering::Acquire) {
                r_processed.store(true, Ordering::Relaxed);
                r_seen.store(r_snapshot.load(Ordering::Relaxed), Ordering::Relaxed);
            }
        });

        controller.join().expect("controller should not panic");
        writer.join().expect("writer should not panic");
        observer.join().expect("observer should not panic");

        // Commit must happen even on hook failure.
        assert!(committed.load(Ordering::Acquire));

        if observer_processed.load(Ordering::Relaxed) {
            let seen = observer_seen_snapshot.load(Ordering::Relaxed);
            if hook_failed.load(Ordering::Relaxed) {
                assert_eq!(seen, 0);
            } else {
                assert_eq!(seen, 456);
            }
        }
    });
}

#[test]
fn loom_temporal_observer_error_does_not_block_other_observers() {
    loom::model(|| {
        let committed = Arc::new(AtomicBool::new(false));
        let observer1_failed = Arc::new(AtomicBool::new(false));
        let observer2_processed = Arc::new(AtomicBool::new(false));

        let w_committed = Arc::clone(&committed);
        let writer = thread::spawn(move || {
            w_committed.store(true, Ordering::Release);
        });

        let d_committed = Arc::clone(&committed);
        let d_fail = Arc::clone(&observer1_failed);
        let d_processed = Arc::clone(&observer2_processed);
        let dispatcher = thread::spawn(move || {
            if d_committed.load(Ordering::Acquire) {
                // Observer 1 errors.
                d_fail.store(true, Ordering::Relaxed);
                // Correct behavior: observer 2 still runs.
                d_processed.store(true, Ordering::Relaxed);
            }
        });

        writer.join().expect("writer should not panic");
        dispatcher.join().expect("dispatcher should not panic");

        if committed.load(Ordering::Acquire) && observer1_failed.load(Ordering::Relaxed) {
            assert!(
                observer2_processed.load(Ordering::Relaxed),
                "observer failure must not suppress other observers"
            );
        }
    });
}

#[test]
fn loom_temporal_snapshot_rotation_and_prune_never_drop_current_snapshot() {
    loom::model(|| {
        // Snapshot liveness for two generations.
        let snap1_alive = Arc::new(AtomicBool::new(true));
        let snap2_alive = Arc::new(AtomicBool::new(false));
        // Current anchor-linked snapshot id (1 initially).
        let current_snapshot = Arc::new(AtomicU64::new(1));

        let r_snap2 = Arc::clone(&snap2_alive);
        let r_current = Arc::clone(&current_snapshot);
        let rotator = thread::spawn(move || {
            // Rotation creates new snapshot first...
            r_snap2.store(true, Ordering::Relaxed);
            // ...then publishes current snapshot switch.
            r_current.store(2, Ordering::Release);
        });

        let p_snap1 = Arc::clone(&snap1_alive);
        let p_current = Arc::clone(&current_snapshot);
        let pruner = thread::spawn(move || {
            // Pruner uses Acquire when reading current generation.
            let observed = p_current.load(Ordering::Acquire);
            // Safe prune protocol: revalidate current generation before delete.
            if observed == 2 {
                // Candidate is snapshot 1 (strictly older generation), but only
                // if current is still 2 at prune commit point.
                if p_current
                    .compare_exchange(2, 2, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    p_snap1.store(false, Ordering::Relaxed);
                }
            } else {
                // observed == 1: nothing older to prune in this 2-generation model.
                // Critically, we never prune the newest candidate generation.
            }
        });

        rotator.join().expect("rotator should not panic");
        pruner.join().expect("pruner should not panic");

        let cur = current_snapshot.load(Ordering::Acquire);
        let cur_alive = if cur == 1 {
            snap1_alive.load(Ordering::Relaxed)
        } else {
            snap2_alive.load(Ordering::Relaxed)
        };
        assert!(
            cur_alive,
            "retention/pruning race removed current snapshot generation"
        );
    });
}
