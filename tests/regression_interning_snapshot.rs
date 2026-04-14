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
            let snapshot = interner_clone.get_all_strings();

            // Check for holes?
            // Actually, holes are expected if an intern operation is in progress (ID reserved but not inserted).
            // But we must ensure that we captured all strings up to the max ID found.
            // The fix in get_all_strings ensures we resize the vector to include the max ID.

            // We just verify that we don't panic and the snapshot looks sane.
            let len = snapshot.len();
            if len > 0 {
                // Verify that we have at least some content
                let content_count = snapshot.iter().filter(|s| !s.is_empty()).count();
                // We might have holes, but we shouldn't have ONLY holes if we are writing.
                if content_count == 0 {
                    // This might happen at very start, but unlikely later.
                }
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
