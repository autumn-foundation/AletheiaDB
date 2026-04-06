#[test]
fn test_completion_notifier_deadlock() {
    loom::model(|| {
        use loom::sync::{Arc, Mutex, Condvar};
        use loom::sync::atomic::{AtomicU64, Ordering};
        use loom::thread;

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum CompletionState {
            Pending = 0,
            Complete = 1,
            Error = 2,
        }

        pub struct CompletionNotifier {
            state: AtomicU64,
            error: Mutex<Option<String>>,
            condvar: Condvar,
            wait_mutex: Mutex<()>,
        }

        impl CompletionNotifier {
            pub fn new() -> Self {
                Self {
                    state: AtomicU64::new(CompletionState::Pending as u64),
                    error: Mutex::new(None),
                    condvar: Condvar::new(),
                    wait_mutex: Mutex::new(()),
                }
            }

            pub fn notify_error(&self, error: &str) {
                let _guard = self.wait_mutex.lock().unwrap_or_else(|e| e.into_inner());
                {
                    let mut err = self.error.lock().unwrap_or_else(|e| e.into_inner());
                    *err = Some(error.to_string());
                }
                self.state.store(CompletionState::Error as u64, Ordering::Release);
                self.condvar.notify_all();
            }

            pub fn wait(&self) -> Result<(), String> {
                let guard = self.wait_mutex.lock().unwrap_or_else(|e| e.into_inner());

                let state = self.state.load(Ordering::Acquire);
                if state == CompletionState::Complete as u64 {
                    return Ok(());
                }
                if state == CompletionState::Error as u64 {
                    let err = self.error.lock().unwrap_or_else(|e| e.into_inner());
                    return Err(err.clone().unwrap_or_else(|| "Unknown error".to_string()));
                }

                let _guard = self
                    .condvar
                    .wait_while(guard, |_| {
                        self.state.load(Ordering::Acquire) == CompletionState::Pending as u64
                    })
                    .unwrap_or_else(|e| e.into_inner());

                let state = self.state.load(Ordering::Acquire);
                if state == CompletionState::Complete as u64 {
                    Ok(())
                } else {
                    let err = self.error.lock().unwrap_or_else(|e| e.into_inner());
                    Err(err.clone().unwrap_or_else(|| "Unknown error".to_string()))
                }
            }
        }

        let notifier = Arc::new(CompletionNotifier::new());
        let notifier_clone = Arc::clone(&notifier);

        let writer_thread = thread::spawn(move || {
            let _ = notifier_clone.wait();
        });

        // Simulate panic during notify_error
        let _result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = notifier.wait_mutex.lock().unwrap_or_else(|e| e.into_inner());
            {
                let mut err = notifier.error.lock().unwrap_or_else(|e| e.into_inner());
                *err = Some("Simulated error".to_string());
                panic!("Flush thread crashed!");
            }
        }));

        // Writer thread will deadlock waiting for condvar notification
        writer_thread.join().unwrap();
    });
}
