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
            let snapshot_result = interner_clone.get_all_strings();

            // It's possible to get an error if contention is extremely high (timeout).
            // That's acceptable behavior for this stress test - we just care that we don't
            // get a corrupted snapshot (Vec with holes) or panic.
            if let Ok(snapshot) = snapshot_result {
                let len = snapshot.len();
                if len > 0 {
                    // With the fix, we should NEVER have holes (empty strings) in the snapshot.
                    // The new implementation either returns a complete snapshot or an error.
                    let hole_count = snapshot.iter().filter(|s| s.is_empty()).count();
                    assert_eq!(
                        hole_count, 0,
                        "Snapshot contained holes (empty strings)! This indicates data corruption."
                    );
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
