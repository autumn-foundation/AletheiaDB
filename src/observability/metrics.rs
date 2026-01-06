//! Production metrics for observability.
//!
//! This module provides lock-free, low-overhead metrics tracking using atomic counters.
//! Metrics are collected only when the `observability` feature is enabled, providing
//! zero runtime cost when disabled.
//!
//! # Critical Metrics
//!
//! - **Lock poisoning**: Thread panicked while holding a lock (data corruption risk)
//! - **Timestamp violations**: Transaction time monotonicity broken (MVCC invariant violated)
//! - **WAL checksum failures**: Durability log corrupted (data loss risk)
//! - **Write conflicts**: Snapshot Isolation write-write conflicts (normal but should be monitored)
//!
//! # Usage
//!
//! ```ignore
//! use gallifreydb::observability::METRICS;
//! use std::sync::atomic::Ordering;
//!
//! // Increment metric
//! METRICS.lock_poison_count.fetch_add(1, Ordering::Relaxed);
//!
//! // Get snapshot
//! let snapshot = METRICS.snapshot();
//! println!("Lock poisons: {}", snapshot.lock_poison_count);
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

/// Global metrics singleton.
///
/// This is a static instance that can be accessed from anywhere in the codebase.
/// Atomic operations use `Relaxed` ordering for minimal overhead (<10ns per increment).
pub static METRICS: Metrics = Metrics::new();

/// Production metrics collected via atomic counters.
///
/// All counters use `AtomicU64` with `Relaxed` ordering for lock-free,
/// cache-friendly updates. These metrics are designed for high-frequency
/// updates with minimal performance impact.
pub struct Metrics {
    /// Number of lock poisoning events detected.
    ///
    /// **CRITICAL**: This should NEVER be >0 in production. A poisoned lock
    /// indicates a thread panicked while holding the lock, potentially leaving
    /// protected data in an inconsistent state.
    ///
    /// **Action**: Immediate investigation required. Check logs for panic stack traces.
    pub lock_poison_count: AtomicU64,

    /// Number of timestamp monotonicity violations.
    ///
    /// **CRITICAL**: This should NEVER be >0. Violates MVCC invariants and can
    /// cause read anomalies (seeing data from the future).
    ///
    /// **Action**: This indicates a serious bug in transaction timestamp management.
    pub timestamp_violations: AtomicU64,

    /// Number of WAL checksum failures detected.
    ///
    /// **CRITICAL**: This should NEVER be >0. Indicates corruption in the
    /// write-ahead log, which means durability guarantees are violated.
    ///
    /// **Action**: Immediate investigation. May indicate disk corruption, hardware
    /// failure, or software bug. Do NOT continue operating on corrupted WAL.
    pub wal_checksum_failures: AtomicU64,

    /// Number of write-write conflicts (Snapshot Isolation).
    ///
    /// **HIGH**: This is expected during normal operation when concurrent
    /// transactions modify the same data, but high rates indicate contention hotspots.
    ///
    /// **Action**: If rate >10/sec, investigate application access patterns.
    /// Consider optimistic locking, application-level batching, or sharding.
    pub write_conflicts: AtomicU64,

    // Error categorization counters (Phase 3)
    /// Total number of storage-related errors.
    ///
    /// Includes: NodeNotFound, EdgeNotFound, InvalidProperty, WalError, etc.
    pub error_storage_total: AtomicU64,

    /// Total number of temporal constraint violations.
    ///
    /// Includes: InvalidTimeRange, TemporalParadox, VersionChain corruption, etc.
    pub error_temporal_total: AtomicU64,

    /// Total number of query-related errors.
    ///
    /// Includes: SyntaxError, InvalidParameter, Timeout, LimitExceeded, etc.
    pub error_query_total: AtomicU64,

    /// Total number of transaction errors.
    ///
    /// Includes: InvalidState, AlreadyCommitted, ValidationFailed, CommitFailed, etc.
    pub error_transaction_total: AtomicU64,

    /// Total number of vector-related errors.
    ///
    /// Includes: DimensionMismatch, ContainsNaN, InvalidVector, etc.
    pub error_vector_total: AtomicU64,

    /// Total number of I/O errors.
    pub error_io_total: AtomicU64,

    /// Total number of other/miscellaneous errors.
    pub error_other_total: AtomicU64,
}

impl Metrics {
    /// Create a new metrics instance with all counters initialized to zero.
    ///
    /// This is a `const fn` to allow static initialization.
    pub const fn new() -> Self {
        Self {
            lock_poison_count: AtomicU64::new(0),
            timestamp_violations: AtomicU64::new(0),
            wal_checksum_failures: AtomicU64::new(0),
            write_conflicts: AtomicU64::new(0),
            error_storage_total: AtomicU64::new(0),
            error_temporal_total: AtomicU64::new(0),
            error_query_total: AtomicU64::new(0),
            error_transaction_total: AtomicU64::new(0),
            error_vector_total: AtomicU64::new(0),
            error_io_total: AtomicU64::new(0),
            error_other_total: AtomicU64::new(0),
        }
    }

    /// Capture a point-in-time snapshot of all metrics.
    ///
    /// This uses `Relaxed` ordering, so the snapshot may not be perfectly
    /// consistent across all counters, but this is acceptable for monitoring.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let snapshot = METRICS.snapshot();
    /// if snapshot.lock_poison_count > 0 {
    ///     panic!("Data corruption detected!");
    /// }
    /// ```
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            lock_poison_count: self.lock_poison_count.load(Ordering::Relaxed),
            timestamp_violations: self.timestamp_violations.load(Ordering::Relaxed),
            wal_checksum_failures: self.wal_checksum_failures.load(Ordering::Relaxed),
            write_conflicts: self.write_conflicts.load(Ordering::Relaxed),
            error_storage_total: self.error_storage_total.load(Ordering::Relaxed),
            error_temporal_total: self.error_temporal_total.load(Ordering::Relaxed),
            error_query_total: self.error_query_total.load(Ordering::Relaxed),
            error_transaction_total: self.error_transaction_total.load(Ordering::Relaxed),
            error_vector_total: self.error_vector_total.load(Ordering::Relaxed),
            error_io_total: self.error_io_total.load(Ordering::Relaxed),
            error_other_total: self.error_other_total.load(Ordering::Relaxed),
        }
    }

    /// Reset all metrics to zero.
    ///
    /// **Warning**: This should typically NOT be called in production. Metrics
    /// should be monotonically increasing counters. This is primarily useful
    /// for testing.
    #[cfg(test)]
    pub fn reset(&self) {
        self.lock_poison_count.store(0, Ordering::Relaxed);
        self.timestamp_violations.store(0, Ordering::Relaxed);
        self.wal_checksum_failures.store(0, Ordering::Relaxed);
        self.write_conflicts.store(0, Ordering::Relaxed);
        self.error_storage_total.store(0, Ordering::Relaxed);
        self.error_temporal_total.store(0, Ordering::Relaxed);
        self.error_query_total.store(0, Ordering::Relaxed);
        self.error_transaction_total.store(0, Ordering::Relaxed);
        self.error_vector_total.store(0, Ordering::Relaxed);
        self.error_io_total.store(0, Ordering::Relaxed);
        self.error_other_total.store(0, Ordering::Relaxed);
    }
}

/// A point-in-time snapshot of all metrics.
///
/// This is a plain struct with owned values, suitable for serialization,
/// logging, or exporting to external monitoring systems (Prometheus, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricsSnapshot {
    /// Number of lock poisoning events detected.
    pub lock_poison_count: u64,

    /// Number of timestamp monotonicity violations.
    pub timestamp_violations: u64,

    /// Number of WAL checksum failures detected.
    pub wal_checksum_failures: u64,

    /// Number of write-write conflicts (Snapshot Isolation).
    pub write_conflicts: u64,

    /// Total storage errors.
    pub error_storage_total: u64,

    /// Total temporal errors.
    pub error_temporal_total: u64,

    /// Total query errors.
    pub error_query_total: u64,

    /// Total transaction errors.
    pub error_transaction_total: u64,

    /// Total vector errors.
    pub error_vector_total: u64,

    /// Total I/O errors.
    pub error_io_total: u64,

    /// Total other errors.
    pub error_other_total: u64,
}

impl MetricsSnapshot {
    /// Check if any critical metrics (corruption indicators) are non-zero.
    ///
    /// Returns `true` if lock poisons, timestamp violations, or WAL checksum
    /// failures have occurred. These should NEVER be non-zero in production.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let snapshot = METRICS.snapshot();
    /// if snapshot.has_critical_errors() {
    ///     panic!("CRITICAL: Data corruption detected - cannot continue safely");
    /// }
    /// ```
    pub fn has_critical_errors(&self) -> bool {
        self.lock_poison_count > 0
            || self.timestamp_violations > 0
            || self.wal_checksum_failures > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_metrics_initialization() {
        let metrics = Metrics::new();
        let snapshot = metrics.snapshot();

        assert_eq!(snapshot.lock_poison_count, 0);
        assert_eq!(snapshot.timestamp_violations, 0);
        assert_eq!(snapshot.wal_checksum_failures, 0);
        assert_eq!(snapshot.write_conflicts, 0);
    }

    #[test]
    fn test_metrics_increment() {
        let metrics = Metrics::new();

        metrics.lock_poison_count.fetch_add(1, Ordering::Relaxed);
        metrics.write_conflicts.fetch_add(5, Ordering::Relaxed);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.lock_poison_count, 1);
        assert_eq!(snapshot.write_conflicts, 5);
    }

    #[test]
    fn test_concurrent_increments() {
        let metrics = Arc::new(Metrics::new());
        let mut handles = vec![];

        // Spawn 10 threads, each incrementing 100 times
        for _ in 0..10 {
            let metrics_clone = Arc::clone(&metrics);
            let handle = thread::spawn(move || {
                for _ in 0..100 {
                    metrics_clone
                        .write_conflicts
                        .fetch_add(1, Ordering::Relaxed);
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Should have 10 * 100 = 1000 increments
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.write_conflicts, 1000);
    }

    #[test]
    fn test_has_critical_errors() {
        let metrics = Metrics::new();

        // No errors initially
        assert!(!metrics.snapshot().has_critical_errors());

        // Lock poison is critical
        metrics.lock_poison_count.fetch_add(1, Ordering::Relaxed);
        assert!(metrics.snapshot().has_critical_errors());

        metrics.reset();

        // Timestamp violation is critical
        metrics.timestamp_violations.fetch_add(1, Ordering::Relaxed);
        assert!(metrics.snapshot().has_critical_errors());

        metrics.reset();

        // WAL checksum failure is critical
        metrics
            .wal_checksum_failures
            .fetch_add(1, Ordering::Relaxed);
        assert!(metrics.snapshot().has_critical_errors());

        metrics.reset();

        // Write conflicts are NOT critical
        metrics.write_conflicts.fetch_add(100, Ordering::Relaxed);
        assert!(!metrics.snapshot().has_critical_errors());
    }

    #[test]
    fn test_global_metrics_singleton() {
        // Access global METRICS
        METRICS.lock_poison_count.fetch_add(1, Ordering::Relaxed);

        let snapshot = METRICS.snapshot();
        assert!(snapshot.lock_poison_count >= 1); // May be >1 if other tests ran

        // Note: Can't reset global singleton in tests, so we check >= instead of ==
    }
}
