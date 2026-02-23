use std::sync::atomic::AtomicU64;

/// Statistics for index operations.
#[derive(Debug, Default)]
pub struct IndexStats {
    /// Total number of vectors added (including updates)
    pub vectors_added: AtomicU64,
    /// Total number of vectors removed
    pub vectors_removed: AtomicU64,
    /// Total number of search operations performed
    pub searches_performed: AtomicU64,
    /// Number of times search operations were retried due to transient errors
    pub search_retries: AtomicU64,
    /// Number of searches that failed even after all retry attempts
    pub search_retry_failures: AtomicU64,
}
