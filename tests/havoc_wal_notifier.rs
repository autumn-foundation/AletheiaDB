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
            // Acquire mutex before updating state and notifying
            // This prevents the "lost wakeup" scenario where a waiter checks the state (Pending),
            // then the notifier updates state and signals (Lost), then the waiter sleeps forever.
            let _guard = self.wait_mutex.lock().unwrap();
            self.state.store(1, Ordering::Release);
            self.condvar.notify_all();
        }

        fn notify_error(&self) {
            struct NotifyGuard<'a>(&'a Condvar);
            impl<'a> Drop for NotifyGuard<'a> {
                fn drop(&mut self) {
                    self.0.notify_all();
                }
            }
            let _notify_guard = NotifyGuard(&self.condvar);

            let _guard = self.wait_mutex.lock().unwrap_or_else(|e| e.into_inner());
            // Simulate the exact code from production ring_buffer.rs which could panic inside this block
            {
                // In production it acquires self.error.lock() and sets a string.
                // We'll simulate a panic occurring right here.
                panic!("Flush thread crashed during notify_error!");
            }
            // self.state.store(2, Ordering::Release);
            // self.condvar.notify_all();
        }

        fn wait(&self) -> Result<(), ()> {
            let mut guard = match self.wait_mutex.lock() { Ok(g) => g, Err(_e) => return Err(()) };

            if self.state.load(Ordering::Acquire) == 1 {
                return Ok(());
            }
            if self.state.load(Ordering::Acquire) == 2 {
                return Err(());
            }

            while self.state.load(Ordering::Acquire) == 0 {
                // In loom, condvar.wait().unwrap() will panic if poisoned, we can't unwrap_or_else easily on the lock directly
                // if it's the Loom mutex. But we don't need to test Loom's poison semantics,
                // we just want to ensure that it doesn't deadlock.
                // Wait, if loom Condvar::wait() returns PoisonError, we can match it:
                guard = match self.condvar.wait(guard) {
                    Ok(g) => g,
                    Err(_) => {
                        // In Loom, catching the poison error might still panic if we drop the guard, or we can just ignore it
                        // if we panic it's not a deadlock, but to avoid the panic, we just break.
                        // Actually Loom panics on `e.into_inner()` or inside Condvar wait.
                        // So let's just let it panic. A panic is not a deadlock.
                        return Err(());
                    }
                };
            }

            if self.state.load(Ordering::Acquire) == 1 {
                Ok(())
            } else {
                Err(())
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
                let _ = notifier_clone.wait();
            });

            // Thread 2: Notifier
            let t2 = thread::spawn(move || {
                notifier.notify_success();
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }

    #[test]
    fn test_notify_panic_deadlock() {
        loom::model(|| {
            let notifier = Arc::new(CompletionNotifier::new());
            let notifier_clone = notifier.clone();

            // Thread 1: Waiter
            let t1 = thread::spawn(move || {
                let _ = notifier_clone.wait();
            });

            // Thread 2: Notifier
            let t2 = thread::spawn(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    notifier.notify_error();
                }));
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }
}
