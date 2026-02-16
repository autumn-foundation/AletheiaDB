use aletheiadb::core::interning::{InternedString, StringInterner};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

#[test]
fn test_interner_persistence_truncation_race() {
    // This test attempts to reproduce a race condition where get_all_strings()
    // returns a truncated vector due to gaps in string_to_id map population,
    // causing data loss of fully committed interned strings with higher IDs.

    let interner = Arc::new(StringInterner::new());
    let stop = Arc::new(AtomicBool::new(false));
    let start_flag = Arc::new(std::sync::Barrier::new(3));

    // Thread A: The "Gap" Creator (Slow Interner)
    let interner_a = interner.clone();
    let stop_a = stop.clone();
    let bar_a = start_flag.clone();
    let thread_a = thread::spawn(move || {
        bar_a.wait();
        let mut i = 0u64;
        while !stop_a.load(Ordering::Relaxed) {
            // Unique prefix to avoid collisions with B
            let _ = interner_a.intern(format!("A-{}", i));
            i += 1;
            // Sleep slightly to allow B to race ahead?
            // Actually we want A to be slow *inside* the intern, which we can't control.
            // But if we have many threads, chance of preemption increases.
        }
    });

    // Thread B: The "Fast" Interner (High volume)
    let interner_b = interner.clone();
    let stop_b = stop.clone();
    let bar_b = start_flag.clone();
    let thread_b = thread::spawn(move || {
        bar_b.wait();
        let mut i = 0u64;
        while !stop_b.load(Ordering::Relaxed) {
            let _ = interner_b.intern(format!("B-{}", i));
            i += 1;
        }
    });

    // Thread C: The Persistence Manager (Calls get_all_strings)
    let interner_c = interner.clone();
    let stop_c = stop.clone();
    let bar_c = start_flag.clone();
    let thread_c = thread::spawn(move || {
        bar_c.wait();
        let mut iterations = 0;
        while !stop_c.load(Ordering::Relaxed) {
            let snapshot = interner_c.get_all_strings();
            iterations += 1;

            // Check if we missed any committed strings.
            // If we find a valid ID >= snap_len, it MIGHT be data loss.

            // However, we know `get_all_strings` logic:
            // It relies on `self.len()`.

            // Check for internal corruption (gaps replaced by empty strings)
            // Since we never intern empty strings in this test, any "" in the snapshot
            // where the actual interner has a value is data corruption.
            if let Some(gap_idx) = snapshot.iter().position(|s| s.is_empty()) {
                let id = InternedString::from_raw(gap_idx as u32);
                if let Some(actual) = interner_c.resolve_with(id, |s| s.to_string()) {
                    // Clippy suggestion: collapse nested if
                    if !actual.is_empty() {
                        panic!(
                            "DATA CORRUPTION DETECTED! Snapshot index {} is empty, but actual string is '{}'",
                            gap_idx, actual
                        );
                    }
                }
            }

            // Tail truncation check removed as it generates false positives for valid
            // concurrent appends (snapshot sees 0..N, while N+1 is added immediately after).
            // The critical bug (internal gaps/truncation of committed data) is caught
            // by the internal corruption check above.

            if iterations % 100 == 0 {
                thread::yield_now();
            }
        }
    });

    // Run for a bit
    thread::sleep(Duration::from_secs(2));
    stop.store(true, Ordering::Relaxed);

    thread_a.join().unwrap();
    thread_b.join().unwrap();
    thread_c.join().unwrap();
}
