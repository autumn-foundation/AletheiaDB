use aletheiadb::storage::wal::LSN;
use aletheiadb::storage::wal::flush_coordinator::{FlushCoordinator, FlushCoordinatorConfig};
use aletheiadb::storage::wal::ring_buffer::PendingEntry;
use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
#[allow(dead_code)] // The guard is used for its side effects (setting permissions)
struct DirPermissionGuard<'a> {
    path: &'a std::path::Path,
    original_mode: u32,
}

#[cfg(unix)]
impl<'a> DirPermissionGuard<'a> {
    #[allow(dead_code)] // The function is used in the test
    fn new(path: &'a std::path::Path, new_mode: u32) -> Self {
        let metadata = std::fs::metadata(path).unwrap();
        let original_mode = metadata.permissions().mode();
        let mut perms = metadata.permissions();
        perms.set_mode(new_mode);
        std::fs::set_permissions(path, perms).unwrap();
        Self {
            path,
            original_mode,
        }
    }
}

#[cfg(unix)]
impl<'a> Drop for DirPermissionGuard<'a> {
    fn drop(&mut self) {
        if let Ok(metadata) = std::fs::metadata(self.path) {
            let mut perms = metadata.permissions();
            perms.set_mode(self.original_mode);
            let _ = std::fs::set_permissions(self.path, perms);
        }
    }
}

#[test]
#[cfg(unix)]
fn test_metadata_corruption_on_error() {
    // 1. Setup
    let dir = tempdir().unwrap();
    let wal_dir = dir.path().to_path_buf();

    // Config: Small segment size to force rotation
    // Segment size includes header (5 bytes).
    // Entry size = wrapper + data.
    // We want entry size + header >= segment_size to rotate.
    let mut config = FlushCoordinatorConfig::new(&wal_dir);
    config.segment_size = 50;

    let coordinator = FlushCoordinator::new(config).unwrap();

    // 2. Write Segment 1 (LSN 1-10)
    // Create entries that fit in one batch but trigger rotation
    // 10 bytes * 5 entries = 50 bytes. + 5 header = 55 > 50.
    // So after this flush, it should rotate.
    let entries1: Vec<_> = (1..=5)
        .map(|i| PendingEntry::new_async(LSN(i), vec![i as u8; 10]))
        .collect();

    // Flush. Should succeed. Segment 1 created.
    let stats = coordinator.flush(entries1, true).unwrap();
    assert!(stats.segment_rotated, "Should rotate after first batch");

    // Now state is reset. Min LSN = MAX, Max LSN = 0.

    // 3. Prepare for failure
    // We want the NEXT flush to trigger rotation AND fail metadata write.
    // To do this, we need to write enough data to trigger rotation.
    // AND we need metadata write to fail.
    // Metadata write happens inside `maybe_rotate_segment`.
    // It tries to create `.meta` file in `wal_dir`.
    // If we make `wal_dir` read-only, file creation fails.

    // But wait, `flush` opens the segment file first.
    // `ensure_segment_open` creates a NEW file for the new segment.
    // If dir is read-only, `ensure_segment_open` will fail!
    // We want `ensure_segment_open` to SUCCEED (using existing handle or before we lock dir?).
    // `ensure_segment_open` is called at start of `flush`.

    // So we need to:
    // a. Call flush with small data (not rotating yet) to Open the segment.
    // b. Then make dir read-only.
    // c. Call flush with more data to Trigger Rotation.
    //    Rotation will try to write metadata. This fails.
    //    We want to see if internal state is corrupted.

    // Step 3a: Open Segment 2
    let entries2_small: Vec<_> = vec![PendingEntry::new_async(LSN(6), vec![6u8; 10])];
    // This flush opens Segment 2. Does not rotate (size 5+10=15 < 50).
    coordinator.flush(entries2_small, true).unwrap();

    // Step 3b: Make dir read-only
    let metadata = std::fs::metadata(&wal_dir).unwrap();
    let mut perms = metadata.permissions();
    perms.set_mode(0o500); // Read/Execute, No Write
    std::fs::set_permissions(&wal_dir, perms).unwrap();

    // Step 3c: Trigger Rotation & Failure
    // Add enough data to Segment 2 to force rotation.
    // Current size ~15. Need > 35 more.
    let entries2_large: Vec<_> = (7..=10)
        .map(|i| PendingEntry::new_async(LSN(i), vec![i as u8; 10]))
        .collect();

    // This flush should:
    // 1. Write data to open file handle (Succeeds - handle already open)
    // 2. Trigger `maybe_rotate_segment`
    // 3. Try `write_segment_metadata`. Fails (dir read-only).
    // 4. Return Err.
    let result = coordinator.flush(entries2_large, true);
    assert!(
        result.is_err(),
        "Flush should fail due to metadata write error"
    );

    // Step 4: Restore permissions
    let metadata = std::fs::metadata(&wal_dir).unwrap();
    let mut perms = metadata.permissions();
    perms.set_mode(0o755); // Restore write
    std::fs::set_permissions(&wal_dir, perms).unwrap();

    // Step 5: Write Segment 3
    // If state was NOT reset, Segment 3 will inherit Segment 2's LSNs.
    // Segment 2 had LSNs 6..10.
    // Segment 3 starts with LSN 11.
    // If corrupted, Segment 3 Min LSN will be min(6, 11) = 6.
    // If correct, Segment 3 Min LSN will be 11.

    let entries3: Vec<_> = (11..=15)
        .map(|i| PendingEntry::new_async(LSN(i), vec![i as u8; 10]))
        .collect();

    // This flush opens Segment 3.
    // NOTE: Segment 2 was effectively "closed" by the failed flush (writer_guard set to None).
    // But LSN state might linger.
    coordinator.flush(entries3, true).unwrap();

    // Force rotation of Segment 3 to write its metadata
    let entries3_rotate: Vec<_> = (16..=20)
        .map(|i| PendingEntry::new_async(LSN(i), vec![i as u8; 10]))
        .collect();
    coordinator.flush(entries3_rotate, true).unwrap();

    // Step 6: Verify Segment 3 Metadata
    // Segment 1 (ID 1).
    // Segment 2 (ID 2) - Failed to rotate properly (metadata missing).
    // Segment 3 (ID 3) - Contains LSN 11..15. If corrupted, contains LSN 6..15 (min=6).

    let segments = coordinator.list_segments_with_metadata();

    // Find Segment 3 specifically
    let (id, meta) = segments
        .iter()
        .find(|(id, _)| *id == 3)
        .expect("Segment 3 not found");
    println!("Segment {} Metadata: Min LSN = {}", id, meta.min_lsn.0);

    // Verification
    // Segment 3 contains LSNs 11..15.
    // If contaminated by Segment 2 (LSN 6..10), min_lsn would be 6.

    assert_eq!(
        meta.min_lsn.0, 11,
        "CRITICAL: Segment 3 Min LSN corrupted! Expected 11, got {}. \
        This means LSN tracking was not reset after failed flush.",
        meta.min_lsn.0
    );
}
