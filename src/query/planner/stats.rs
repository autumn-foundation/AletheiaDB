//! Statistics Collection for Query Optimization
//!
//! Provides cardinality and selectivity estimates for cost-based optimization.
//!
//! # Role in Query Planning
//!
//! The query planner relies on accurate statistics to make informed decisions about:
//! - **Join Order**: Which side of a join should be the build vs. probe side?
//! - **Access Methods**: Should we use a full scan, an index lookup, or a vector search?
//! - **Filter Ordering**: Which predicates should be applied first?
//!
//! # Lifecycle
//!
//! Statistics are **not** updated in real-time with every write operation to avoid
//! lock contention on the shared `Statistics` structure. Instead, they follow a
//! lazy/manual refresh model:
//!
//! 1. **Initialization**: Created with default/empty values.
//! 2. **Lazy Refresh**: The database triggers a refresh on the first query execution.
//! 3. **Manual Refresh**: Administrators can trigger a refresh via [`AletheiaDB::refresh_statistics`].
//! 4. **Invalidation**: Schema changes (e.g., adding an index) invalidate the cache,
//!    forcing a refresh on the next query.
//!
//! [`AletheiaDB::refresh_statistics`]: crate::db::AletheiaDB::refresh_statistics

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use dashmap::DashMap;
use parking_lot::RwLock;

use crate::core::interning::InternedString;

/// Atomic f64 for lock-free floating-point statistics.
///
/// Converts `f64` into its bit representation to be stored
/// lock-free within an `AtomicU64`.
///
/// ## Examples
///
/// ```rust
/// use aletheiadb::query::planner::stats::AtomicF64;
/// use std::sync::atomic::Ordering;
///
/// let value = AtomicF64::new(42.5);
/// assert_eq!(value.load(Ordering::Acquire), 42.5);
///
/// value.store(100.0, Ordering::Release);
/// assert_eq!(value.load(Ordering::Acquire), 100.0);
/// ```
#[derive(Debug)]
pub struct AtomicF64 {
    inner: std::sync::atomic::AtomicU64,
}

impl AtomicF64 {
    /// Create a new atomic f64
    pub fn new(value: f64) -> Self {
        AtomicF64 {
            inner: std::sync::atomic::AtomicU64::new(value.to_bits()),
        }
    }

    /// Load the value
    pub fn load(&self, ordering: Ordering) -> f64 {
        f64::from_bits(self.inner.load(ordering))
    }

    /// Store a value
    pub fn store(&self, value: f64, ordering: Ordering) {
        self.inner.store(value.to_bits(), ordering);
    }
}

impl Default for AtomicF64 {
    fn default() -> Self {
        Self::new(0.0)
    }
}

/// Query statistics for optimization.
///
/// Statistics are collected lazily on first query and cached.
/// They can be manually refreshed or automatically invalidated
/// on schema changes.
///
/// # Examples
///
/// ```rust
/// use aletheiadb::query::planner::Statistics;
/// use aletheiadb::core::interning::InternedString;
///
/// let stats = Statistics::new();
///
/// // Statistics are initially empty
/// assert_eq!(stats.node_count(), 0);
/// assert!(!stats.is_initialized());
///
/// // Manually refreshing statistics
/// // In a real app, AletheiaDB::refresh_statistics() calls this
/// stats.refresh(
///     1000, // node_count
///     5000, // edge_count
///     100,  // vector_count
///     vec![], // label_counts
///     5.0   // avg_delta_chain
/// );
///
/// assert_eq!(stats.node_count(), 1000);
/// assert!(stats.is_initialized());
/// ```
#[derive(Debug, Default)]
pub struct Statistics {
    /// Whether statistics have been collected.
    ///
    /// If false, the query engine will trigger a refresh before planning.
    initialized: AtomicBool,

    /// Total node count in the current graph.
    ///
    /// Used to estimate scan costs and filter selectivity.
    node_count: AtomicUsize,

    /// Total edge count in the current graph.
    ///
    /// Used to estimate traversal costs and average degree.
    edge_count: AtomicUsize,

    /// Number of indexed vectors.
    ///
    /// Used to estimate the cost of vector search operations.
    vector_count: AtomicUsize,

    /// Label cardinalities (count of nodes per label).
    ///
    /// Used to estimate the output size of `ScanNodes(label)`.
    label_counts: DashMap<InternedString, usize>,

    /// Average out-degree (edges per node).
    ///
    /// Critical for estimating the explosion factor of graph traversals.
    /// A high degree increases the cost of multi-hop queries exponentially.
    avg_out_degree: AtomicF64,

    /// Average delta chain length in historical storage.
    ///
    /// Represents the average number of versions one must walk back to find an anchor.
    /// Used to estimate the I/O cost of temporal lookups (`AS OF` queries).
    ///
    /// - **Low value (near 1.0)**: Most lookups hit anchors directly (fast).
    /// - **High value**: Lookups require applying many deltas (slow).
    avg_delta_chain: AtomicF64,

    /// Property statistics for selectivity estimation.
    property_stats: RwLock<PropertyStats>,
}

impl Statistics {
    /// Create new empty statistics
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    ///
    /// let stats = Statistics::new();
    /// assert_eq!(stats.node_count(), 0);
    /// assert!(!stats.is_initialized());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if statistics have been initialized
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    ///
    /// let stats = Statistics::new();
    /// assert!(!stats.is_initialized()); // false by default
    /// ```
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    /// Get the total node count
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    ///
    /// let stats = Statistics::new();
    /// assert_eq!(stats.node_count(), 0);
    /// ```
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.node_count.load(Ordering::Relaxed)
    }

    /// Get the total edge count
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    ///
    /// let stats = Statistics::new();
    /// assert_eq!(stats.edge_count(), 0);
    /// ```
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edge_count.load(Ordering::Relaxed)
    }

    /// Get the number of indexed vectors
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    ///
    /// let stats = Statistics::new();
    /// assert_eq!(stats.vector_count(), 0);
    /// ```
    #[must_use]
    pub fn vector_count(&self) -> usize {
        self.vector_count.load(Ordering::Relaxed)
    }

    /// Get the average out-degree
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    ///
    /// let stats = Statistics::new();
    /// // Returns a non-zero default even when uninitialized to prevent divide-by-zero
    /// assert!(stats.average_out_degree() > 0.0);
    /// ```
    #[must_use]
    pub fn average_out_degree(&self) -> f64 {
        let avg = self.avg_out_degree.load(Ordering::Relaxed);
        if avg < f64::EPSILON {
            // Default estimate if not initialized or near-zero
            10.0
        } else {
            avg
        }
    }

    /// Get the average delta chain length
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    ///
    /// let stats = Statistics::new();
    /// // Returns a non-zero default even when uninitialized
    /// assert!(stats.average_delta_chain_length() > 0.0);
    /// ```
    #[must_use]
    pub fn average_delta_chain_length(&self) -> f64 {
        let avg = self.avg_delta_chain.load(Ordering::Relaxed);
        if avg < f64::EPSILON {
            // Default estimate (anchor every 10 versions -> avg depth ~5)
            5.0
        } else {
            avg
        }
    }

    /// Get cardinality for a specific label
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    /// use aletheiadb::core::interning::InternedString;
    ///
    /// let stats = Statistics::new();
    /// let label = InternedString::from_raw(1); // Assuming 1 represents "Person"
    /// assert_eq!(stats.label_cardinality(&label), None);
    /// ```
    #[must_use]
    pub fn label_cardinality(&self, label: &InternedString) -> Option<usize> {
        self.label_counts.get(label).map(|r| *r)
    }

    /// Estimate selectivity for a property value
    ///
    /// Returns a value between 0.0 and 1.0 indicating what fraction
    /// of rows would pass an equality predicate.
    ///
    /// If distinct counts are available, uses `1.0 / distinct_count`.
    /// Otherwise, assumes a default selectivity of 10% (0.1).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    ///
    /// let stats = Statistics::new();
    ///
    /// // Default selectivity (unknown distribution)
    /// assert_eq!(stats.estimate_selectivity("status", "active"), 0.1);
    ///
    /// // With known distinct counts (e.g. 2 statuses: active, inactive)
    /// stats.update_property_stats("status", 2);
    /// assert_eq!(stats.estimate_selectivity("status", "active"), 0.5);
    /// ```
    #[must_use]
    pub fn estimate_selectivity(&self, property: &str, _value: &str) -> f64 {
        let stats = self.property_stats.read();
        stats
            .distinct_counts
            .get(property)
            .map(|&count| 1.0 / (count.max(1) as f64))
            .unwrap_or(0.1) // Default 10% selectivity
    }

    /// Update statistics from storage.
    ///
    /// This is called by [`AletheiaDB::refresh_statistics`](crate::db::AletheiaDB::refresh_statistics)
    /// to populate the cache with fresh values from the storage engine.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    ///
    /// let stats = Statistics::new();
    /// stats.refresh(100, 50, 10, vec![], 2.0);
    /// assert_eq!(stats.node_count(), 100);
    /// ```
    pub fn refresh(
        &self,
        node_count: usize,
        edge_count: usize,
        vector_count: usize,
        label_counts: impl IntoIterator<Item = (InternedString, usize)>,
        avg_delta_chain: f64,
    ) {
        self.node_count.store(node_count, Ordering::Relaxed);
        self.edge_count.store(edge_count, Ordering::Relaxed);
        self.vector_count.store(vector_count, Ordering::Relaxed);

        // Calculate average degree
        if node_count > 0 {
            let avg = edge_count as f64 / node_count as f64;
            self.avg_out_degree.store(avg, Ordering::Relaxed);
        }

        self.avg_delta_chain
            .store(avg_delta_chain, Ordering::Relaxed);

        // Update label counts
        self.label_counts.clear();
        for (label, count) in label_counts {
            self.label_counts.insert(label, count);
        }

        self.initialized.store(true, Ordering::Release);
    }

    /// Invalidate cached statistics.
    ///
    /// Forces the next query to trigger a refresh.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    ///
    /// let stats = Statistics::new();
    /// stats.refresh(100, 50, 10, vec![], 2.0);
    /// assert!(stats.is_initialized());
    ///
    /// stats.invalidate();
    /// assert!(!stats.is_initialized());
    /// ```
    pub fn invalidate(&self) {
        self.initialized.store(false, Ordering::Release);
    }

    /// Update property statistics.
    ///
    /// Used to feed distinct counts from background analysis jobs.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::Statistics;
    ///
    /// let stats = Statistics::new();
    /// stats.update_property_stats("status", 3);
    /// assert_eq!(stats.estimate_selectivity("status", "active"), 1.0 / 3.0);
    /// ```
    pub fn update_property_stats(&self, property: &str, distinct_count: usize) {
        let mut stats = self.property_stats.write();
        stats
            .distinct_counts
            .insert(property.to_string(), distinct_count);
    }
}

/// Property-level statistics for selectivity estimation.
#[derive(Debug, Default)]
struct PropertyStats {
    /// Number of distinct values per property
    distinct_counts: std::collections::HashMap<String, usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_statistics_defaults() {
        let stats = Statistics::new();

        assert!(!stats.is_initialized());
        assert_eq!(stats.node_count(), 0);
        assert_eq!(stats.edge_count(), 0);
        assert_eq!(stats.vector_count(), 0);

        // Should return default estimates
        assert!(stats.average_out_degree() > 0.0);
        assert!(stats.average_delta_chain_length() > 0.0);
    }

    #[test]
    fn test_statistics_refresh() {
        let stats = Statistics::new();

        let labels: Vec<(InternedString, usize)> = vec![];
        stats.refresh(100, 500, 50, labels, 4.5);

        assert!(stats.is_initialized());
        assert_eq!(stats.node_count(), 100);
        assert_eq!(stats.edge_count(), 500);
        assert_eq!(stats.vector_count(), 50);
        assert!((stats.average_out_degree() - 5.0).abs() < 0.01);
        assert!((stats.average_delta_chain_length() - 4.5).abs() < 0.01);
    }

    #[test]
    fn test_selectivity_estimation() {
        let stats = Statistics::new();

        // Default selectivity
        let sel = stats.estimate_selectivity("name", "Alice");
        assert!((sel - 0.1).abs() < 0.01);

        // Update with distinct count
        stats.update_property_stats("name", 100);
        let sel = stats.estimate_selectivity("name", "Alice");
        assert!((sel - 0.01).abs() < 0.001);
    }

    #[test]
    fn test_invalidation() {
        let stats = Statistics::new();

        let labels: Vec<(InternedString, usize)> = vec![];
        stats.refresh(100, 500, 50, labels, 4.5);
        assert!(stats.is_initialized());

        stats.invalidate();
        assert!(!stats.is_initialized());
    }

    #[test]
    fn test_atomic_f64() {
        let af = AtomicF64::new(1.23);
        assert!((af.load(Ordering::Relaxed) - 1.23).abs() < 0.001);

        af.store(4.56, Ordering::Relaxed);
        assert!((af.load(Ordering::Relaxed) - 4.56).abs() < 0.001);
    }
}
