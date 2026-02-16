#[cfg(test)]
mod tests {
    use aletheiadb::core::interning::StringInterner;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_repro_phantom_empty_string() {
        // This test attempts to reproduce a race condition where get_all_strings()
        // observes a reserved next_id but not the string in id_to_string,
        // resulting in a trailing empty string being persisted.

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
                // Add tiny sleep to increase chance of being preempted between next_id inc and map insert
                if i % 10 == 0 {
                    thread::yield_now();
                }
            }
        });

        // Reader thread: continuously take snapshots and check for phantom empty strings
        let reader = thread::spawn(move || {
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(2) {
                let snapshot = interner_reader.get_all_strings();

                // If the snapshot ends with an empty string, it's likely a phantom entry
                // from a reserved but not yet inserted ID.
                if let Some(last) = snapshot.last() {
                    if last.is_empty() && snapshot.len() > 0 {
                        // Double check: if the last element is empty, but we've interned actual strings,
                        // it's corrupted unless we actually interned empty string (which we didn't).
                        // Note: We only intern "s{i}", none are empty.
                        return Err(format!(
                            "Phantom empty string detected at end of snapshot! Len: {}, last element is empty.",
                            snapshot.len()
                        ));
                    }
                }

                // Also check for internal gaps if possible, though less likely with monotonic ID
                for (i, s) in snapshot.iter().enumerate() {
                    if s.is_empty() {
                        return Err(format!(
                            "Phantom empty string detected at index {}! Snapshot len: {}.",
                            i,
                            snapshot.len()
                        ));
                    }
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

        // Fail if phantom string detected
        if let Err(e) = result {
            panic!("{}", e);
        }
    }
}
