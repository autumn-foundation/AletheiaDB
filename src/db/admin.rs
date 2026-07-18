//! Administrative and test-only helper methods.
//!
//! Provides internal statistics, test visibility, and database maintenance operations.
use crate::core::error::{PersistenceErrorKind, Result, ResultExt, StorageError};
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::index::temporal::TemporalIndexes;
use crate::query::planner::Statistics;
#[cfg(test)]
use crate::storage::current::CurrentStorage;
use crate::storage::historical::{HistoricalStats, HistoricalStorage};
use crate::storage::index_persistence::operations::{
    persist_graph_index_from_snapshot, persist_temporal_adjacency_index,
    persist_temporal_index_from_snapshot, persist_vector_indexes,
};
use parking_lot::RwLock;
use std::sync::Arc;

impl AletheiaDB {
    /// Get statistics about the historical storage.
    #[must_use = "the historical statistics value must be used"]
    pub fn historical_stats(&self) -> Result<HistoricalStats> {
        Ok(self.historical.read().stats())
    }

    /// Persist all indexes to disk.
    ///
    /// This saves the current state of all indexes (graph, temporal, vector, strings)
    /// to disk in the configured persistence directory.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Index persistence is not enabled in configuration
    /// - Writing index files fails due to I/O errors
    /// - Index serialization fails
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// // ... add data ...
    /// db.persist_indexes()?; // Save indexes to disk
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn persist_indexes(&self) -> Result<()> {
        let result = (|| {
            use crate::storage::index_persistence::formats::IndexManifest;

            let manager = self.persistence_manager.as_ref().ok_or_else(|| {
                StorageError::InconsistentState {
                    reason: "Index persistence not enabled".to_string(),
                }
            })?;

            // Fail-closed guard for the QUIESCED post-`enable_encryption` handle
            // (Issue #3616 PR3). After `enable_encryption` the live WAL is encrypted
            // but this handle's index manager still carries a PLAINTEXT keyring
            // (there is no live `None → Some` index-keyring install — see the loud
            // reopen contract on `enable_encryption`). Persisting now would write
            // PLAINTEXT index files OVER the freshly-wrapped `AEIX` snapshot — the
            // exact corruption the enable engine exists to prevent. Refuse loudly and
            // direct the caller to reopen; the resume path (which DOES build the
            // manager under the enable index DEK, so its keyring is `Some`) is
            // unaffected, as is a normally-encrypted reopen.
            if self.wal.is_encrypted() && manager.keyring().is_none() {
                return Err(crate::core::error::Error::FailedPrecondition(
                    "cannot persist indexes on a post-enable quiesced handle: the WAL is \
                     encrypted but this handle's index manager is still plaintext, so a \
                     persist would write plaintext index files over the encrypted (AEIX) \
                     snapshot. You MUST reopen the database (drop this handle and call \
                     AletheiaDB::open) to get a fully-encrypted, persistable instance."
                        .to_string(),
                ));
            }

            // Fail-closed guard for the QUIESCED post-`disable_encryption` handle
            // (Issue #3616 PR4) — the mirror of the enable guard above. After
            // `disable_encryption` the live WAL is plaintext (keyring uninstalled)
            // but this handle's index manager still carries the ENCRYPTED keyring
            // (there is no live `Some → None` index-keyring uninstall — see the loud
            // reopen contract on `disable_encryption`). Persisting now would write
            // `AEIX` index files OVER the freshly-unwrapped plaintext snapshot — the
            // exact corruption the disable engine exists to prevent.
            //
            // Unlike the enable guard, the raw state (WAL plaintext + index keyring
            // `Some`) is NOT unique to a post-disable handle: an index-only-encrypted
            // database (a plaintext WAL over an encrypted index — the sole config in
            // which an index-only key rotation is safe) has the identical shape and
            // MUST keep persisting normally. So this rare ambiguous branch is
            // disambiguated by TWO durable signals, EITHER of which marks a
            // post-disable quiesced handle:
            //
            //   (a) the authority records `disabled` — the terminal state a
            //       SUCCESSFUL `disable_encryption` flips it to; and
            //   (b) a pending `direction=disable` rotation ledger is present — the
            //       breadcrumb a disable writes BEFORE it flips the authority.
            //
            // Signal (b) closes a real gap: a disable that FAILS at the cold-unwrap
            // step (index already unwrapped to plaintext, WAL already plaintext, but
            // the authority not yet flipped) returns Err leaving WAL plaintext +
            // keyring `Some` + authority STILL `enabled` + a pending disable ledger.
            // On that errored handle, gating on the authority alone would read
            // `enabled` and WRONGLY allow a persist that rewrites `AEIX` over the
            // already-plaintext index snapshot. Also firing on the disable ledger
            // fails closed in that window. This does NOT reintroduce the
            // index-only-encrypted false positive: that config runs an index key
            // ROTATION whose ledger is `direction=rotate/enable`, and
            // `read_disable_ledger` filters to `direction=disable` only (returning
            // `None` for a rotation/enable ledger or no ledger), so the legitimate
            // index-only case still persists normally. The extra reads happen ONLY in
            // this branch (never entered by a normal plaintext DB — keyring `None`, or
            // a normal encrypted DB — WAL encrypted), so they add no hot-path cost.
            if !self.wal.is_encrypted()
                && manager.keyring().is_some()
                && (crate::db::encryption_state::read_encryption_state(manager.base_path())?
                    .is_some_and(|state| !state.enabled)
                    || crate::db::rotation::read_disable_ledger(manager)?.is_some())
            {
                return Err(crate::core::error::Error::FailedPrecondition(
                    "cannot persist indexes on a post-disable quiesced handle: the WAL is \
                     plaintext but this handle's index manager is still encrypted, so a \
                     persist would write encrypted (AEIX) index files over the plaintext \
                     snapshot. You MUST reopen the database (drop this handle and call \
                     AletheiaDB::open) to get a fully-plaintext, persistable instance."
                        .to_string(),
                ));
            }

            // Warn if background persistence thread has stopped
            if self
                .persistence_thread_stopped
                .load(std::sync::atomic::Ordering::Acquire)
            {
                eprintln!(
                    "Warning: Background persistence thread has stopped. \
                     Automatic persistence is disabled. Manual persist_indexes() calls will still work."
                );
            }

            let tracker = self.persistence_tracker.as_ref();

            // ── Coherence barrier (lost-write persist race fix) ─────────────────
            //
            // The manifest LSN is the APPLIED WATERMARK: the minimum LSN of any
            // commit that is durable (fsynced) but not yet applied. Stamping the
            // manifest with this — rather than the WAL allocation frontier —
            // guarantees replay re-covers any durable-but-unapplied write instead
            // of dropping it. When nothing is in flight it equals today's frontier
            // (idle persist keeps the identical manifest LSN). Re-applying a write
            // that DID make it into the snapshot is safe: replay is idempotent
            // (keyed by version_id).
            //
            // We read the watermark under a BRIEF `current_timestamp` hold
            // (lock-order class 1, acquired FIRST), then RELEASE it before the two
            // O(N) snapshot clones. This closes a narrow window: a commit's WAL LSN
            // band is allocated (bumping the frontier) INSIDE `append_batch`, but
            // its in-flight registration happens just AFTER the append returns —
            // both under the commit's own `current_timestamp` hold. Without taking
            // `current_timestamp` here, we could read a frontier already advanced
            // past such a commit while `in_flight.min()` does not yet see it, so the
            // manifest would sit ABOVE that soon-to-be-durable write and replay
            // would drop it. Holding `current_timestamp` only for this O(log n) read
            // guarantees the frontier and the in-flight set are mutually consistent,
            // WITHOUT stalling every writer (class 1 is the top of every commit)
            // across the deep clones below.
            //
            // CORRECTNESS INVARIANT (why releasing before the clone is safe): with
            // T0 = the instant of this watermark read and T1 > T0 the instant of the
            // snapshot clone, `manifest_lsn = min(in-flight bases at T0)` and
            // `manifest_lsn <= frontier(T0) <= any later LSN allocation`. Every write
            // with `lsn < manifest_lsn` was NOT in-flight at T0 (min is the smallest
            // in-flight), hence already applied at T0, hence present in the coherent
            // snapshot taken at T1 (storage is monotonic; the snapshot is coherent
            // under `historical.read()`). Every durable write NOT in the snapshot has
            // `lsn >= manifest_lsn` and is re-applied by inclusive replay (idempotent
            // by version_id). So releasing `current_timestamp` before the clone does
            // NOT reintroduce a lost write, and snapshot entries with
            // `lsn >= manifest_lsn` are harmless (replay is idempotent). We do NOT
            // re-read the frontier or `in_flight.min()` after releasing the lock.
            let manifest_lsn = {
                let _ts =
                    self.current_timestamp
                        .lock()
                        .map_err(|_| StorageError::InconsistentState {
                            reason: "current_timestamp lock poisoned during persist_indexes"
                                .to_string(),
                        })?;
                let frontier = self.wal.current_lsn().0;
                self.in_flight.min().unwrap_or(frontier)
            }; // current_timestamp released here — clones run WITHOUT the class-1 lock

            // Now take the coherent in-memory snapshot WITHOUT holding
            // `current_timestamp`, so the graph and temporal indexes cannot observe
            // different instants (the pre-fix torn snapshot could restore e.g. 85
            // graph nodes against 86 temporal versions). We mirror the PROVEN
            // backup.rs lock order EXACTLY — `historical.read()` THEN
            // `current.snapshot_lock.write()` — which is consistent with the commit
            // path's `historical.write()` → `snapshot_lock.read()` ordering, so no
            // AB-BA inversion. Locks are released BEFORE any disk I/O; serialization
            // below runs OFF-LOCK from the immutable snapshots, like checkpoint /
            // backup.
            let hist = self.historical.read();
            let current_snapshot = {
                let _snap_lock = self.current.snapshot_lock.write();
                self.current
                    .create_snapshot(crate::storage::wal::LSN(manifest_lsn))
            };
            let historical_snapshot = hist.create_snapshot(crate::storage::wal::LSN(manifest_lsn));
            drop(hist);

            // String interner must be saved first (dependency for all others).
            // Update the string LSN tracker to manifest_lsn BEFORE calculating
            // safe LSN so even if no new strings were added, the tracker reflects
            // the watermark.
            if let Some(tracker) = tracker {
                crate::storage::index_persistence::operations::persist_string_interner(
                    manager,
                    tracker,
                    manifest_lsn,
                )?;
            } else {
                manager.save_string_interner().map_err(|e| {
                    StorageError::persistence_with_kind(
                        PersistenceErrorKind::from(&e),
                        format!("Failed to save string interner: {}", e),
                    )
                })?;
            }

            // Graph index from the COHERENT current snapshot (off-lock).
            persist_graph_index_from_snapshot(&current_snapshot, manager, tracker, manifest_lsn)?;

            if let Some(tracker) = tracker {
                // NOTE (v1 scope, F7 follow-up): vector indexes are persisted from
                // LIVE current storage here — AFTER the coherence barrier above was
                // released — rather than from the coherent snapshot. Consequently a
                // graph-vs-vector torn snapshot is possible on recovery. This is NOT
                // a lost write: replay re-indexes every entry with lsn >= manifest_lsn,
                // so any node missing from the vector file is re-covered. But an entry
                // that IS in the vector file AND also replayed can get a double HNSW
                // insert. The reported torn-snapshot symptom is graph-vs-temporal
                // (both fixed above); a snapshot-coherent vector persist (snapshot the
                // vectors under the same barrier, or gate loaded vector entries by
                // lsn <= manifest on restore) is tracked follow-up F7. See design doc.
                persist_vector_indexes(&self.current, manager, Some(tracker), manifest_lsn)?;
                // Temporal index from the COHERENT historical snapshot (off-lock).
                persist_temporal_index_from_snapshot(
                    &historical_snapshot,
                    manager,
                    tracker,
                    manifest_lsn,
                )?;
            }

            // Persist the temporal-adjacency index too, so a manual force-persist
            // (e.g. re-encrypting a dataset after enabling encryption) rewrites
            // EVERY on-disk index file — matching the shutdown path
            // `persist_all_indexes`. Without this, `temporal_adjacency/adjacency.idx`
            // could remain plaintext after a manual re-encrypt (Issue #481).
            //
            // Persisted from LIVE historical storage AFTER the coherence barrier was
            // released (same treatment as `persist_vector_indexes` above): the
            // temporal-adjacency index shares the vector index's documented
            // torn-snapshot follow-up (F7). This is NOT a lost write — replay
            // reconstructs adjacency for every entry with lsn >= manifest_lsn, so the
            // on-disk file is re-covered on restore.
            persist_temporal_adjacency_index(&self.historical, manager)?;

            // Record WAL position for future replay coordination. Safe LSN = min
            // of all components; since every component was just persisted at
            // `manifest_lsn`, this resolves to the applied watermark.
            let safe_lsn = tracker
                .map(|t| t.get_safe_manifest_lsn())
                .unwrap_or(manifest_lsn);

            let manifest = IndexManifest::new(safe_lsn);
            manager.save_manifest(&manifest).map_err(|e| {
                StorageError::persistence_with_kind(
                    PersistenceErrorKind::from(&e),
                    format!("Failed to save manifest: {}", e),
                )
            })?;

            Ok(())
        })();
        result.record_error_metric()
    }

    /// Get a reference to the current storage (test-only helper).
    ///
    /// This method is only available in test builds and provides access to the
    /// internal CurrentStorage for integration test verification purposes.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn storage(&self) -> &Arc<CurrentStorage> {
        &self.current
    }

    /// Get the current WAL LSN (test-only helper).
    ///
    /// This method provides access to the current WAL Log Sequence Number for
    /// test verification purposes. This is particularly useful for testing index
    /// persistence where LSN coordination with the WAL is critical for correctness.
    ///
    /// **Warning**: This method exposes internal implementation details and
    /// should only be used in tests.
    ///
    /// # Returns
    ///
    /// The current LSN from the WAL system.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMap};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// # let properties = PropertyMap::new();
    /// db.create_node("Person", properties)?;
    /// let lsn = db.__test_current_wal_lsn();
    /// assert!(lsn > 0); // LSN advances after operations
    /// # Ok(())
    /// # }
    /// ```
    #[doc(hidden)]
    pub fn __test_current_wal_lsn(&self) -> u64 {
        self.wal.current_lsn().0
    }

    /// Get the current transaction timestamp (test-only helper).
    ///
    /// This method provides access to the internal transaction clock for
    /// integration test verification purposes.
    #[doc(hidden)]
    pub fn __test_current_timestamp(&self) -> Timestamp {
        *self.current_timestamp.lock().unwrap()
    }

    /// Access the internal HistoricalStorage for testing purposes.
    ///
    /// This method provides access to the internal HistoricalStorage for
    /// integration test verification purposes. It is public to allow access from
    /// integration tests but is hidden from documentation and marked with
    /// `__test_` prefix to discourage production use.
    ///
    /// **Warning**: This method exposes internal implementation details and
    /// should only be used in tests.
    #[doc(hidden)]
    pub fn __test_historical_storage(&self) -> &Arc<RwLock<HistoricalStorage>> {
        &self.historical
    }

    /// Provide test-only access to temporal indexes for performance testing.
    ///
    /// This allows tests to verify that temporal indexes are populated correctly
    /// and can query them directly. This is marked as `#[doc(hidden)]` and
    /// should only be used in tests.
    #[doc(hidden)]
    pub fn __test_temporal_indexes(&self) -> &Arc<TemporalIndexes> {
        &self.temporal_indexes
    }

    /// Get adaptive over-fetch statistics for a label (test-only helper).
    ///
    /// Returns the current statistics (search_count, total_candidates, total_results)
    /// for the given label, or None if no searches have been performed yet.
    ///
    /// This is used for testing to verify that adaptive learning is working correctly.
    ///
    /// **Warning**: This method exposes internal implementation details and
    /// should only be used in tests.
    ///
    /// # Returns
    ///
    /// Some((search_count, total_candidates, total_results)) if statistics exist,
    /// None otherwise.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::index::vector::HnswConfig;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// # let config = HnswConfig::default();
    /// db.vector_index("embedding").hnsw(config).enable()?;
    /// // ... create nodes and perform searches ...
    /// let (count, candidates, results) = db.__test_get_filter_stats("Person").unwrap();
    /// assert_eq!(count, 10); // 10 searches performed
    /// # Ok(())
    /// # }
    /// ```
    #[doc(hidden)]
    pub fn __test_get_filter_stats(&self, label: &str) -> Option<(u64, u64, u64)> {
        self.current.get_filter_stats(label)
    }

    /// Get the query optimization statistics.
    ///
    /// Statistics are used for cost-based query optimization and are cached
    /// across queries for efficiency. The statistics are automatically refreshed
    /// when needed, but can be manually refreshed using [`refresh_statistics`](Self::refresh_statistics).
    ///
    /// # Returns
    ///
    /// A reference to the shared statistics object.
    pub fn statistics(&self) -> &Arc<Statistics> {
        &self.stats
    }

    /// Refresh query optimization statistics from current storage.
    ///
    /// This collects fresh statistics about node counts, edge counts, label
    /// cardinalities, and other metrics used for cost-based query optimization.
    /// Call this method after significant schema changes or data modifications
    /// to ensure the query planner has accurate information.
    ///
    /// Statistics are automatically refreshed lazily on first query, so this
    /// method is typically only needed for benchmarking or after bulk imports.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMap};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let documents: Vec<PropertyMap> = vec![];
    /// // After bulk import
    /// for props in documents {
    ///     db.create_node("Document", props)?;
    /// }
    ///
    /// // Refresh statistics for optimal query planning
    /// db.refresh_statistics();
    ///
    /// // Now queries will use accurate statistics
    /// # let query = db.query().build();
    /// let results = db.execute_query(query)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn refresh_statistics(&self) {
        // Collect statistics from current storage
        let node_count = self.current.node_count();
        let edge_count = self.current.edge_count();
        let vector_count = self.current.vector_count();

        // Collect label counts from current storage
        let label_counts = self.current.label_counts();

        // Calculate the average delta chain length from historical storage
        // (Issue #366). This feeds the query planner's temporal-lookup cost
        // model. O(1): reads incrementally-maintained anchor/delta counters
        // (Issue #212) and falls back to the default estimate of 5.0 when the
        // historical storage is empty.
        let avg_delta_chain = self.historical.read().calculate_avg_delta_chain();

        self.stats.refresh(
            node_count,
            edge_count,
            vector_count,
            label_counts,
            avg_delta_chain,
        );
    }

    /// Invalidate cached query optimization statistics.
    ///
    /// Call this after schema changes to force re-collection of statistics
    /// on the next query. The statistics will be lazily refreshed when needed.
    pub fn invalidate_statistics(&self) {
        self.stats.invalidate();
    }
}
