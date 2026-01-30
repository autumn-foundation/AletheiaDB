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
