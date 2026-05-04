//! Metrics contract for AletheiaDB.
//!
//! Metric names are stable public API. The built-in [`METRICS`] value keeps a
//! small atomic mirror for tests and local diagnostics; production export is
//! handled by an installed [`MetricsRecorder`] such as
//! [`metrics_rs_adapter::MetricsRsRecorder`](crate::observability::metrics_rs_adapter::MetricsRsRecorder).

use std::sync::atomic::{AtomicU64, Ordering};

const METRIC_READ_ORDERING: Ordering = Ordering::Acquire;
const METRIC_WRITE_ORDERING: Ordering = Ordering::Release;

/// Counter: errors by bounded category.
pub const METRIC_ERRORS: &str = "aletheiadb.errors";

/// Counter: write conflicts observed under snapshot isolation.
pub const METRIC_WRITE_CONFLICTS: &str = "aletheiadb.write_conflicts";

/// Counter: events that should be zero in healthy production systems.
pub const METRIC_CRITICAL_EVENTS: &str = "aletheiadb.critical_events";

/// Counter: transaction commits by durability mode and status.
pub const METRIC_TRANSACTION_COMMITS: &str = "aletheiadb.transaction.commits";

/// Histogram: transaction commit wall-clock duration in seconds.
pub const METRIC_TRANSACTION_COMMIT_DURATION: &str = "aletheiadb.transaction.commit.duration";

/// Histogram: operations included in a committed transaction.
pub const METRIC_TRANSACTION_OPERATIONS: &str = "aletheiadb.transaction.operations";

/// Metric label: bounded error category.
pub const METRIC_LABEL_CATEGORY: &str = "category";

/// Metric label: bounded operation status.
pub const METRIC_LABEL_STATUS: &str = "status";

/// Metric label: durability mode.
pub const METRIC_LABEL_DURABILITY_MODE: &str = "durability_mode";

/// Metric label: bounded critical event name.
pub const METRIC_LABEL_EVENT: &str = "event";

/// Global atomic metric mirror.
pub static METRICS: Metrics = Metrics::new();

/// Bounded error categories safe for metric labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Storage-layer error.
    Storage,
    /// Temporal validity or transaction-time error.
    Temporal,
    /// Query parsing, planning, or execution error.
    Query,
    /// Transaction lifecycle error.
    Transaction,
    /// Vector validation, indexing, or search error.
    Vector,
    /// I/O error.
    Io,
    /// Catch-all category for miscellaneous errors.
    Other,
}

impl ErrorCategory {
    /// Stable metric label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Temporal => "temporal",
            Self::Query => "query",
            Self::Transaction => "transaction",
            Self::Vector => "vector",
            Self::Io => "io",
            Self::Other => "other",
        }
    }
}

/// Critical event categories safe for metric labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriticalEvent {
    /// A thread panicked while holding a lock.
    LockPoison,
    /// Transaction timestamp monotonicity was violated.
    TimestampViolation,
    /// WAL checksum validation failed.
    WalChecksumFailure,
}

impl CriticalEvent {
    /// Stable metric label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LockPoison => "lock_poison",
            Self::TimestampViolation => "timestamp_violation",
            Self::WalChecksumFailure => "wal_checksum_failure",
        }
    }
}

/// Sink for AletheiaDB's standard metrics.
///
/// Implementations should preserve the metric names and label sets defined in
/// this module. All methods default to no-op bodies so applications can record
/// only the samples they need.
pub trait MetricsRecorder: Send + Sync {
    /// Record one error by bounded category.
    fn record_error(&self, category: ErrorCategory) {
        let _ = category;
    }

    /// Record one write conflict.
    fn record_write_conflict(&self) {}

    /// Record one critical event.
    fn record_critical_event(&self, event: CriticalEvent) {
        let _ = event;
    }

    /// Record a completed transaction commit.
    fn record_transaction_commit(
        &self,
        duration_secs: f64,
        operations_count: u64,
        durability_mode: &str,
        status: &str,
    ) {
        let _ = (duration_secs, operations_count, durability_mode, status);
    }
}

/// Default metrics recorder that discards every sample.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpMetrics;

impl MetricsRecorder for NoOpMetrics {}

/// Atomic metric mirror used for local diagnostics and existing tests.
pub struct Metrics {
    /// Number of lock poisoning events detected.
    pub lock_poison_count: AtomicU64,

    /// Number of timestamp monotonicity violations.
    pub timestamp_violations: AtomicU64,

    /// Number of WAL checksum failures detected.
    pub wal_checksum_failures: AtomicU64,

    /// Number of write-write conflicts.
    pub write_conflicts: AtomicU64,

    /// Total storage errors.
    pub error_storage_total: AtomicU64,

    /// Total temporal errors.
    pub error_temporal_total: AtomicU64,

    /// Total query errors.
    pub error_query_total: AtomicU64,

    /// Total transaction errors.
    pub error_transaction_total: AtomicU64,

    /// Total vector errors.
    pub error_vector_total: AtomicU64,

    /// Total I/O errors.
    pub error_io_total: AtomicU64,

    /// Total other errors.
    pub error_other_total: AtomicU64,

    /// Total transaction commits.
    pub transaction_commits_total: AtomicU64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// Create a zeroed metric mirror.
    #[must_use]
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
            transaction_commits_total: AtomicU64::new(0),
        }
    }

    /// Capture a point-in-time snapshot.
    #[must_use]
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            lock_poison_count: self.lock_poison_count.load(METRIC_READ_ORDERING),
            timestamp_violations: self.timestamp_violations.load(METRIC_READ_ORDERING),
            wal_checksum_failures: self.wal_checksum_failures.load(METRIC_READ_ORDERING),
            write_conflicts: self.write_conflicts.load(METRIC_READ_ORDERING),
            error_storage_total: self.error_storage_total.load(METRIC_READ_ORDERING),
            error_temporal_total: self.error_temporal_total.load(METRIC_READ_ORDERING),
            error_query_total: self.error_query_total.load(METRIC_READ_ORDERING),
            error_transaction_total: self.error_transaction_total.load(METRIC_READ_ORDERING),
            error_vector_total: self.error_vector_total.load(METRIC_READ_ORDERING),
            error_io_total: self.error_io_total.load(METRIC_READ_ORDERING),
            error_other_total: self.error_other_total.load(METRIC_READ_ORDERING),
            transaction_commits_total: self.transaction_commits_total.load(METRIC_READ_ORDERING),
        }
    }

    /// Reset all counters to zero. Intended for tests only.
    #[cfg(test)]
    pub fn reset(&self) {
        self.lock_poison_count.store(0, METRIC_WRITE_ORDERING);
        self.timestamp_violations.store(0, METRIC_WRITE_ORDERING);
        self.wal_checksum_failures.store(0, METRIC_WRITE_ORDERING);
        self.write_conflicts.store(0, METRIC_WRITE_ORDERING);
        self.error_storage_total.store(0, METRIC_WRITE_ORDERING);
        self.error_temporal_total.store(0, METRIC_WRITE_ORDERING);
        self.error_query_total.store(0, METRIC_WRITE_ORDERING);
        self.error_transaction_total.store(0, METRIC_WRITE_ORDERING);
        self.error_vector_total.store(0, METRIC_WRITE_ORDERING);
        self.error_io_total.store(0, METRIC_WRITE_ORDERING);
        self.error_other_total.store(0, METRIC_WRITE_ORDERING);
        self.transaction_commits_total
            .store(0, METRIC_WRITE_ORDERING);
    }
}

impl MetricsRecorder for Metrics {
    fn record_error(&self, category: ErrorCategory) {
        let counter = match category {
            ErrorCategory::Storage => &self.error_storage_total,
            ErrorCategory::Temporal => &self.error_temporal_total,
            ErrorCategory::Query => &self.error_query_total,
            ErrorCategory::Transaction => &self.error_transaction_total,
            ErrorCategory::Vector => &self.error_vector_total,
            ErrorCategory::Io => &self.error_io_total,
            ErrorCategory::Other => &self.error_other_total,
        };
        counter.fetch_add(1, METRIC_WRITE_ORDERING);
    }

    fn record_write_conflict(&self) {
        self.write_conflicts.fetch_add(1, METRIC_WRITE_ORDERING);
    }

    fn record_critical_event(&self, event: CriticalEvent) {
        let counter = match event {
            CriticalEvent::LockPoison => &self.lock_poison_count,
            CriticalEvent::TimestampViolation => &self.timestamp_violations,
            CriticalEvent::WalChecksumFailure => &self.wal_checksum_failures,
        };
        counter.fetch_add(1, METRIC_WRITE_ORDERING);
    }

    fn record_transaction_commit(
        &self,
        _duration_secs: f64,
        _operations_count: u64,
        _durability_mode: &str,
        _status: &str,
    ) {
        self.transaction_commits_total
            .fetch_add(1, METRIC_WRITE_ORDERING);
    }
}

/// Snapshot of the built-in atomic metric mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricsSnapshot {
    /// Number of lock poisoning events detected.
    pub lock_poison_count: u64,

    /// Number of timestamp monotonicity violations.
    pub timestamp_violations: u64,

    /// Number of WAL checksum failures detected.
    pub wal_checksum_failures: u64,

    /// Number of write-write conflicts.
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

    /// Total transaction commits.
    pub transaction_commits_total: u64,
}

impl MetricsSnapshot {
    /// `true` when any critical corruption indicator is non-zero.
    #[must_use]
    pub const fn has_critical_errors(&self) -> bool {
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
    fn error_category_string_values_are_stable() {
        assert_eq!(ErrorCategory::Storage.as_str(), "storage");
        assert_eq!(ErrorCategory::Temporal.as_str(), "temporal");
        assert_eq!(ErrorCategory::Query.as_str(), "query");
        assert_eq!(ErrorCategory::Transaction.as_str(), "transaction");
        assert_eq!(ErrorCategory::Vector.as_str(), "vector");
        assert_eq!(ErrorCategory::Io.as_str(), "io");
        assert_eq!(ErrorCategory::Other.as_str(), "other");
    }

    #[test]
    fn metrics_initialization() {
        let metrics = Metrics::new();
        let snapshot = metrics.snapshot();

        assert_eq!(snapshot.lock_poison_count, 0);
        assert_eq!(snapshot.timestamp_violations, 0);
        assert_eq!(snapshot.wal_checksum_failures, 0);
        assert_eq!(snapshot.write_conflicts, 0);
        assert_eq!(snapshot.transaction_commits_total, 0);
    }

    #[test]
    fn metrics_increment_through_contract() {
        let metrics = Metrics::new();

        metrics.record_critical_event(CriticalEvent::LockPoison);
        metrics.record_write_conflict();
        metrics.record_error(ErrorCategory::Storage);
        metrics.record_transaction_commit(0.001, 3, "Synchronous", "committed");

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.lock_poison_count, 1);
        assert_eq!(snapshot.write_conflicts, 1);
        assert_eq!(snapshot.error_storage_total, 1);
        assert_eq!(snapshot.transaction_commits_total, 1);
    }

    #[test]
    fn concurrent_increments() {
        let metrics = Arc::new(Metrics::new());
        let mut handles = vec![];

        for _ in 0..10 {
            let metrics_clone = Arc::clone(&metrics);
            let handle = thread::spawn(move || {
                for _ in 0..100 {
                    metrics_clone.record_write_conflict();
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(metrics.snapshot().write_conflicts, 1000);
    }

    #[test]
    fn has_critical_errors() {
        let metrics = Metrics::new();

        assert!(!metrics.snapshot().has_critical_errors());

        metrics.record_critical_event(CriticalEvent::LockPoison);
        assert!(metrics.snapshot().has_critical_errors());

        metrics.reset();
        metrics.record_critical_event(CriticalEvent::TimestampViolation);
        assert!(metrics.snapshot().has_critical_errors());

        metrics.reset();
        metrics.record_critical_event(CriticalEvent::WalChecksumFailure);
        assert!(metrics.snapshot().has_critical_errors());

        metrics.reset();
        metrics.record_write_conflict();
        assert!(!metrics.snapshot().has_critical_errors());
    }

    #[test]
    fn global_metrics_singleton() {
        METRICS.record_critical_event(CriticalEvent::LockPoison);
        assert!(METRICS.snapshot().lock_poison_count >= 1);
    }
}
