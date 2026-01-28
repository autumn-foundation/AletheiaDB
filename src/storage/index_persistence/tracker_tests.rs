#[cfg(test)]
mod tests {

    use crate::storage::index_persistence::tracker::PersistenceTracker;

    #[test]
    fn test_tracker_initialization() {
        let tracker = PersistenceTracker::new();
        assert_eq!(tracker.get_vector_mutations(), 0);
        assert_eq!(tracker.get_graph_mutations(), 0);
        assert_eq!(tracker.get_temporal_mutations(), 0);
        assert_eq!(tracker.get_string_mutations(), 0);
        assert!(!tracker.is_shutdown());
    }

    #[test]
    fn test_mutation_counting() {
        let tracker = PersistenceTracker::new();

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
    }

    #[test]
    fn test_reset_mutations() {
        let tracker = PersistenceTracker::new();

        tracker.record_vector_mutation();
        tracker.record_graph_mutation();
        tracker.record_temporal_mutation();
        tracker.record_string_mutation();

        assert_eq!(tracker.reset_vector_mutations(), 1);
        assert_eq!(tracker.get_vector_mutations(), 0);

        assert_eq!(tracker.reset_graph_mutations(), 1);
        assert_eq!(tracker.get_graph_mutations(), 0);

        assert_eq!(tracker.reset_temporal_mutations(), 1);
        assert_eq!(tracker.get_temporal_mutations(), 0);

        assert_eq!(tracker.reset_string_mutations(), 1);
        assert_eq!(tracker.get_string_mutations(), 0);
    }

    #[test]
    fn test_shutdown_signal() {
        let tracker = PersistenceTracker::new();
        assert!(!tracker.is_shutdown());

        tracker.signal_shutdown();
        assert!(tracker.is_shutdown());
    }

    #[test]
    fn test_time_tracking() {
        let tracker = PersistenceTracker::new();
        // Just verify it doesn't panic and returns reasonable values
        // We can't mock SystemTime easily without a trait, but we can check bounds
        let seconds = tracker.seconds_since_vector_persist();
        assert!(seconds < 100); // Should be very recent

        // Reset should update timestamp (conceptually), but hard to test without sleep
        // or dependency injection. Main goal is ensuring no panics.
        tracker.reset_vector_mutations();
    }
}
