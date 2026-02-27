use aletheiadb::core::interning::StringInterner;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn test_havoc_interner_holes() {
    let interner = Arc::new(StringInterner::new());
    let duration = Duration::from_secs(5);
    let start = Instant::now();

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut handles = vec![];

    // Spawn 8 writers to create high contention
    for t in 0..8 {
        let interner_clone = interner.clone();
        let stop_clone = stop.clone();
        handles.push(thread::spawn(move || {
            let mut i = 0;
            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                // Intern a non-empty string
                let s = format!("writer_{}_{}", t, i);
                let _ = interner_clone.intern(s);
                i += 1;

                // Small yield to encourage race conditions
                if i % 100 == 0 {
                    thread::yield_now();
                }
            }
        }));
    }

    // Spawn a reader that checks for holes
    let interner_clone = interner.clone();
    let stop_clone = stop.clone();
    let reader = thread::spawn(move || {
        let mut snapshots_taken = 0;
        let mut failures = 0;
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            // Get all strings (snapshot)
            match interner_clone.get_all_strings() {
                Ok(snapshot) => {
                    snapshots_taken += 1;
                    // Check for empty strings (holes)
                    for (id, s) in snapshot.iter().enumerate() {
                        if s.is_empty() {
                            panic!("HOLE FOUND at ID {}! The snapshot returned Ok but contains an empty string.", id);
                        }
                    }
                }
                Err(e) => {
                    // It's acceptable to fail under extreme contention if we timeout,
                    // but we must not return corrupted data (Ok with holes).
                    failures += 1;
                    eprintln!("Snapshot failed (expected under high load): {}", e);
                }
            }
        }
        println!("Reader finished: {} snapshots taken, {} failures", snapshots_taken, failures);
    });
    handles.push(reader);

    // Run for the duration
    while start.elapsed() < duration {
        thread::sleep(Duration::from_millis(100));
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);

    for h in handles {
        h.join().unwrap();
    }
}
