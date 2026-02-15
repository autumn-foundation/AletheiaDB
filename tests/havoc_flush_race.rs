use aletheiadb::storage::wal::LSN;
use aletheiadb::storage::wal::flush_coordinator::{FlushCoordinator, FlushCoordinatorConfig};
use aletheiadb::storage::wal::ring_buffer::PendingEntry;
use std::sync::Arc;
use std::thread;

fn create_entry(lsn: u64) -> PendingEntry {
    // 8 bytes payload containing LSN
    PendingEntry::new_async(LSN(lsn), lsn.to_le_bytes().to_vec())
}

#[test]
fn test_flush_race_condition() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = FlushCoordinatorConfig::new(dir.path());
    // Set segment size small to force frequent rotations
    // Header (5) + 2 entries (16) = 21 bytes.
    // If we set limit to 20, it should rotate every 2 entries roughly.
    config.segment_size = 20;
    config.segments_to_retain = 1000; // Keep everything

    let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

    let num_threads = 4;
    let entries_per_thread = 100;

    let mut handles = Vec::new();

    for t in 0..num_threads {
        let coordinator = Arc::clone(&coordinator);
        handles.push(thread::spawn(move || {
            for i in 0..entries_per_thread {
                let lsn = (t * entries_per_thread + i) as u64;
                let entry = create_entry(lsn);
                // We ignore errors here as we are stress testing
                let _ = coordinator.flush(vec![entry], false);
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Now verify consistency
    let mut violations = 0;

    // Iterate over all segment files
    let segments = coordinator.list_segments_with_metadata();

    // Also check for segments that might be missing metadata (though list_segments_with_metadata filters them)
    // We want to be sure we check everything.
    // But let's trust list_segments_with_metadata for now as it gives us the metadata to check against.

    for (id, meta) in segments {
        let path = dir.path().join(format!("{:06}.log", id));
        let content = std::fs::read(&path).unwrap();

        // Skip header
        if content.len() < 5 {
            continue;
        }
        let data = &content[5..];

        let mut actual_min = u64::MAX;
        let mut actual_max = 0;
        let mut count = 0;

        for chunk in data.chunks_exact(8) {
            let lsn = u64::from_le_bytes(chunk.try_into().unwrap());
            actual_min = actual_min.min(lsn);
            actual_max = actual_max.max(lsn);
            count += 1;
        }

        if count == 0 {
            continue;
        }

        println!(
            "Segment {}: Meta[{}, {}] Actual[{}, {}]",
            id, meta.min_lsn.0, meta.max_lsn.0, actual_min, actual_max
        );

        if meta.min_lsn.0 > actual_min {
            println!(
                "VIOLATION: Meta min {} > Actual min {}",
                meta.min_lsn.0, actual_min
            );
            violations += 1;
        }

        if meta.max_lsn.0 < actual_max {
            println!(
                "VIOLATION: Meta max {} < Actual max {}",
                meta.max_lsn.0, actual_max
            );
            violations += 1;
        }
    }

    assert_eq!(
        violations, 0,
        "Found {} metadata consistency violations due to race condition",
        violations
    );
}
