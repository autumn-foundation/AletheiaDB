use aletheiadb::core::interning::StringInterner;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_interner_snapshot_consistency() {
    let interner = Arc::new(StringInterner::new());
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));

    struct RunningGuard(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for RunningGuard {
        fn drop(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    // Writer thread: continuously interns unique strings
    let writer = {
        let interner = interner.clone();
        let running = running.clone();
        thread::spawn(move || {
            let mut i: u64 = 0;
            while running.load(std::sync::atomic::Ordering::Relaxed) {
                // Intern a non-empty string
                let s = format!("s-{}", i);
                let _ = interner.intern(&s);
                i = i.wrapping_add(1);
                #[allow(clippy::manual_is_multiple_of)]
                if i % 100 == 0 {
                    thread::yield_now();
                }
            }
        })
    };

    // Reader thread: continuously gets all strings and checks for consistency
    let reader = {
        let interner = interner.clone();
        let running = running.clone();
        thread::spawn(move || {
            // Ensure writer stops even if reader panics
            let _guard = RunningGuard(running);

            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(2) {
                let snapshot = interner.get_all_strings();

                // Check for holes (empty strings)
                // We assume we only intern "s-{}" which is never empty.
                // If get_all_strings returns "", it means it allocated space
                // for an ID but didn't find the string in the map yet.
                for (id, s) in snapshot.iter().enumerate() {
                    if s.is_empty() {
                        panic!(
                            "Consistency violation! ID {} exists in snapshot (len {}) but string is empty. \
                             Race condition detected: ID reserved but string not yet visible.",
                            id,
                            snapshot.len()
                        );
                    }
                }
                thread::yield_now();
            }
        })
    };

    let res = reader.join();
    writer.join().unwrap();

    // If reader panicked, the test failed (which is what we want for reproduction)
    if let Err(e) = res {
        std::panic::resume_unwind(e);
    }
}
