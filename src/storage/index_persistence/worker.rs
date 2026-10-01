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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

/// Classify a background-persist failure as *deterministic* (cannot succeed on
/// retry until the process restarts with a raised cap) vs *transient* (I/O, or a
/// feature-gap that self-heals when the offending entity is edited; a later
/// attempt may succeed).
///
/// This matches a **structured** signal — a typed
/// [`StorageError::CapacityExceeded`](crate::core::error::StorageError::CapacityExceeded)
/// (configurable interner cap) — rather than scraping Display text, so a future
/// reword of an error message can never silently reintroduce the infinite-retry
/// hang. The persist pipeline preserves that variant to the worker via
/// [`IndexPersistenceError::into_persist_storage_error`](crate::storage::index_persistence::error::IndexPersistenceError::into_persist_storage_error).
///
/// Only genuine capacity/intern-exhaustion is deterministic. Notably the
/// Array/SparseVector "not yet supported for persistence" case is TRANSIENT: it
/// self-heals the moment the offending node/edge is updated or deleted, so it
/// must NOT permanently suspend the whole index (a `{:?}`/reword-proof structural
/// distinction, not a message-substring one). Pure I/O / disk failures are
/// likewise transient.
pub(crate) fn is_deterministic_persist_failure(e: &crate::core::error::Error) -> bool {
    matches!(
        e,
        crate::core::error::Error::Storage(
            crate::core::error::StorageError::CapacityExceeded { .. }
        )
    )
}

/// Per-index background-persist suspension latch (configurable interner cap fix).
///
/// On a **deterministic** persist failure (string-interner capacity exhausted,
/// or a value that cannot be interned) the affected index is SUSPENDED: no
/// further background attempts are made until the process restarts. This
/// replaces the pre-fix infinite 1-second retry that pegged a CPU and spammed
/// stderr forever on a capacity exhaustion — retrying is futile because raising
/// the cap requires a restart. Suspension is per-index, so a deterministic
/// failure on one index never silences the others. **Transient** (I/O) failures
/// do not suspend and stay on the normal cadence.
#[derive(Default)]
pub(crate) struct PersistSuspension {
    /// Once `true`, this index makes no further background-persist attempts.
    suspended: AtomicBool,
    /// Number of real persist attempts made (a suspended index makes none).
    /// Observability / test hook: after a deterministic failure this reaches
    /// exactly 1 and then stops.
    attempts: AtomicU64,
    /// Number of actionable "suspended" log lines emitted. Guaranteed to be at
    /// most 1 (emitted only on the transition into the suspended state). Test
    /// hook so a test can assert the log fires exactly once without capturing
    /// stderr.
    log_emits: AtomicU64,
}

impl PersistSuspension {
    /// Whether this index is suspended (no further attempts until restart).
    pub(crate) fn is_suspended(&self) -> bool {
        self.suspended.load(Ordering::Acquire)
    }

    /// Total real persist attempts made for this index (observability / test
    /// hook: after a deterministic failure this reaches exactly 1 and stops).
    #[cfg(test)]
    pub(crate) fn attempts(&self) -> u64 {
        self.attempts.load(Ordering::Acquire)
    }

    /// Number of actionable suspend-transition log lines emitted (0 or 1).
    #[cfg(test)]
    pub(crate) fn log_emits(&self) -> u64 {
        self.log_emits.load(Ordering::Acquire)
    }

    /// Run one persist attempt for `index_label` unless the index is suspended.
    ///
    /// Returns `Some(true)` on success, `Some(false)` on a handled failure, and
    /// `None` when skipped because the index was already suspended. A
    /// deterministic failure latches the suspension and logs (once); a transient
    /// failure logs on the normal cadence and leaves the index attemptable.
    pub(crate) fn attempt<T, F>(&self, index_label: &str, f: F) -> Option<bool>
    where
        F: FnOnce() -> crate::core::error::Result<T>,
    {
        if self.is_suspended() {
            return None;
        }
        self.attempts.fetch_add(1, Ordering::AcqRel);
        match f() {
            Ok(_) => Some(true),
            Err(e) => {
                let message = e.to_string();
                if is_deterministic_persist_failure(&e) {
                    // Latch suspension and log EXACTLY ONCE, on the transition.
                    if !self.suspended.swap(true, Ordering::AcqRel) {
                        self.log_emits.fetch_add(1, Ordering::AcqRel);
                        eprintln!(
                            "Background persistence SUSPENDED for {index_label}: {message}. The \
                             string interner is at capacity (limit is configured by \
                             `persistence.max_interned_strings`). NO DATA HAS BEEN LOST — the WAL \
                             is the source of truth; only the on-disk index snapshot is now stale. \
                             Raise `persistence.max_interned_strings` and RESTART to resume \
                             background index persistence."
                        );
                    }
                    Some(false)
                } else {
                    // Transient (I/O): keep the normal 1s cadence, no suspension.
                    eprintln!("Background persistence: Failed to persist {index_label}: {message}");
                    Some(false)
                }
            }
        }
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

            // Per-index suspension latches: a deterministic failure (e.g. the
            // string interner at capacity) suspends that index's background
            // persistence until restart, instead of retrying every cycle forever
            // (configurable interner cap fix).
            let vector_suspension = PersistSuspension::default();
            let graph_suspension = PersistSuspension::default();
            let temporal_suspension = PersistSuspension::default();
            let string_suspension = PersistSuspension::default();

            while !tracker.is_shutdown() {
                std::thread::sleep(check_interval);

                // Skip if shutdown signaled
                if tracker.is_shutdown() {
                    break;
                }

                // Track if any index was persisted this cycle
                let mut any_index_persisted = false;

                // CRITICAL: Capture snapshot state BEFORE persistence operations start
                // This ensures the manifest records a conservative LSN that is guaranteed
                // to be consistent with what gets written to disk, preventing data loss
                // during recovery if concurrent writes happen during persistence.
                let snapshot_lsn = wal.current_lsn().0;

                // Check vector index policy
                let vector_mutations = tracker.get_vector_mutations();
                let vector_seconds = tracker.seconds_since_vector_persist();
                if (vector_mutations >= policies.vector.mutation_threshold as u64
                    || vector_seconds >= policies.vector.time_interval_secs as u64)
                    && let Some(true) = vector_suspension.attempt("vector indexes", || {
                        persist_vector_indexes(&current, &manager, Some(&tracker), snapshot_lsn)
                    })
                {
                    any_index_persisted = true;
                }

                // Check graph index policy
                let graph_mutations = tracker.get_graph_mutations();
                let graph_seconds = tracker.seconds_since_graph_persist();
                if (graph_mutations >= policies.graph.mutation_threshold as u64
                    || graph_seconds >= policies.graph.time_interval_secs as u64)
                    && let Some(true) = graph_suspension.attempt("graph index", || {
                        persist_graph_index(&current, &manager, Some(&tracker), snapshot_lsn)
                    })
                {
                    any_index_persisted = true;
                }

                // Check temporal index policy
                let temporal_mutations = tracker.get_temporal_mutations();
                let temporal_seconds = tracker.seconds_since_temporal_persist();
                if (temporal_mutations >= policies.temporal.version_threshold as u64
                    || temporal_seconds >= policies.temporal.time_interval_secs as u64)
                    && let Some(true) = temporal_suspension.attempt("temporal index", || {
                        persist_temporal_index(
                            &historical,
                            &temporal_indexes,
                            &manager,
                            &tracker,
                            snapshot_lsn,
                        )
                    })
                {
                    any_index_persisted = true;
                }

                // Check string interner policy
                let string_mutations = tracker.get_string_mutations();
                let string_seconds = tracker.seconds_since_string_persist();
                if (string_mutations >= policies.strings.new_strings_threshold as u64
                    || string_seconds >= policies.strings.time_interval_secs as u64)
                    && let Some(true) = string_suspension.attempt("string interner", || {
                        persist_string_interner(&manager, &tracker, snapshot_lsn)
                    })
                {
                    any_index_persisted = true;
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
                        tracker.get_last_persisted_node_count(),
                        tracker.get_last_persisted_edge_count(),
                        tracker.get_last_persisted_string_count(),
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
                // Normal exit (only happens when shutdown is signaled)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::{Error, StorageError};

    fn capacity_error() -> Error {
        Error::Storage(StorageError::CapacityExceeded {
            resource: "string interner".to_string(),
            current: 200,
            limit: 200,
        })
    }

    fn array_unsupported_error() -> Error {
        // Mirrors how the Array "not yet supported for persistence" feature-gap
        // reaches the worker: flattened into a PersistenceError string (NOT a
        // typed CapacityExceeded).
        Error::Storage(StorageError::PersistenceError(
            "Failed to persist node properties: Serialization error: Array properties are not \
             yet supported for persistence. This prevents silent data loss."
                .to_string(),
        ))
    }

    #[test]
    fn test_deterministic_classifier_matches_typed_capacity_only() {
        // A typed CapacityExceeded (configurable interner cap) is deterministic.
        assert!(is_deterministic_persist_failure(&capacity_error()));

        // The Array "not yet supported" feature-gap is TRANSIENT: it self-heals
        // when the offending entity is edited/deleted, so it must NOT suspend.
        assert!(!is_deterministic_persist_failure(&array_unsupported_error()));

        // Pure I/O / disk failures are transient.
        assert!(!is_deterministic_persist_failure(&Error::Storage(
            StorageError::PersistenceError(
                "Failed to save graph index: No space left on device".to_string(),
            )
        )));
        assert!(!is_deterministic_persist_failure(&Error::Storage(
            StorageError::PersistenceError(
                "Failed to create graph directory: permission denied".to_string(),
            )
        )));
    }

    #[test]
    fn test_array_unsupported_does_not_permanently_suspend() {
        // Regression (configurable interner cap review): the Array "not yet
        // supported" feature-gap must NOT permanently suspend the whole index —
        // it self-heals. It stays attemptable on the normal cadence, like any
        // transient failure.
        let suspension = PersistSuspension::default();
        for _ in 0..5 {
            assert_eq!(
                suspension.attempt("graph index", || -> crate::core::error::Result<()> {
                    Err(array_unsupported_error())
                }),
                Some(false)
            );
        }
        assert!(
            !suspension.is_suspended(),
            "array-unsupported feature-gap must not permanently suspend the index"
        );
        assert_eq!(suspension.attempts(), 5);
        assert_eq!(suspension.log_emits(), 0);
    }

    #[test]
    fn test_deterministic_failure_suspends_after_exactly_one_attempt() {
        // Regression: a deterministic CapacityExceeded must NOT be retried every
        // cycle forever. After the first attempt the index suspends and makes no
        // further attempts (observable non-spin).
        let suspension = PersistSuspension::default();

        // First call: one real attempt, deterministic failure, latches suspend.
        assert_eq!(
            suspension.attempt("graph index", || -> crate::core::error::Result<()> {
                Err(capacity_error())
            }),
            Some(false)
        );
        assert!(suspension.is_suspended());
        assert_eq!(suspension.attempts(), 1);

        // Many further cycles: all skipped, attempt count STAYS at exactly 1.
        for _ in 0..1000 {
            assert_eq!(
                suspension.attempt("graph index", || -> crate::core::error::Result<()> {
                    panic!("suspended index must not run the persist closure");
                }),
                None
            );
        }
        assert_eq!(
            suspension.attempts(),
            1,
            "attempts must stop at 1 once suspended"
        );
    }

    #[test]
    fn test_deterministic_failure_logs_exactly_once() {
        // The actionable "suspended" line is emitted exactly once (on the
        // transition into the suspended state), asserted via the emit counter.
        let suspension = PersistSuspension::default();
        for _ in 0..1000 {
            let _ = suspension.attempt("string interner", || -> crate::core::error::Result<()> {
                Err(capacity_error())
            });
        }
        assert_eq!(
            suspension.log_emits(),
            1,
            "actionable log must fire exactly once"
        );
    }

    #[test]
    fn test_transient_failure_does_not_suspend() {
        // A transient I/O failure keeps the index attemptable on the normal
        // cadence — it must NOT latch the suspension.
        let suspension = PersistSuspension::default();
        for _ in 0..5 {
            assert_eq!(
                suspension.attempt("graph index", || -> crate::core::error::Result<()> {
                    Err(Error::Storage(StorageError::PersistenceError(
                        "Failed to save graph index: No space left on device".to_string(),
                    )))
                }),
                Some(false)
            );
        }
        assert!(!suspension.is_suspended());
        assert_eq!(
            suspension.attempts(),
            5,
            "transient failures keep attempting"
        );
        assert_eq!(
            suspension.log_emits(),
            0,
            "transient failures do not emit the suspend line"
        );
    }

    #[test]
    fn test_success_keeps_index_attemptable() {
        let suspension = PersistSuspension::default();
        assert_eq!(
            suspension.attempt("graph index", || -> crate::core::error::Result<u64> {
                Ok(42)
            }),
            Some(true)
        );
        assert!(!suspension.is_suspended());
        assert_eq!(suspension.attempts(), 1);
    }

    /// End-to-end regression (finding 8, the actual "egregore hang" trigger):
    /// drive the REAL persist pipeline (`persist_indexes` →
    /// `persist_graph_index_from_snapshot`, which interns property VALUES at
    /// persist time) at a tiny cap so a genuine high-cardinality value load
    /// overflows the interner — then assert the real `PersistSuspension` latch
    /// suspends after exactly one attempt (no spin, one log). NOT a synthetic
    /// injected error: the failure originates in real value interning.
    ///
    /// It lowers the process-global interner cap, so it runs in a SUBPROCESS.
    #[test]
    fn real_value_path_capacity_suspends_persistence_via_subprocess() {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let exe = std::env::current_exe().expect("failed to locate current test binary");
        let mut child = Command::new(exe)
            .args([
                "--ignored",
                "--exact",
                "storage::index_persistence::worker::tests::\
                 real_value_path_capacity_suspends_persistence_helper",
            ])
            .spawn()
            .expect("failed to spawn subprocess for real-value-path suspension test");

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(status.success(), "real-value-path suspension helper failed");
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("real-value-path suspension helper did not complete");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("failed while polling subprocess: {e}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn real_value_path_capacity_suspends_persistence_helper() {
        use crate::AletheiaDB;
        use crate::core::GLOBAL_INTERNER;
        use crate::core::property::PropertyMapBuilder;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db = AletheiaDB::open(dir.path()).expect("durable open");

        // Write nodes carrying DISTINCT string property VALUES. Values are
        // interned LAZILY at persist time (not the write path), so these writes
        // succeed regardless of the cap — exactly the "egregore hang" setup where
        // the writer sees no error and the persist thread later chokes.
        for i in 0..50 {
            db.create_node(
                "ValNode",
                PropertyMapBuilder::new()
                    .insert("v", format!("distinct_value_{i}"))
                    .build(),
            )
            .unwrap();
        }

        // Pin the cap just above the current interner size so persisting the
        // high-cardinality VALUES overflows partway through. This runs well within
        // the background thread's initial 1s sleep, so our manual persist is the
        // one that trips the cap (the background thread, if it later fires, only
        // suspends ITS own independent latch).
        let base = GLOBAL_INTERNER.len();
        GLOBAL_INTERNER.set_max_capacity(base + 10);

        let suspension = PersistSuspension::default();

        // First REAL persist: value interning trips a typed CapacityExceeded,
        // which the structured classifier treats as deterministic → suspend after
        // exactly one attempt.
        let r1 = suspension.attempt("graph index", || db.persist_indexes());
        assert_eq!(
            r1,
            Some(false),
            "the real persist must fail on the value-interning capacity breach"
        );
        assert!(
            suspension.is_suspended(),
            "a real value-path capacity breach must suspend background persistence"
        );
        assert_eq!(suspension.attempts(), 1);

        // Many further cycles are all SKIPPED (no spin): attempts stay at 1.
        for _ in 0..200 {
            assert_eq!(
                suspension.attempt("graph index", || db.persist_indexes()),
                None
            );
        }
        assert_eq!(
            suspension.attempts(),
            1,
            "a suspended index must make no further attempts (no CPU-pegging spin)"
        );
        assert_eq!(
            suspension.log_emits(),
            1,
            "the actionable suspend line must fire exactly once"
        );
    }
}
