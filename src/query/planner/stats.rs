//! Statistics Collection for Query Optimization
//!
//! Provides cardinality and selectivity estimates for cost-based optimization.
//! Statistics are collected lazily and cached until invalidated.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use dashmap::DashMap;
use parking_lot::RwLock;

use crate::core::interning::InternedString;

/// Atomic f64 for lock-free floating-point statistics.
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
#[derive(Debug, Default)]
pub struct Statistics {
    /// Whether statistics have been collected
    initialized: AtomicBool,

    /// Total node count
    node_count: AtomicUsize,

    /// Total edge count
    edge_count: AtomicUsize,

    /// Number of indexed vectors
    vector_count: AtomicUsize,

    /// Label cardinalities
    label_counts: DashMap<InternedString, usize>,

    /// Average out-degree (edges per node)
    avg_out_degree: AtomicF64,

    /// Average delta chain length in historical storage
    avg_delta_chain: AtomicF64,

    /// Property statistics for selectivity estimation
    property_stats: RwLock<PropertyStats>,
}

impl Statistics {
    /// Create new empty statistics
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if statistics have been initialized
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    /// Get the total node count
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.node_count.load(Ordering::Relaxed)
    }

    /// Get the total edge count
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edge_count.load(Ordering::Relaxed)
    }

    /// Get the number of indexed vectors
    #[must_use]
    pub fn vector_count(&self) -> usize {
        self.vector_count.load(Ordering::Relaxed)
    }

    /// Get the average out-degree
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
    #[must_use]
    pub fn average_delta_chain_length(&self) -> f64 {
        let avg = self.avg_delta_chain.load(Ordering::Relaxed);
        if avg < f64::EPSILON {
            // Default estimate (anchor every 10 versions)
            5.0
        } else {
            avg
        }
    }

    /// Get cardinality for a specific label
    #[must_use]
    pub fn label_cardinality(&self, label: &InternedString) -> Option<usize> {
        self.label_counts.get(label).map(|r| *r)
    }

    /// Estimate selectivity for a property value
    ///
    /// Returns a value between 0.0 and 1.0 indicating what fraction
    /// of rows would pass an equality predicate.
    #[must_use]
    pub fn estimate_selectivity(&self, property: &str, _value: &str) -> f64 {
        let stats = self.property_stats.read();
        stats
            .distinct_counts
            .get(property)
            .map(|&count| 1.0 / (count.max(1) as f64))
            .unwrap_or(0.1) // Default 10% selectivity
    }

    /// Update statistics from storage
    ///
    /// This is called lazily on first query or when explicitly requested.
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

    /// Invalidate cached statistics
    pub fn invalidate(&self) {
        self.initialized.store(false, Ordering::Release);
    }

    /// Update property statistics
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
