#[cfg(test)]
mod tests {
    use super::super::worker::update_manifest;
    use crate::storage::historical::HistoricalStorage;
    use crate::storage::index_persistence::IndexPersistenceManager;
    use crate::storage::index_persistence::tracker::PersistenceTracker;
    use parking_lot::RwLock;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn test_update_manifest_uses_safe_minimum_lsn() {
        // Setup dependencies
        let temp_dir = tempdir().unwrap();
        let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let tracker = Arc::new(PersistenceTracker::new());

        // Initialize tracker with a base LSN
        tracker.set_start_lsn(100);

        // Simulate a scenario where components are out of sync:
        // - Vector index is updated to LSN 200
        // - Graph index is updated to LSN 200
        // - String interner is updated to LSN 200
        // - BUT Temporal index is lagging at LSN 100 (didn't persist yet)
        tracker.update_vector_lsn(200);
        tracker.update_graph_lsn(200);
        tracker.update_string_lsn(200);
        // Temporal LSN remains at 100

        // Call update_manifest
        // Note: The counts passed don't matter for LSN logic
        update_manifest(&manager, &historical, &tracker, 10, 10, 50);

        // Load the manifest back and verify LSN
        let manifest = manager
            .load_manifest_and_strings()
            .expect("Failed to load manifest");

        // CRITICAL CHECK:
        // The manifest LSN MUST be 100 (the minimum), not 200.
        // If it were 200, recovery would skip operations between 100 and 200
        // for the temporal index, causing data loss.
        assert_eq!(
            manifest.lsn, 100,
            "Manifest LSN should be the minimum of all component LSNs (100), but got {}",
            manifest.lsn
        );

        // Now update temporal to 200
        tracker.update_temporal_lsn(200);

        // Call update_manifest again
        update_manifest(&manager, &historical, &tracker, 10, 10, 50);

        let manifest_updated = manager
            .load_manifest_and_strings()
            .expect("Failed to load updated manifest");

        // Now it should be 200
        assert_eq!(
            manifest_updated.lsn, 200,
            "Manifest LSN should advance to 200 when all components catch up"
        );
    }
}
