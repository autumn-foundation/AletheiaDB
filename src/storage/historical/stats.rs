use super::HistoricalStorage;
use std::sync::atomic::Ordering;

/// Cache performance metrics (Issue #338: Improvement #3).
///
/// Provides granular insight into cache behavior:
/// - `primary_cache_hits`: Fast path hits (most common)
/// - `anchor_cache_hits`: Fallback hits (indicates primary cache pressure)
/// - `full_reconstructions`: Slow path (indicates insufficient cache capacity)
///
/// # Interpretation
/// - High `anchor_cache_hits` + low `primary_cache_hits` → increase primary cache size
/// - High `full_reconstructions` → increase overall cache capacity
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheMetrics {
    /// Number of successful lookups in primary property cache (fast path)
    pub primary_cache_hits: u64,
    /// Number of successful lookups in anchor cache fallback
    pub anchor_cache_hits: u64,
    /// Number of full property reconstructions from deltas
    pub full_reconstructions: u64,
}

impl CacheMetrics {
    /// Calculate total cache operations (hits + reconstructions).
    pub fn total_operations(&self) -> u64 {
        self.primary_cache_hits + self.anchor_cache_hits + self.full_reconstructions
    }

    /// Calculate overall cache hit rate (0.0 to 1.0).
    ///
    /// Returns None if no operations have been performed yet.
    pub fn hit_rate(&self) -> Option<f64> {
        let total = self.total_operations();
        if total == 0 {
            None
        } else {
            Some((self.primary_cache_hits + self.anchor_cache_hits) as f64 / total as f64)
        }
    }

    /// Calculate primary cache hit rate (0.0 to 1.0).
    ///
    /// This shows how often the primary cache is sufficient without fallback.
    /// Returns None if no operations have been performed yet.
    pub fn primary_hit_rate(&self) -> Option<f64> {
        let total = self.total_operations();
        if total == 0 {
            None
        } else {
            Some(self.primary_cache_hits as f64 / total as f64)
        }
    }

    /// Calculate anchor cache fallback rate (0.0 to 1.0).
    ///
    /// This shows how often we need to fall back to the anchor cache.
    /// High values indicate the primary cache is under pressure.
    pub fn anchor_fallback_rate(&self) -> Option<f64> {
        let total = self.total_operations();
        if total == 0 {
            None
        } else {
            Some(self.anchor_cache_hits as f64 / total as f64)
        }
    }

    /// Calculate reconstruction rate (0.0 to 1.0).
    ///
    /// This shows how often we need to perform full reconstruction.
    /// High values indicate insufficient overall cache capacity.
    pub fn reconstruction_rate(&self) -> Option<f64> {
        let total = self.total_operations();
        if total == 0 {
            None
        } else {
            Some(self.full_reconstructions as f64 / total as f64)
        }
    }
}

/// Statistics about the historical storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalStats {
    /// Total number of node versions stored
    pub total_node_versions: usize,
    /// Total number of edge versions stored
    pub total_edge_versions: usize,
    /// Number of anchor node versions
    pub node_anchor_count: usize,
    /// Number of delta node versions
    pub node_delta_count: usize,
    /// Number of anchor edge versions
    pub edge_anchor_count: usize,
    /// Number of delta edge versions
    pub edge_delta_count: usize,
    /// Number of unique nodes with version history
    pub unique_nodes: usize,
    /// Number of unique edges with version history
    pub unique_edges: usize,
    /// Number of cached node property reconstructions (regular cache)
    pub node_cache_entries: usize,
    /// Number of cached edge property reconstructions (regular cache)
    pub edge_cache_entries: usize,
    /// Number of cached node anchor properties (dedicated anchor cache, Issue #338)
    pub node_anchor_cache_entries: usize,
    /// Number of cached edge anchor properties (dedicated anchor cache, Issue #338)
    pub edge_anchor_cache_entries: usize,
}

impl HistoricalStats {
    /// Calculate the compression ratio (anchors vs total versions).
    pub fn compression_ratio(&self) -> f64 {
        let total_versions = self.total_node_versions + self.total_edge_versions;
        let total_anchors = self.node_anchor_count + self.edge_anchor_count;

        if total_versions == 0 {
            return 1.0;
        }

        total_anchors as f64 / total_versions as f64
    }

    /// Estimate total cache memory usage in bytes (Issue #338: Memory Accounting).
    ///
    /// Provides rough estimate of memory consumed by all caches. Actual memory
    /// usage may vary based on property sizes, Arc overhead, and allocator behavior.
    ///
    /// # Formula
    /// Per entry overhead:
    /// - VersionId: 8 bytes
    /// - Arc pointer: 8 bytes
    /// - PropertyMap overhead: ~16 bytes
    /// - Average property data: ~100 bytes (varies by use case)
    /// - Total: ~132 bytes per entry (rounded to 150 for safety margin)
    ///
    /// # Returns
    /// Estimated bytes used by all caches (primary + anchor)
    ///
    /// # Example
    /// ```no_run
    /// # use aletheiadb::storage::historical::stats::HistoricalStats;
    /// # // ... construct stats ...
    /// # let stats = HistoricalStats {
    /// #     total_node_versions: 0, total_edge_versions: 0,
    /// #     node_anchor_count: 0, node_delta_count: 0,
    /// #     edge_anchor_count: 0, edge_delta_count: 0,
    /// #     unique_nodes: 0, unique_edges: 0,
    /// #     node_cache_entries: 0, edge_cache_entries: 0,
    /// #     node_anchor_cache_entries: 0, edge_anchor_cache_entries: 0,
    /// # };
    /// let bytes = stats.estimated_cache_memory_bytes();
    /// println!("Cache using ~{:.2} MB", bytes as f64 / 1_048_576.0);
    /// ```
    pub fn estimated_cache_memory_bytes(&self) -> usize {
        // Rough estimate per cache entry:
        // - VersionId (u64): 8 bytes
        // - Arc<PropertyMap> pointer: 8 bytes
        // - PropertyMap struct overhead: ~16 bytes
        // - Average property data: ~100 bytes (varies widely)
        // Total: ~132 bytes, rounded to 150 for safety margin
        const BYTES_PER_ENTRY: usize = 150;

        let total_entries = self.node_cache_entries
            + self.edge_cache_entries
            + self.node_anchor_cache_entries
            + self.edge_anchor_cache_entries;

        total_entries * BYTES_PER_ENTRY
    }
}

impl HistoricalStorage {
    /// Get statistics about the storage.
    ///
    /// Issue #212: This method now returns cached counters in O(1) time instead of
    /// iterating through all versions. The counters are maintained incrementally as
    /// versions are added, making stats retrieval constant-time regardless of the
    /// number of versions stored.
    pub fn stats(&self) -> HistoricalStats {
        // Debug assertions to verify counter invariants (zero cost in release builds)
        debug_assert_eq!(
            self.cached_node_anchor_count + self.cached_node_delta_count,
            self.node_versions.len(),
            "Node counter invariant violated: anchors({}) + deltas({}) != total({})",
            self.cached_node_anchor_count,
            self.cached_node_delta_count,
            self.node_versions.len()
        );
        debug_assert_eq!(
            self.cached_edge_anchor_count + self.cached_edge_delta_count,
            self.edge_versions.len(),
            "Edge counter invariant violated: anchors({}) + deltas({}) != total({})",
            self.cached_edge_anchor_count,
            self.cached_edge_delta_count,
            self.edge_versions.len()
        );

        HistoricalStats {
            total_node_versions: self.node_versions.len(),
            total_edge_versions: self.edge_versions.len(),
            // Issue #212: Use cached counters instead of iterating (O(1) vs O(versions))
            node_anchor_count: self.cached_node_anchor_count,
            node_delta_count: self.cached_node_delta_count,
            edge_anchor_count: self.cached_edge_anchor_count,
            edge_delta_count: self.cached_edge_delta_count,
            unique_nodes: self.node_version_heads.len(),
            unique_edges: self.edge_version_heads.len(),
            // Separate regular and anchor cache entries for better visibility (Issue #338)
            node_cache_entries: self.node_property_cache.len(),
            edge_cache_entries: self.edge_property_cache.len(),
            node_anchor_cache_entries: self.node_anchor_cache.len(),
            edge_anchor_cache_entries: self.edge_anchor_cache.len(),
        }
    }

    /// Get cache performance metrics (Improvement #3: Adaptive Cache Sizing).
    ///
    /// Returns granular cache performance metrics that show:
    /// - `primary_cache_hits`: Fast path hits (most common)
    /// - `anchor_cache_hits`: Fallback hits (indicates primary cache pressure)
    /// - `full_reconstructions`: Slow path (indicates insufficient cache capacity)
    ///
    /// This provides actionable insights for cache tuning:
    /// - High `anchor_cache_hits` + low `primary_cache_hits` → increase primary cache size
    /// - High `full_reconstructions` → increase overall cache capacity
    ///
    /// # Example
    /// ```no_run
    /// # use aletheiadb::storage::historical::HistoricalStorage;
    /// let storage = HistoricalStorage::new();
    /// // ... perform some operations ...
    /// let metrics = storage.cache_metrics();
    ///
    /// if let Some(hit_rate) = metrics.hit_rate() {
    ///     println!("Overall cache hit rate: {:.2}%", hit_rate * 100.0);
    /// }
    ///
    /// if let Some(fallback_rate) = metrics.anchor_fallback_rate() {
    ///     if fallback_rate > 0.2 {
    ///         println!("Warning: High anchor cache fallback rate ({:.2}%), \
    ///                   consider increasing primary cache size", fallback_rate * 100.0);
    ///     }
    /// }
    ///
    /// if let Some(recon_rate) = metrics.reconstruction_rate() {
    ///     if recon_rate > 0.2 {
    ///         println!("Warning: High reconstruction rate ({:.2}%), \
    ///                   increase overall cache size", recon_rate * 100.0);
    ///     }
    /// }
    /// ```
    pub fn cache_metrics(&self) -> CacheMetrics {
        CacheMetrics {
            primary_cache_hits: self.primary_cache_hits.load(Ordering::Relaxed),
            anchor_cache_hits: self.anchor_cache_hits.load(Ordering::Relaxed),
            full_reconstructions: self.full_reconstructions.load(Ordering::Relaxed),
        }
    }

    /// Calculate the cache hit rate as a percentage (Improvement #3).
    ///
    /// Returns the cache hit rate as a value between 0.0 and 1.0, or None if
    /// no cache operations have been performed yet.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        self.cache_metrics().hit_rate()
    }
}
