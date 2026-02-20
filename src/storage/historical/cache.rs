//! Caching logic for historical storage.
//!
//! This module encapsulates the caching strategy for node and edge properties,
//! including the dual-cache mechanism (primary + anchor) and adaptive sizing metrics.

use crate::core::id::VersionId;
use crate::core::property::PropertyMap;
use quick_cache::sync::Cache;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Default cache size for reconstructed properties (10,000 entries)
pub const DEFAULT_RECONSTRUCTION_CACHE_SIZE: usize = 10_000;

/// Anchor cache size ratio relative to main cache (Improvement #1: Issue #338).
///
/// Typically 10-20% of versions become anchors depending on `anchor_interval`.
/// With default interval of 10, we get ~10% anchors. Setting to 1/5 (20%)
/// provides headroom for configurations with smaller intervals.
pub const ANCHOR_CACHE_SIZE_RATIO: usize = 5; // 20% of main cache

/// Minimum anchor cache size to ensure reasonable performance (Improvement #1: Issue #338).
///
/// Even with very small main caches, we want enough anchor cache to hold
/// at least a few anchors to avoid immediate evictions.
pub const MIN_ANCHOR_CACHE_SIZE: usize = 100;

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
        self.calculate_rate(self.primary_cache_hits + self.anchor_cache_hits)
    }

    /// Calculate primary cache hit rate (0.0 to 1.0).
    ///
    /// This shows how often the primary cache is sufficient without fallback.
    /// Returns None if no operations have been performed yet.
    pub fn primary_hit_rate(&self) -> Option<f64> {
        self.calculate_rate(self.primary_cache_hits)
    }

    /// Calculate anchor cache fallback rate (0.0 to 1.0).
    ///
    /// This shows how often we need to fall back to the anchor cache.
    /// High values indicate the primary cache is under pressure.
    pub fn anchor_fallback_rate(&self) -> Option<f64> {
        self.calculate_rate(self.anchor_cache_hits)
    }

    /// Calculate reconstruction rate (0.0 to 1.0).
    ///
    /// This shows how often we need to perform full reconstruction.
    /// High values indicate insufficient overall cache capacity.
    pub fn reconstruction_rate(&self) -> Option<f64> {
        self.calculate_rate(self.full_reconstructions)
    }

    fn calculate_rate(&self, numerator: u64) -> Option<f64> {
        let total = self.total_operations();
        if total == 0 {
            None
        } else {
            Some(numerator as f64 / total as f64)
        }
    }
}

/// Manages caching for historical storage.
pub struct HistoryCache {
    /// TinyLFU cache for reconstructed node properties (reduces lock contention)
    node_property_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// TinyLFU cache for reconstructed edge properties
    edge_property_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// Improvement #1: Dedicated cache for node anchor properties.
    node_anchor_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// Improvement #1: Dedicated cache for edge anchor properties.
    edge_anchor_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// Improvement #3: Primary cache hit counter for adaptive sizing.
    primary_cache_hits: Arc<AtomicU64>,
    /// Improvement #3: Anchor cache hit counter for adaptive sizing.
    anchor_cache_hits: Arc<AtomicU64>,
    /// Improvement #3: Full reconstruction counter for adaptive sizing.
    full_reconstructions: Arc<AtomicU64>,
}

impl HistoryCache {
    /// Create a new history cache with the specified size.
    pub fn new(cache_size: usize) -> Self {
        // Calculate anchor cache size: typically 10-20% of entities become anchors
        // depending on anchor_interval (Improvement #1: Issue #338)
        let anchor_cache_size = (cache_size / ANCHOR_CACHE_SIZE_RATIO).max(MIN_ANCHOR_CACHE_SIZE);

        HistoryCache {
            node_property_cache: Arc::new(Cache::new(cache_size)),
            edge_property_cache: Arc::new(Cache::new(cache_size)),
            node_anchor_cache: Arc::new(Cache::new(anchor_cache_size)),
            edge_anchor_cache: Arc::new(Cache::new(anchor_cache_size)),
            primary_cache_hits: Arc::new(AtomicU64::new(0)),
            anchor_cache_hits: Arc::new(AtomicU64::new(0)),
            full_reconstructions: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Get cached node properties with dual-cache lookup.
    ///
    /// 1. Check regular cache first.
    /// 2. If not found, check dedicated anchor cache.
    ///
    /// Updates hit counters accordingly.
    pub fn get_node_properties(&self, version_id: VersionId) -> Option<Arc<PropertyMap>> {
        if let Some(cached) = self.node_property_cache.get(&version_id) {
            self.primary_cache_hits.fetch_add(1, Ordering::Relaxed);
            return Some(cached.clone());
        }

        // Fallback to dedicated anchor cache (survives delta cache pressure)
        if let Some(cached) = self.node_anchor_cache.get(&version_id) {
            self.anchor_cache_hits.fetch_add(1, Ordering::Relaxed);
            // Re-populate main cache to make this anchor "hot" again
            // This prevents repeatedly falling back to anchor cache for frequently accessed anchors
            self.node_property_cache.insert(version_id, cached.clone());
            return Some(cached.clone());
        }

        None
    }

    /// Put node properties into the cache.
    ///
    /// If `is_anchor` is true, also inserts into the dedicated anchor cache.
    pub fn put_node_properties(
        &self,
        version_id: VersionId,
        properties: Arc<PropertyMap>,
        is_anchor: bool,
    ) {
        self.node_property_cache
            .insert(version_id, properties.clone());

        if is_anchor {
            self.node_anchor_cache.insert(version_id, properties);
        }
    }

    /// Get cached edge properties with dual-cache lookup.
    pub fn get_edge_properties(&self, version_id: VersionId) -> Option<Arc<PropertyMap>> {
        if let Some(cached) = self.edge_property_cache.get(&version_id) {
            self.primary_cache_hits.fetch_add(1, Ordering::Relaxed);
            return Some(cached.clone());
        }

        if let Some(cached) = self.edge_anchor_cache.get(&version_id) {
            self.anchor_cache_hits.fetch_add(1, Ordering::Relaxed);
            self.edge_property_cache.insert(version_id, cached.clone());
            return Some(cached.clone());
        }

        None
    }

    /// Put edge properties into the cache.
    pub fn put_edge_properties(
        &self,
        version_id: VersionId,
        properties: Arc<PropertyMap>,
        is_anchor: bool,
    ) {
        self.edge_property_cache
            .insert(version_id, properties.clone());

        if is_anchor {
            self.edge_anchor_cache.insert(version_id, properties);
        }
    }

    /// Record a full reconstruction (cache miss).
    pub fn record_full_reconstruction(&self) {
        self.full_reconstructions.fetch_add(1, Ordering::Relaxed);
    }

    /// Get cache metrics.
    pub fn metrics(&self) -> CacheMetrics {
        CacheMetrics {
            primary_cache_hits: self.primary_cache_hits.load(Ordering::Relaxed),
            anchor_cache_hits: self.anchor_cache_hits.load(Ordering::Relaxed),
            full_reconstructions: self.full_reconstructions.load(Ordering::Relaxed),
        }
    }

    /// Get overall cache hit rate.
    pub fn hit_rate(&self) -> Option<f64> {
        self.metrics().hit_rate()
    }

    /// Check if the cache should be resized.
    pub fn should_resize(&self, threshold: f64, min_operations: u64) -> Option<f64> {
        let metrics = self.metrics();
        let total = metrics.total_operations();

        if total < min_operations {
            return None;
        }

        let hit_rate = metrics.hit_rate().unwrap_or(0.0);

        if hit_rate < threshold {
            Some(hit_rate)
        } else {
            None
        }
    }

    /// Clear all caches (for testing).
    pub fn clear(&self) {
        self.node_property_cache.clear();
        self.edge_property_cache.clear();
        self.node_anchor_cache.clear();
        self.edge_anchor_cache.clear();
    }

    /// Get cache sizes (node, edge, node_anchor, edge_anchor).
    pub fn sizes(&self) -> (usize, usize, usize, usize) {
        (
            self.node_property_cache.len(),
            self.edge_property_cache.len(),
            self.node_anchor_cache.len(),
            self.edge_anchor_cache.len(),
        )
    }

    /// Estimate total cache memory usage in bytes.
    #[allow(dead_code)]
    pub fn estimated_memory(&self) -> usize {
        // Rough estimate per cache entry:
        // - VersionId (u64): 8 bytes
        // - Arc<PropertyMap> pointer: 8 bytes
        // - PropertyMap struct overhead: ~16 bytes
        // - Average property data: ~100 bytes (varies widely)
        // Total: ~132 bytes, rounded to 150 for safety margin
        const BYTES_PER_ENTRY: usize = 150;

        let (n, e, na, ea) = self.sizes();
        (n + e + na + ea) * BYTES_PER_ENTRY
    }
}
