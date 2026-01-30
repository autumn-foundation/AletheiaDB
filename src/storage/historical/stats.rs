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
    /// # use gallifreydb::storage::historical::HistoricalStorage;
    /// let storage = HistoricalStorage::new();
    /// // ... perform operations ...
    /// let stats = storage.stats();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_metrics_calculations() {
        let metrics = CacheMetrics {
            primary_cache_hits: 70,
            anchor_cache_hits: 10,
            full_reconstructions: 20,
        };

        assert_eq!(metrics.total_operations(), 100);
        assert!((metrics.hit_rate().unwrap() - 0.8).abs() < 1e-6);
        assert!((metrics.primary_hit_rate().unwrap() - 0.7).abs() < 1e-6);
        assert!((metrics.anchor_fallback_rate().unwrap() - 0.1).abs() < 1e-6);
        assert!((metrics.reconstruction_rate().unwrap() - 0.2).abs() < 1e-6);
    }

    #[test]
    fn test_cache_metrics_empty() {
        let metrics = CacheMetrics {
            primary_cache_hits: 0,
            anchor_cache_hits: 0,
            full_reconstructions: 0,
        };

        assert_eq!(metrics.total_operations(), 0);
        assert!(metrics.hit_rate().is_none());
        assert!(metrics.primary_hit_rate().is_none());
        assert!(metrics.anchor_fallback_rate().is_none());
        assert!(metrics.reconstruction_rate().is_none());
    }

    #[test]
    fn test_historical_stats_compression_ratio() {
        let mut stats = HistoricalStats {
            total_node_versions: 0,
            total_edge_versions: 0,
            node_anchor_count: 0,
            node_delta_count: 0,
            edge_anchor_count: 0,
            edge_delta_count: 0,
            unique_nodes: 0,
            unique_edges: 0,
            node_cache_entries: 0,
            edge_cache_entries: 0,
            node_anchor_cache_entries: 0,
            edge_anchor_cache_entries: 0,
        };

        // Case 1: Empty
        assert!((stats.compression_ratio() - 1.0).abs() < 1e-6);

        // Case 2: 10 node versions (1 anchor), 10 edge versions (1 anchor)
        stats.total_node_versions = 10;
        stats.node_anchor_count = 1;
        stats.total_edge_versions = 10;
        stats.edge_anchor_count = 1;

        // Ratio: 2 / 20 = 0.1
        assert!((stats.compression_ratio() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn test_estimated_cache_memory() {
        let stats = HistoricalStats {
            total_node_versions: 0,
            total_edge_versions: 0,
            node_anchor_count: 0,
            node_delta_count: 0,
            edge_anchor_count: 0,
            edge_delta_count: 0,
            unique_nodes: 0,
            unique_edges: 0,
            node_cache_entries: 10,
            edge_cache_entries: 10,
            node_anchor_cache_entries: 5,
            edge_anchor_cache_entries: 5,
        };

        // Total entries: 30
        // Expected size: 30 * 150 = 4500
        assert_eq!(stats.estimated_cache_memory_bytes(), 4500);
    }
}
