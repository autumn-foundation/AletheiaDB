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

/// Maximum number of search attempts (initial attempt + retries) when encountering transient errors.
///
/// Under high concurrent load, usearch may fail with "No available threads to lock" when its
/// internal thread pool is exhausted. This constant controls how many times we retry before
/// giving up.
///
/// # Performance Impact
///
/// With exponential backoff (1ms, 2ms, 4ms), a query that exhausts all retry attempts will
/// add up to 7ms latency. This is significant relative to the project's performance targets:
/// - k-NN search target: <10ms
/// - Hybrid query target: <30ms
///
/// Operators should monitor `search_retries` and `search_retry_failures` metrics. Frequent
/// retries indicate thread pool exhaustion and may require tuning usearch parameters or
/// reducing concurrency.
pub const MAX_SEARCH_ATTEMPTS: u32 = 4; // 1 initial attempt + 3 retries
