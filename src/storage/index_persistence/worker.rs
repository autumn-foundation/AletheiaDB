//! Background persistence worker.
//!
//! This module manages the background thread responsible for automatic index persistence.
//! It implements the persistence loop, policy checking, and crash safety mechanisms.
//!
//! # Responsibilities
//!
//! - **Policy Enforcement**: Monitors mutation counts and time elapsed for each index type.
//! - **Automatic Triggering**: Executes persistence operations when thresholds are met.
//! - **Graceful Shutdown**: Ensures all indexes are saved one last time on shutdown.
//! - **Crash Safety**: Isolates persistence failures from the main application thread.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────┐
//! │  PersistenceTracker  │◄───────┐
//! │ (Mutation Counters)  │        │ Checks
//! └──────────────────────┘        │ Counters
//!                                 │
//! ┌──────────────────────┐   ┌────┴────────────┐
//! │   Main Thread        │   │ Background      │
//! │ (Writes/Mutations)   │   │ Persistence     │
//! └──────────────────────┘   │ Worker Thread   │
//!                            └────┬────────────┘
//!                                 │
//!                                 │ Triggers
//!                                 │ Save
//!                                 ▼
//!                        ┌──────────────────────┐
//!                        │ Operations Module    │
//!                        │ (persist_*_index)    │
//!                        └──────────────────────┘
//! ```
//!
//! # Persistence Loop
//!
//! The worker thread runs in a loop with a 1-second sleep interval. In each iteration:
//! 1. Checks for shutdown signal.
//! 2. Evaluates policies for each index type (vector, graph, temporal, strings).
//! 3. If a threshold is met (mutations or time), triggers persistence for that index.
//! 4. Logs any errors but continues running (unless it panics).
//!
//! # Crash Safety
//!
//! The entire worker loop is wrapped in `std::panic::catch_unwind`. If the persistence
//! logic panics (e.g., due to a bug or memory corruption):
//! 1. The panic is caught and logged to stderr.
//! 2. The `stopped_flag` is set to `true`.
//! 3. The main application continues running, but automatic persistence stops.
//! 4. Users are warned via stderr that persistence has failed.
//!
//! This ensures that a bug in the persistence layer doesn't crash the database.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::RwLock;

use crate::index::temporal::TemporalIndexes;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::index_persistence::IndexPersistenceManager;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;

use super::formats::PersistencePolicies;
use super::operations::{
    persist_all_indexes, persist_graph_index, persist_string_interner, persist_temporal_index,
    persist_vector_indexes,
};
use super::tracker::PersistenceTracker;

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
pub(crate) fn spawn_background_persistence_thread(
    current: Arc<CurrentStorage>,
    historical: Arc<RwLock<HistoricalStorage>>,
    temporal_indexes: Arc<TemporalIndexes>,
    wal: Arc<ConcurrentWalSystem>,
    manager: Arc<IndexPersistenceManager>,
    tracker: Arc<PersistenceTracker>,
    policies: PersistencePolicies,
    stopped_flag: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // Wrap entire thread in panic handler to prevent silent failures
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Check policies every second
            let check_interval = std::time::Duration::from_secs(1);

            while !tracker.is_shutdown() {
                std::thread::sleep(check_interval);

                // Skip if shutdown signaled
                if tracker.is_shutdown() {
                    break;
                }

                // Check vector index policy
                let vector_mutations = tracker.get_vector_mutations();
                let vector_seconds = tracker.seconds_since_vector_persist();
                if (vector_mutations >= policies.vector.mutation_threshold as u64
                    || vector_seconds >= policies.vector.time_interval_secs as u64)
                    && let Err(e) = persist_vector_indexes(&current, &manager, Some(&tracker))
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
                    && let Err(e) = persist_graph_index(&current, &manager, Some(&tracker))
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
                    && let Err(e) =
                        persist_temporal_index(&historical, &temporal_indexes, &manager, &tracker)
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
                    && let Err(e) = persist_string_interner(&manager, &tracker)
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
                &tracker,
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
    })
}
