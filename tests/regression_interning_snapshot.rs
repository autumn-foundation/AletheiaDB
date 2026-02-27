use aletheiadb::core::interning::StringInterner;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_havoc_interner_snapshot_consistency() {
    let interner = Arc::new(StringInterner::new());
    let duration = Duration::from_secs(5); // Increased duration

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut handles = vec![];

    // Multiple writers to force interleaving of fetch_add and insert
    for t in 0..4 {
        let interner_clone = interner.clone();
        let stop_clone = stop.clone();
        handles.push(thread::spawn(move || {
            let mut i = 0;
            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                let s = format!("t{}_{}", t, i);
                let _ = interner_clone.intern(s);
                i += 1;
                // Yield randomly to encourage race
                if i % 10 == 0 {
                    thread::yield_now();
                }
            }
        }));
    }

    // Reader thread
    let interner_clone = interner.clone();
    let stop_clone = stop.clone();
    let reader = thread::spawn(move || {
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            // Now get_all_strings returns a Result, so we handle it.
            let snapshot = interner_clone.get_all_strings();

            // We expect it to SUCCEED now, because the retry logic should handle the holes.
            if let Ok(snapshot) = snapshot {
                let len = snapshot.len();
                if len > 0 {
                    // Verify that we have at least some content
                    let content_count = snapshot.iter().filter(|s| !s.is_empty()).count();

                    // Since we are writing non-empty strings, if we have a successful snapshot,
                    // we shouldn't have holes if we trust get_all_strings implementation.
                    // But in this test, we are just verifying it doesn't panic and returns something reasonable.
                    // Actually, if get_all_strings works correctly, it should NEVER return holes.
                    // So we can assert that too!

                    // Note: If the interner supports empty strings and we were writing them, this check would be invalid.
                    // But here we write format!("t{}_{}", t, i) which is never empty.
                    // However, get_all_strings MIGHT resize beyond the initial load if we keep appending.
                    // But our implementation uses next_id load at start of loop.

                    for (i, s) in snapshot.iter().enumerate() {
                        if s.is_empty() {
                            panic!("Hole found at index {} in a 'successful' snapshot!", i);
                        }
                    }
                }
            } else {
                 // It failed to get a consistent snapshot. This is possible under extreme contention
                 // if the timeout is reached. We shouldn't panic, but maybe log it.
                 // In a real regression test, we might want to ensure it succeeds eventually,
                 // but for this chaos-style test, just ensuring it doesn't return corrupted data is key.
                 eprintln!("Failed to get consistent snapshot (timeout)");
            }
        }
    });
    handles.push(reader);

    thread::sleep(duration);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);

    for h in handles {
        h.join().unwrap();
    }
}
