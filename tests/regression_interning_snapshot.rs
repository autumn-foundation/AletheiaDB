#[cfg(test)]
mod tests {
    use aletheiadb::core::interning::StringInterner;
    use aletheiadb::core::interning::InternedString;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn test_get_all_strings_race_condition() {
        // Run multiple iterations to increase chance of hitting the race
        for _ in 0..5 {
            run_race_test();
        }
    }

    fn run_race_test() {
        let interner = Arc::new(StringInterner::new());
        let barrier = Arc::new(Barrier::new(2));

        // Pre-fill some strings
        for i in 0..100 {
            interner.intern(format!("init_{}", i)).unwrap();
        }

        let interner_clone = interner.clone();
        let barrier_clone = barrier.clone();

        // Spawn a thread that will insert strings concurrently
        let handle = thread::spawn(move || {
            barrier_clone.wait(); // Sync start
            for i in 0..5000 {
                interner_clone.intern(format!("race_{}", i)).unwrap();
                if i % 50 == 0 { thread::yield_now(); }
            }
        });

        barrier.wait(); // Sync start

        let mut max_len = 0;

        // Repeatedly call get_all_strings and check for consistency
        for _ in 0..50 {
            let snapshot = interner.get_all_strings();
            max_len = std::cmp::max(max_len, snapshot.len());

            // Check 1: No empty holes in the middle of the vector
            for (id, s) in snapshot.iter().enumerate() {
                if s.is_empty() {
                    let id_handle = InternedString::from_raw(id as u32);
                    // If the ID exists in the interner, the snapshot MUST have it.
                    if let Some(actual_str) = interner.resolve_with(id_handle, |s| s.to_string()) {
                         if !actual_str.is_empty() {
                             panic!(
                                 "Snapshot corruption (HOLE) at ID {}: expected '{}', got empty string. Snapshot len: {}",
                                 id, actual_str, snapshot.len()
                             );
                         }
                    }
                } else {
                    // Check 2: Content matches
                    let id_handle = InternedString::from_raw(id as u32);
                    if let Some(actual_str) = interner.resolve_with(id_handle, |s| s.to_string()) {
                        assert_eq!(s, &actual_str, "Snapshot content mismatch at ID {}", id);
                    }
                }
            }
        }

        handle.join().unwrap();

        // Check 3: Ensure we actually captured new data
        // This prevents the "Alibi Test" where we always return just the initial 100 items.
        assert!(max_len > 100, "Snapshot failed to capture any concurrent insertions");
    }
}
