use loom::sync::{Arc, Mutex, Condvar};
use loom::sync::atomic::{AtomicU64, Ordering};
use loom::thread;

pub struct CompletionNotifier {
    state: AtomicU64,
    error: Mutex<Option<String>>,
    condvar: Condvar,
    wait_mutex: Mutex<()>,
}

impl CompletionNotifier {
    pub fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
            error: Mutex::new(None),
            condvar: Condvar::new(),
            wait_mutex: Mutex::new(()),
        }
    }

    pub fn notify_error(&self) {
        let _guard = self.wait_mutex.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut err = self.error.lock().unwrap_or_else(|e| e.into_inner());
            *err = Some("test".to_string());
        }
        self.state.store(2, Ordering::Release);
        self.condvar.notify_all();
    }

    pub fn wait(&self) -> Result<(), String> {
        let guard = self.wait_mutex.lock().unwrap_or_else(|e| e.into_inner());

        let _guard = self
            .condvar
            .wait_while(guard, |_| {
                self.state.load(Ordering::Acquire) == 0
            })
            .unwrap_or_else(|e| e.into_inner());

        Ok(())
    }
}

#[test]
fn test_completion_notifier_deadlock() {
    loom::model(|| {
        let notifier = Arc::new(CompletionNotifier::new());
        let notifier_clone = Arc::clone(&notifier);

        let writer_thread = thread::spawn(move || {
            let _ = notifier_clone.wait();
        });

        // Simulate panic during notify_error
        let _result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = notifier.wait_mutex.lock().unwrap_or_else(|e| e.into_inner());
            {
                let mut _err = notifier.error.lock().unwrap_or_else(|e| e.into_inner());
                panic!("Flush thread crashed!");
            }
        }));

        writer_thread.join().unwrap();
    });
}
