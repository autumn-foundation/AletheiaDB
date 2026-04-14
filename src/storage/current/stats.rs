/// Statistics about current storage
#[derive(Debug, Clone)]
pub struct CurrentStats {
    /// Number of nodes
    pub node_count: usize,
    /// Number of edges
    pub edge_count: usize,
}

/// Statistics for adaptive over-fetch heuristic in filtered vector search.
///
/// Tracks the historical pass rate of label filters to dynamically adjust
/// the over-fetch multiplier. This improves performance by:
/// - Reducing over-fetch for dense labels (high pass rate)
/// - Increasing over-fetch for sparse labels (low pass rate)
///
/// Issue #334: Adaptive over-fetch strategy
#[derive(Debug)]
pub(crate) struct FilterStats {
    /// Number of searches performed for this label
    pub(crate) search_count: std::sync::atomic::AtomicU64,
    /// Total number of candidates fetched across all searches
    pub(crate) total_candidates: std::sync::atomic::AtomicU64,
    /// Total number of results returned after filtering
    pub(crate) total_results: std::sync::atomic::AtomicU64,
}

impl FilterStats {
    /// Create new filter statistics with zero counts.
    pub(crate) fn new() -> Self {
        FilterStats {
            search_count: std::sync::atomic::AtomicU64::new(0),
            total_candidates: std::sync::atomic::AtomicU64::new(0),
            total_results: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Record a search operation and its results.
    ///
    /// # Arguments
    ///
    /// * `candidates_fetched` - Number of candidates retrieved from HNSW
    /// * `results_returned` - Number of results after label filtering
    ///
    /// # Memory Ordering
    ///
    /// Uses `Relaxed` ordering because:
    /// - These are simple counters with no synchronization requirements
    /// - Exact ordering between increments doesn't affect correctness
    /// - Pass rate calculation tolerates slightly stale reads
    /// - Performance is critical (called on every filtered search)
    ///
    /// # Overflow Safety
    ///
    /// Overflow after 2^64 operations is not a realistic concern in practice.
    /// At 1 million searches/second, overflow would take ~584,000 years.
    /// Standard wrapping arithmetic is used for simplicity and performance.
    pub(crate) fn record_search(&self, candidates_fetched: usize, results_returned: usize) {
        use std::sync::atomic::Ordering;

        // Simple atomic increments - wrapping overflow at 2^64 is not a realistic concern
        self.search_count.fetch_add(1, Ordering::Relaxed);
        self.total_candidates
            .fetch_add(candidates_fetched as u64, Ordering::Relaxed);
        self.total_results
            .fetch_add(results_returned as u64, Ordering::Relaxed);
    }

    /// Calculate the adaptive over-fetch multiplier based on historical pass rate.
    ///
    /// # Returns
    ///
    /// A multiplier to apply to k (e.g., 10.0 means fetch k * 10 candidates).
    ///
    /// # Algorithm
    ///
    /// 1. Start with default multiplier (10.0) for first few searches
    /// 2. Calculate historical pass rate = results / candidates
    /// 3. Adjust multiplier inversely to pass rate:
    ///    - High pass rate (90%+) → lower multiplier (5-7)
    ///    - Medium pass rate (50%) → medium multiplier (10-15)
    ///    - Low pass rate (10%-) → higher multiplier (20-30)
    /// 4. Cap multiplier at reasonable bounds (5.0 - 50.0)
    pub(crate) fn get_adaptive_multiplier(&self) -> f64 {
        use std::sync::atomic::Ordering;

        const MIN_SEARCHES_FOR_ADAPTATION: u64 = 3;
        const DEFAULT_MULTIPLIER: f64 = 10.0;
        const MIN_MULTIPLIER: f64 = 5.0;
        const MAX_MULTIPLIER: f64 = 50.0;

        let search_count = self.search_count.load(Ordering::Relaxed);

        // Use default until we have enough data
        if search_count < MIN_SEARCHES_FOR_ADAPTATION {
            return DEFAULT_MULTIPLIER;
        }

        let total_candidates = self.total_candidates.load(Ordering::Relaxed);
        let total_results = self.total_results.load(Ordering::Relaxed);

        // Avoid division by zero
        if total_candidates == 0 {
            return DEFAULT_MULTIPLIER;
        }

        // Calculate historical pass rate
        // Clamp to 1.0 to handle race conditions where results might temporarily
        // exceed candidates due to concurrent atomic operations
        let pass_rate = (total_results as f64 / total_candidates as f64).min(1.0);

        // Adaptive formula: higher multiplier for lower pass rates
        // multiplier = base / sqrt(pass_rate)
        // This gives smooth adaptation:
        // - pass_rate=1.0 (100%) → multiplier=5
        // - pass_rate=0.5 (50%)  → multiplier=7
        // - pass_rate=0.1 (10%)  → multiplier=16
        // - pass_rate=0.01 (1%)  → multiplier=50
        let multiplier = MIN_MULTIPLIER / pass_rate.sqrt();

        // Clamp to reasonable bounds
        multiplier.clamp(MIN_MULTIPLIER, MAX_MULTIPLIER)
    }
}
