#![allow(unexpected_cfgs)]

mod loom_tests {
    //! # Completion Notifier Model
    //!
    //! This test file implements a **Loom Model** of the `CompletionNotifier` found in
    //! `src/storage/wal/ring_buffer.rs`.
    //!
    //! ## Why a Model?
    //!
    //! The production `CompletionNotifier` uses `std::sync` primitives which are
    //! not instrumented by Loom. To verify the deadlock fix (Lost Wakeup), we
    //! model the exact logic using `loom::sync` primitives.
    //!
    //! This model verified that acquiring the mutex in `notify_success` prevents
    //! the lost wakeup bug.

    use loom::sync::atomic::{AtomicU64, Ordering};
    use loom::sync::{Condvar, Mutex};
    use loom::thread;
    use std::sync::Arc;

    struct CompletionNotifier {
        state: AtomicU64,
        condvar: Condvar,
        wait_mutex: Mutex<()>,
    }

    impl CompletionNotifier {
        fn new() -> Self {
            Self {
                state: AtomicU64::new(0), // 0 = Pending, 1 = Complete
                condvar: Condvar::new(),
                wait_mutex: Mutex::new(()),
            }
        }

        fn notify_success(&self) {
            // FIX: Acquire mutex before updating state and notifying
            // This prevents the "lost wakeup" scenario where a waiter checks the state (Pending),
            // then the notifier updates state and signals (Lost), then the waiter sleeps forever.
            let _guard = self.wait_mutex.lock().unwrap();
            self.state.store(1, Ordering::Release);
            self.condvar.notify_all();
        }

        fn wait(&self) {
            let mut guard = self.wait_mutex.lock().unwrap();

            // Check if already complete
            if self.state.load(Ordering::Acquire) == 1 {
                return;
            }

            // Wait for notification
            // loom::sync::Condvar doesn't have wait_while, so we implement the loop manually
            while self.state.load(Ordering::Acquire) == 0 {
                guard = self.condvar.wait(guard).unwrap();
            }
        }
    }

    #[test]
    fn test_notify_race_condition() {
        loom::model(|| {
            let notifier = Arc::new(CompletionNotifier::new());
            let notifier_clone = notifier.clone();

            // Thread 1: Waiter
            let t1 = thread::spawn(move || {
                notifier_clone.wait();
            });

            // Thread 2: Notifier
            let t2 = thread::spawn(move || {
                notifier.notify_success();
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }
}
