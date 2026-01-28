//! Background thread for automatic index persistence.
//!
//! This module handles the background thread that periodically checks persistence policies
//! and triggers index saves when thresholds are exceeded.

use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::IndexPersistenceManager;
use super::formats::PersistencePolicies;
use super::operations::{
    persist_all_indexes, persist_graph_index, persist_string_interner, persist_temporal_index,
    persist_vector_indexes,
};
use super::tracker::PersistenceTracker;
use crate::index::temporal::TemporalIndexes;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;

/// Spawn a background thread for automatic index persistence.
///
/// This thread periodically checks persistence policies and triggers index saves when:
/// - Mutation thresholds are exceeded
/// - Time intervals have elapsed
/// - Special events occur (e.g., graph adjacency rebuild)
///
/// # Crash Safety
///
/// The thread is wrapped in `catch_unwind` to prevent silent failures. If a panic occurs:
/// - The panic is logged to stderr
/// - The `stopped_flag` is set to true
/// - The database continues running but future persistence attempts will fail with warnings
///
/// This prevents data corruption while alerting users to the failure.
#[allow(clippy::too_many_arguments)]
pub fn spawn_background_persistence_thread(
    current: Arc<CurrentStorage>,
    historical: Arc<RwLock<HistoricalStorage>>,
    temporal_indexes: Arc<TemporalIndexes>,
    wal: Arc<ConcurrentWalSystem>,
    manager: Arc<IndexPersistenceManager>,
    tracker: Arc<PersistenceTracker>,
    policies: PersistencePolicies,
    stopped_flag: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        // Wrap entire thread in panic handler to prevent silent failures
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Check policies every second
            let check_interval = Duration::from_secs(1);

            while !tracker.is_shutdown() {
                thread::sleep(check_interval);

                // Skip if shutdown signaled
                if tracker.is_shutdown() {
                    break;
                }

                // Check vector index policy
                let vector_mutations = tracker.get_vector_mutations();
                let vector_seconds = tracker.seconds_since_vector_persist();
                if (vector_mutations >= policies.vector.mutation_threshold as u64
                    || vector_seconds >= policies.vector.time_interval_secs as u64)
                    && let Err(e) = persist_vector_indexes(&current, &manager, Some(&*tracker))
                {
                    eprintln!(
                        "Background persistence: Failed to persist vector indexes: {}",
                        e
                    );
                }

                // Check graph index policy
                let graph_mutations = tracker.get_graph_mutations();
                let graph_seconds = tracker.seconds_since_graph_persist();
                if (graph_mutations >= policies.graph.mutation_threshold as u64
                    || graph_seconds >= policies.graph.time_interval_secs as u64)
                    && let Err(e) = persist_graph_index(&current, &manager, Some(&*tracker))
                {
                    eprintln!(
                        "Background persistence: Failed to persist graph index: {}",
                        e
                    );
                }

                // Check temporal index policy
                let temporal_mutations = tracker.get_temporal_mutations();
                let temporal_seconds = tracker.seconds_since_temporal_persist();
                if (temporal_mutations >= policies.temporal.version_threshold as u64
                    || temporal_seconds >= policies.temporal.time_interval_secs as u64)
                    && let Err(e) = persist_temporal_index(
                        &historical,
                        &temporal_indexes,
                        &manager,
                        Some(&*tracker),
                    )
                {
                    eprintln!(
                        "Background persistence: Failed to persist temporal index: {}",
                        e
                    );
                }

                // Check string interner policy
                let string_mutations = tracker.get_string_mutations();
                let string_seconds = tracker.seconds_since_string_persist();
                if (string_mutations >= policies.strings.new_strings_threshold as u64
                    || string_seconds >= policies.strings.time_interval_secs as u64)
                    && let Err(e) = persist_string_interner(&manager, Some(&*tracker))
                {
                    eprintln!(
                        "Background persistence: Failed to persist string interner: {}",
                        e
                    );
                }
            }

            // Final persist on shutdown
            let _ = persist_all_indexes(
                &current,
                &historical,
                &temporal_indexes,
                &wal,
                &manager,
                Some(&*tracker),
            );
        }));

        // Set stopped flag and log regardless of normal exit or panic
        stopped_flag.store(true, Ordering::Release);

        match result {
            Ok(()) => {
                eprintln!(
                    "Warning: Background persistence thread exited normally but unexpectedly. Future persistence operations will fail."
                );
            }
            Err(e) => {
                eprintln!("CRITICAL: Background persistence thread panicked: {:?}", e);
                eprintln!(
                    "Database will continue running but NO FURTHER INDEX PERSISTENCE will occur."
                );
                eprintln!("You MUST restart the database to restore automatic persistence.");
            }
        }
    });
}
