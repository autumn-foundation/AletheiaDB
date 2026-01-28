#![cfg(test)]

use gallifreydb::storage::index_persistence::PersistenceTracker;
use std::thread;
use std::time::Duration;

#[test]
fn test_persistence_tracker_basic_ops() {
    let tracker = PersistenceTracker::new();

    // Initial state
    assert_eq!(tracker.get_vector_mutations(), 0);
    assert_eq!(tracker.get_graph_mutations(), 0);
    assert_eq!(tracker.get_temporal_mutations(), 0);
    assert_eq!(tracker.get_string_mutations(), 0);
    assert!(!tracker.is_shutdown());

    // Record mutations
    tracker.record_vector_mutation();
    tracker.record_vector_mutation();
    assert_eq!(tracker.get_vector_mutations(), 2);

    tracker.record_graph_mutation();
    assert_eq!(tracker.get_graph_mutations(), 1);

    tracker.record_temporal_mutation();
    tracker.record_temporal_mutation();
    tracker.record_temporal_mutation();
    assert_eq!(tracker.get_temporal_mutations(), 3);

    tracker.record_string_mutation();
    assert_eq!(tracker.get_string_mutations(), 1);

    // Reset mutations
    assert_eq!(tracker.reset_vector_mutations(), 2);
    assert_eq!(tracker.get_vector_mutations(), 0);

    assert_eq!(tracker.reset_graph_mutations(), 1);
    assert_eq!(tracker.get_graph_mutations(), 0);

    assert_eq!(tracker.reset_temporal_mutations(), 3);
    assert_eq!(tracker.get_temporal_mutations(), 0);

    assert_eq!(tracker.reset_string_mutations(), 1);
    assert_eq!(tracker.get_string_mutations(), 0);

    // Default impl
    let default_tracker = PersistenceTracker::default();
    assert_eq!(default_tracker.get_vector_mutations(), 0);
}

#[test]
fn test_persistence_tracker_timing() {
    let tracker = PersistenceTracker::new();

    // Sleep to ensure time passes
    thread::sleep(Duration::from_secs(1));

    // Check timing methods (should be >= 1s)
    let vec_secs = tracker.seconds_since_vector_persist();
    assert!(vec_secs >= 1, "Should be at least 1s since init");

    let graph_secs = tracker.seconds_since_graph_persist();
    assert!(graph_secs >= 1);

    let temp_secs = tracker.seconds_since_temporal_persist();
    assert!(temp_secs >= 1);

    let str_secs = tracker.seconds_since_string_persist();
    assert!(str_secs >= 1);

    // Reset updates timestamp
    tracker.reset_vector_mutations();
    // Should be close to 0 now
    assert!(tracker.seconds_since_vector_persist() < 2);
}

#[test]
fn test_persistence_tracker_shutdown() {
    let tracker = PersistenceTracker::new();
    assert!(!tracker.is_shutdown());

    tracker.signal_shutdown();
    assert!(tracker.is_shutdown());
}
