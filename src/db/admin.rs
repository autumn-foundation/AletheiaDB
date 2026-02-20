use crate::api::transaction::visibility::CompressionStats;
use crate::core::error::{Result, StorageError};
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::index::temporal::TemporalIndexes;
use crate::query::planner::Statistics;
#[cfg(test)]
use crate::storage::current::CurrentStorage;
use crate::storage::historical::{HistoricalStats, HistoricalStorage};
use crate::storage::index_persistence::operations::{
    persist_temporal_index, persist_vector_indexes,
};
use parking_lot::RwLock;
use std::sync::Arc;

impl AletheiaDB {
    /// Get statistics about the historical storage.
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
    /// ```ignore
    /// let db = AletheiaDB::new();
    /// // ... add data ...
    /// db.persist_indexes()?; // Save indexes to disk
    /// ```
    pub fn persist_indexes(&self) -> Result<()> {
        use crate::storage::index_persistence::formats::IndexManifest;

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

        let manager =
            self.persistence_manager
                .as_ref()
                .ok_or_else(|| StorageError::InconsistentState {
                    reason: "Index persistence not enabled".to_string(),
                })?;

        // Capture current LSN for all operations
        let current_lsn = self.wal.current_lsn().0;

        // 1. Save string interner first (dependency for all others)
        if let Some(ref tracker) = self.persistence_tracker {
            crate::storage::index_persistence::operations::persist_string_interner(
                manager,
                tracker,
                current_lsn,
            )?;
        } else {
            manager.save_string_interner().map_err(|e| {
                StorageError::PersistenceError(format!("Failed to save string interner: {}", e))
            })?;
        }

        // 2. Save graph index
        crate::storage::index_persistence::operations::persist_graph_index(
            &self.current,
            manager,
            self.persistence_tracker.as_ref(),
            current_lsn,
        )?;

        // 3. Save vector indexes
        if let Some(ref tracker) = self.persistence_tracker {
            persist_vector_indexes(&self.current, manager, Some(tracker), current_lsn)?;
        }

        // 4. Save temporal index (version history)
        if let Some(ref tracker) = self.persistence_tracker {
            persist_temporal_index(
                &self.historical,
                &self.temporal_indexes,
                manager,
                tracker,
                current_lsn,
            )?;
        }

        // 5. Save manifest last with SAFE LSN
        // Note: This records the WAL position at persist time for future WAL replay coordination.
        // We use the safe LSN (min of all components) if tracker is available, or current LSN if not.
        // Since we just persisted everything successfully above, current_lsn is safe (and equal to min).
        let safe_lsn = if let Some(ref tracker) = self.persistence_tracker {
            tracker.get_safe_manifest_lsn()
        } else {
            current_lsn
        };

        let manifest = IndexManifest::new(safe_lsn);
        manager.save_manifest(&manifest).map_err(|e| {
            StorageError::PersistenceError(format!("Failed to save manifest: {}", e))
        })?;

        Ok(())
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
    /// ```ignore
    /// let db = AletheiaDB::new();
    /// db.create_node("Person", properties)?;
    /// let lsn = db.__test_current_wal_lsn();
    /// assert!(lsn > 0); // LSN advances after operations
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
    /// ```ignore
    /// let db = AletheiaDB::new()?;
    /// db.enable_vector_index("embedding", config)?;
    /// // ... create nodes and perform searches ...
    /// let (count, candidates, results) = db.__test_get_filter_stats("Person").unwrap();
    /// assert_eq!(count, 10); // 10 searches performed
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
    /// ```ignore
    /// // After bulk import
    /// for doc in documents {
    ///     db.create_node("Document", doc.properties)?;
    /// }
    ///
    /// // Refresh statistics for optimal query planning
    /// db.refresh_statistics();
    ///
    /// // Now queries will use accurate statistics
    /// let results = db.execute_query(query)?;
    /// ```
    pub fn refresh_statistics(&self) {
        // Collect statistics from current storage
        let node_count = self.current.node_count();
        let edge_count = self.current.edge_count();
        let vector_count = self.current.vector_count();

        // Collect label counts from current storage
        let label_counts = self.current.label_counts();

        // Calculate average delta chain length from historical storage
        // (using default estimate if historical storage is empty)
        // See issue #366: Calculate from historical storage instead of hardcoding
        let avg_delta_chain = 5.0;

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

    /// Compress the MVCC commit log to reduce memory usage (Issue #237).
    ///
    /// This applies epoch-based compression to the transaction visibility manager's
    /// commit log, converting sequential transaction ranges into compressed epochs.
    /// Achieves 10-100x memory reduction for typical workloads with sequential
    /// transaction patterns.
    ///
    /// # When to Call
    ///
    /// - Periodically during bulk imports (e.g., every 10K commits)
    /// - During idle periods
    /// - Before checkpointing
    ///
    /// # Performance
    ///
    /// O(N log N) where N is the number of uncompressed transactions.
    /// Relatively expensive, so should not be called on every commit.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // After bulk import
    /// for i in 0..100000 {
    ///     db.create_node("Node", props)?;
    /// }
    ///
    /// // Compress commit log to free memory
    /// db.compress_commit_log();
    /// ```
    pub fn compress_commit_log(&self) {
        self.visibility_manager.compress_commit_log();
    }

    /// Get memory usage of the MVCC commit log in bytes.
    ///
    /// This reports the current memory footprint of the transaction commit log
    /// in the visibility manager. Useful for monitoring and triggering compression.
    ///
    /// # Returns
    ///
    /// Memory usage in bytes
    pub fn commit_log_memory_usage(&self) -> usize {
        self.visibility_manager.commit_log_memory_usage()
    }

    /// Get detailed compression statistics for the MVCC commit log (Issue #237).
    ///
    /// Returns statistics about:
    /// - Total transactions tracked
    /// - Number of compressed epochs
    /// - Number of exception entries
    /// - Compression ratio
    /// - Memory usage and savings
    ///
    /// # Returns
    ///
    /// Compression statistics
    ///
    /// # Example
    ///
    /// ```ignore
    /// let stats = db.get_compression_stats();
    /// println!("Compression ratio: {}x", stats.compression_ratio);
    /// println!("Memory saved: {} bytes", stats.memory_saved_bytes);
    /// ```
    pub fn get_compression_stats(&self) -> CompressionStats {
        self.visibility_manager.get_compression_stats()
    }

    /// Check if commit log compression should be triggered based on memory threshold.
    ///
    /// This is a convenience method to help decide when to call `compress_commit_log()`.
    /// Returns `true` if the current commit log memory usage exceeds the threshold.
    ///
    /// # Arguments
    ///
    /// * `threshold_bytes` - Memory threshold in bytes
    ///
    /// # Returns
    ///
    /// `true` if compression is recommended, `false` otherwise
    ///
    /// # Example
    ///
    /// ```ignore
    /// // After bulk import, compress if using > 10MB
    /// if db.should_compress_commit_log(10 * 1024 * 1024) {
    ///     db.compress_commit_log();
    /// }
    /// ```
    pub fn should_compress_commit_log(&self, threshold_bytes: usize) -> bool {
        self.visibility_manager
            .should_compress_commit_log(threshold_bytes)
    }

    /// Check if compression should be triggered based on exception count.
    ///
    /// This is an alternative trigger mechanism that compresses when there are
    /// many uncompressed exceptions (indicating potential for compression).
    ///
    /// # Arguments
    ///
    /// * `threshold_exceptions` - Number of exceptions to trigger compression
    ///
    /// # Returns
    ///
    /// `true` if compression is recommended, `false` otherwise
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Compress every 50K commits
    /// if db.should_compress_by_exception_count(50_000) {
    ///     db.compress_commit_log();
    /// }
    /// ```
    pub fn should_compress_by_exception_count(&self, threshold_exceptions: usize) -> bool {
        self.visibility_manager
            .should_compress_by_exception_count(threshold_exceptions)
    }
}
