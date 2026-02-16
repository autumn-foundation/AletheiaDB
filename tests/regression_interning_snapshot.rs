#[cfg(test)]
mod tests {
    use aletheiadb::core::interning::StringInterner;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_concurrent_snapshot_integrity() {
        // Regression test for snapshot truncation bug.
        // Ensures that get_all_strings() never returns a vector missing IDs
        // that are already resolvable (present in id_to_string), even under
        // heavy concurrent modification.

        let interner = Arc::new(StringInterner::new());
        let interner_writer = interner.clone();
        let interner_reader = interner.clone();

        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running_writer = running.clone();

        // Writer thread: continuously intern strings
        let writer = thread::spawn(move || {
            let mut i = 0;
            while running_writer.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = interner_writer.intern(format!("s{}", i));
                i += 1;
                if i % 100 == 0 {
                    thread::yield_now();
                }
            }
        });

        // Reader thread: continuously take snapshots and check for truncation
        let reader = thread::spawn(move || {
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(2) {
                // Capture length before taking snapshot.
                // The snapshot MUST contain at least this many elements.
                // If the snapshot is smaller than the number of committed strings before we started,
                // we have dropped data.
                let min_expected = interner_reader.len();
                let snapshot = interner_reader.get_all_strings();

                if snapshot.len() < min_expected {
                    return Err(format!(
                        "Snapshot truncation detected! Expected at least {}, got {}.",
                        min_expected,
                        snapshot.len()
                    ));
                }
                thread::yield_now();
            }
            Ok(())
        });

        // Let it run for a bit
        thread::sleep(Duration::from_secs(2));
        running.store(false, std::sync::atomic::Ordering::Relaxed);

        let _ = writer.join();
        let result = reader.join().unwrap();

        // The reader returns Err if it detects truncation.
        assert!(
            result.is_ok(),
            "Snapshot truncation detected: {}",
            result.unwrap_err()
        );
    }
}
