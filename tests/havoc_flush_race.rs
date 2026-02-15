use aletheiadb::storage::wal::entry::LSN;
use aletheiadb::storage::wal::flush_coordinator::{
    FlushCoordinator, FlushCoordinatorConfig, SegmentMetadata,
};
use aletheiadb::storage::wal::ring_buffer::PendingEntry;
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::tempdir;

#[test]
fn test_flush_coordinator_race_condition() {
    let dir = tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();

    // Set segment size small enough to trigger rotation frequently
    // Header is 5 bytes. Data is 8 bytes per entry.
    // Segment size 100 bytes => ~12 entries per segment.
    let mut config = FlushCoordinatorConfig::new(dir_path.clone());
    config.segment_size = 100;
    // We don't care about sync for this test, just the rotation logic
    config.sync_on_flush = false;

    let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

    // Use multiple threads to create contention on segment rotation
    let num_threads = 50;
    let entries_per_thread = 100;

    for _ in 0..10 {
        let barrier = Arc::new(Barrier::new(num_threads));
        let mut handles = Vec::new();

        for t in 0..num_threads {
            let coordinator = Arc::clone(&coordinator);
            let barrier = Arc::clone(&barrier);

            handles.push(thread::spawn(move || {
                barrier.wait();
                for i in 0..entries_per_thread {
                    // LSN is unique across threads to allow checking later
                    // We use a large multiplier to avoid collisions between iterations
                    let lsn =
                        (t * entries_per_thread + i) as u64 + (coordinator.total_flushes() * 1000);
                    // Store LSN in data for verification (little endian)
                    let data = lsn.to_le_bytes().to_vec();
                    let entry = PendingEntry::new_async(LSN(lsn), data);

                    // Flush individually to increase contention on rotation check
                    // We ignore errors here as we are testing for data consistency, not I/O errors
                    let _ = coordinator.flush(vec![entry], false);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }
    }

    // Verify consistency
    // Read all segments and check if metadata matches content
    let mut segments = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "log") {
                if let Some(id) = path
                    .file_stem()
                    .and_then(|s| s.to_string_lossy().parse::<u64>().ok())
                {
                    segments.push((id, path));
                }
            }
        }
    }
    segments.sort_by_key(|(id, _)| *id);

    println!("Found {} segments", segments.len());
    let mut failures = 0;

    for (id, path) in segments {
        // Read content
        let content = match std::fs::read(&path) {
            Ok(c) => c,
            Err(e) => {
                println!("Failed to read segment {}: {}", id, e);
                continue;
            }
        };

        // Skip header (5 bytes: 4 magic + 1 version)
        if content.len() < 5 {
            continue;
        }
        let data_slice = &content[5..];

        // Parse entries (fixed 8 bytes because we wrote u64 LSNs)
        let mut actual_min = u64::MAX;
        let mut actual_max = 0;
        let mut count = 0;

        for chunk in data_slice.chunks_exact(8) {
            let lsn = u64::from_le_bytes(chunk.try_into().unwrap());
            actual_min = actual_min.min(lsn);
            actual_max = actual_max.max(lsn);
            count += 1;
        }

        if count == 0 {
            continue;
        }

        // Read metadata
        let metadata = match coordinator.read_segment_metadata(id) {
            Some(m) => m,
            None => {
                // Metadata file might be missing if rotation didn't happen gracefully or test ended
                // But for intermediate segments it should exist.
                // Let's just log and continue if missing, but it's suspicious.
                println!("Missing metadata for segment {}", id);
                continue;
            }
        };

        println!(
            "Segment {}: Content LSN range [{}-{}], Metadata LSN range [{}-{}]",
            id, actual_min, actual_max, metadata.min_lsn.0, metadata.max_lsn.0
        );

        if metadata.min_lsn.0 > actual_min {
            println!(
                "FAILURE: Segment {} metadata min LSN {} > actual min LSN {}",
                id, metadata.min_lsn.0, actual_min
            );
            failures += 1;
        }
        if metadata.max_lsn.0 < actual_max {
            println!(
                "FAILURE: Segment {} metadata max LSN {} < actual max LSN {}",
                id, metadata.max_lsn.0, actual_max
            );
            failures += 1;
        }
    }

    assert_eq!(
        failures, 0,
        "Found {} segments with inconsistent metadata",
        failures
    );
}
