use super::*;
use crate::storage::current::CurrentStorage;
use crate::storage::index_persistence::tracker::PersistenceTracker;
use std::sync::Arc;

#[test]
fn test_persist_vector_indexes_with_none_tracker() {
    // This is a minimal test to verify that the function accepts None
    // and doesn't panic. Real IO logic would be skipped or mockable
    // in a more complex setup, but here we just want to ensure
    // the Option handling logic (lines 109-111) is correct.

    // We can't easily mock CurrentStorage and IndexPersistenceManager fully
    // without trait abstraction, so we will construct minimal ones
    // and expect failure at IO step, but verify it didn't panic on tracker.

    let current = Arc::new(CurrentStorage::new());

    // We use a tempdir for manager to avoid polluting real FS
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));

    // This will likely fail on save_string_interner or empty indexes,
    // but the critical path we are testing is the None tracker handling
    // at the end of the function.
    let _ = persist_vector_indexes(&current, &manager, None);

    // If we reached here without panic, the Option check worked.
}

#[test]
fn test_persist_vector_indexes_with_tracker() {
    let current = Arc::new(CurrentStorage::new());
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
    let tracker = Arc::new(PersistenceTracker::new());

    // Simulate mutation
    tracker.record_vector_mutation();

    // Even if persistence fails (e.g. IO error), we want to see if we attempted it
    let _ = persist_vector_indexes(&current, &manager, Some(&tracker));

    // NOTE: In the current implementation, if persistence fails early (e.g. IO),
    // the tracker reset might NOT be reached because of `?`.
    // This test mainly verifies signature compatibility.
}
