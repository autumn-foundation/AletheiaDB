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

use super::formats::{
    GraphIndexManifestEntry, IndexManifest, PersistencePolicies, StringInternerManifestEntry,
    TemporalAdjacencyIndexManifestEntry,
};
use super::operations::{
    persist_all_indexes, persist_graph_index, persist_string_interner, persist_temporal_index,
    persist_vector_indexes,
};
use super::tracker::PersistenceTracker;

/// Update the manifest file to reflect persisted index state.
///
/// **CRITICAL**: This function must be called after any index persistence operation
/// during background persistence. Without this, the manifest becomes stale/missing,
/// causing recovery failures after crashes (e.g., string interner ID mismatches).
///
/// # Parameters
///
/// * `snapshot_lsn` - The LSN captured BEFORE persistence started. This is critical
///   for crash recovery: the manifest must record a conservative LSN that is guaranteed
///   to be consistent with the persisted data. If we used the current LSN (after
///   persistence), concurrent writes could cause the manifest to reference a state
///   ahead of what's on disk, leading to data loss during WAL replay.
///
/// * `node_count`, `edge_count` - The exact counts of nodes/edges that were persisted.
///   These should be captured at the same time as the snapshot LSN to ensure consistency.
///
/// # Background
///
/// Issue #1011: Before this fix, the manifest was ONLY saved during clean shutdown
/// via `persist_all_indexes`. This caused a critical bug:
/// 1. Background worker saves indexes (graph.idx, temporal.idx, interner.idx)
/// 2. Manifest is NOT updated (remains stale or doesn't exist)
/// 3. Process crashes or is killed via Ctrl+C
/// 4. On restart: indexes exist but manifest is missing
/// 5. Recovery loads interner but temporal data references missing string IDs
///
/// This function ensures the manifest stays in sync with persisted index files,
/// enabling successful recovery even without clean shutdown.
///
/// # Error Handling
///
/// Errors are logged but swallowed to prevent killing the background persistence thread.
/// Manifest save failures are non-fatal since:
/// - Index files are still valid (can be loaded with best-effort recovery)
/// - Next persistence cycle will retry manifest save
/// - WAL provides durability for data
pub(crate) fn update_manifest(
    manager: &Arc<IndexPersistenceManager>,
    historical: &Arc<RwLock<HistoricalStorage>>,
    tracker: &Arc<PersistenceTracker>,
    node_count: u64,
    edge_count: u64,
    string_count: u64,
) {
    // CRITICAL: Use the safe manifest LSN from tracker, which is the minimum
    // LSN of all persisted components. This prevents the manifest from
    // claiming consistency up to an LSN that some components haven't reached yet.
    let safe_lsn = tracker.get_safe_manifest_lsn();
    let mut manifest = IndexManifest::new(safe_lsn);

    // Add string interner entry (always present if any data exists)
    if string_count > 0 {
        manifest.string_interner = Some(StringInternerManifestEntry {
            interner_file: "strings/interner.idx".to_string(),
            string_count,
        });
    }

    // Add graph index entry if we have nodes/edges
    if node_count > 0 || edge_count > 0 {
        manifest.graph_index = Some(GraphIndexManifestEntry {
            adjacency_file: "graph/adjacency.idx".to_string(),
            node_count,
            edge_count,
        });
    }

    // Add temporal adjacency index entry if configured
    let hist_read = historical.read();
    if let Some(adj_index) = hist_read.get_temporal_adjacency_index() {
        let total_entries: usize = adj_index
            .outgoing
            .iter()
            .map(|entry| entry.value().len())
            .sum();
        let node_count = adj_index.outgoing.len();

        if total_entries > 0 {
            manifest.temporal_adjacency_index = Some(TemporalAdjacencyIndexManifestEntry {
                adjacency_file: "temporal_adjacency/adjacency.idx".to_string(),
                entry_count: total_entries as u64,
                node_count: node_count as u64,
            });
        }
    }
    drop(hist_read);

    // Save manifest (swallow errors to avoid killing background thread)
    // Note: Using eprintln! for consistency with other error handling in this module.
    // The `tracing` crate is optional and behind feature flags, while this module
    // consistently uses eprintln! for direct stderr logging.
    if let Err(e) = manager.save_manifest(&manifest) {
        eprintln!("Background persistence: Failed to save manifest: {}", e);
    }
}

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

                // Track if any index was persisted this cycle
                let mut any_index_persisted = false;

                // CRITICAL: Capture snapshot state BEFORE persistence operations start
                // This ensures the manifest records a conservative LSN/counts that are
                // guaranteed to be consistent with what gets written to disk, preventing
                // data loss during recovery if concurrent writes happen during persistence.
                let snapshot_lsn = wal.current_lsn().0;
                let node_count = current.node_count() as u64;
                let edge_count = current.edge_count() as u64;
                let string_count = crate::core::GLOBAL_INTERNER.len() as u64;

                // Check vector index policy
                let vector_mutations = tracker.get_vector_mutations();
                let vector_seconds = tracker.seconds_since_vector_persist();
                if vector_mutations >= policies.vector.mutation_threshold as u64
                    || vector_seconds >= policies.vector.time_interval_secs as u64
                {
                    match persist_vector_indexes(&current, &manager, Some(&tracker), snapshot_lsn) {
                        Ok(()) => any_index_persisted = true,
                        Err(e) => {
                            eprintln!(
                                "Background persistence: Failed to persist vector indexes: {}",
                                e
                            );
                        }
                    }
                }

                // Check graph index policy
                let graph_mutations = tracker.get_graph_mutations();
                let graph_seconds = tracker.seconds_since_graph_persist();
                if graph_mutations >= policies.graph.mutation_threshold as u64
                    || graph_seconds >= policies.graph.time_interval_secs as u64
                {
                    match persist_graph_index(&current, &manager, Some(&tracker), snapshot_lsn) {
                        Ok(()) => any_index_persisted = true,
                        Err(e) => {
                            eprintln!(
                                "Background persistence: Failed to persist graph index: {}",
                                e
                            );
                        }
                    }
                }

                // Check temporal index policy
                let temporal_mutations = tracker.get_temporal_mutations();
                let temporal_seconds = tracker.seconds_since_temporal_persist();
                if temporal_mutations >= policies.temporal.version_threshold as u64
                    || temporal_seconds >= policies.temporal.time_interval_secs as u64
                {
                    match persist_temporal_index(
                        &historical,
                        &temporal_indexes,
                        &manager,
                        &tracker,
                        snapshot_lsn,
                    ) {
                        Ok(()) => any_index_persisted = true,
                        Err(e) => {
                            eprintln!(
                                "Background persistence: Failed to persist temporal index: {}",
                                e
                            );
                        }
                    }
                }

                // Check string interner policy
                let string_mutations = tracker.get_string_mutations();
                let string_seconds = tracker.seconds_since_string_persist();
                if string_mutations >= policies.strings.new_strings_threshold as u64
                    || string_seconds >= policies.strings.time_interval_secs as u64
                {
                    match persist_string_interner(&manager, &tracker, snapshot_lsn) {
                        Ok(()) => any_index_persisted = true,
                        Err(e) => {
                            eprintln!(
                                "Background persistence: Failed to persist string interner: {}",
                                e
                            );
                        }
                    }
                }

                // CRITICAL FIX (Issue #1011): Update manifest after any index persistence
                // This ensures manifest stays in sync with index files on disk, preventing
                // recovery failures after crashes (e.g., string ID mismatches in temporal data).
                // Without this, manifest is only saved on clean shutdown, which never happens
                // when the process is killed via Ctrl+C or crashes.
                //
                // We use the safe LSN from tracker (min of component LSNs) to ensuring
                // the manifest doesn't jump ahead of lagging components.
                if any_index_persisted {
                    update_manifest(
                        &manager,
                        &historical,
                        &tracker,
                        node_count,
                        edge_count,
                        string_count,
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
                // If shutdown was signaled, this is a normal exit.
                // Only warn if the thread exited without a shutdown signal.
                if !tracker.is_shutdown() {
                    eprintln!(
                        "Warning: Background persistence thread exited normally but unexpectedly. Future persistence operations will fail."
                    );
                }
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
