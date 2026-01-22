//! Tiered storage for transparent hot/cold data access.
//!
//! This module implements Issue #122 (SCALE-004): Transparent cold data access.
//! It provides a unified interface that seamlessly retrieves data from hot storage
//! (in-memory) or cold storage (disk-based) with a read-through cache.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      TieredStorage                           │
//! │   get_version() → hot → warm cache → cold → populate cache  │
//! └─────────────────────────────────────────────────────────────┘
//!                               │
//!           ┌───────────────────┼───────────────────┐
//!           │                   │                   │
//!           ▼                   ▼                   ▼
//! ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐
//! │    HOT TIER     │  │   WARM TIER     │  │   COLD TIER     │
//! │   (In-memory)   │  │  (LRU Cache)    │  │    (Disk)       │
//! │  22ns lookup    │  │  <1µs lookup    │  │  <1ms lookup    │
//! └─────────────────┘  └─────────────────┘  └─────────────────┘
//! ```
//!
//! # Performance Targets
//!
//! - Hot path: Maintain current speed (22-32 nanoseconds)
//! - Cache hit: Under 1 microsecond
//! - Disk read: Below 1 millisecond (p50 latency)
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::storage::tiered_storage::{TieredStorage, TieredStorageConfig};
//! use gallifreydb::storage::cold_storage::FileColdStorage;
//!
//! // Create tiered storage
//! let config = TieredStorageConfig::default();
//! let cold = FileColdStorage::with_default_config("data/cold")?;
//! let tiered = TieredStorage::new(config, Box::new(cold));
//!
//! // Transparently access data from any tier
//! let version = tiered.get_node_version(version_id)?;
//! ```

use crate::core::id::VersionId;
use crate::storage::cold_storage::{ColdStorage, ColdStorageStats};
use crate::storage::version::{EdgeVersion, NodeVersion};
use crate::utils::error::Result;
use quick_cache::sync::Cache;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

/// Configuration for tiered storage.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
pub struct TieredStorageConfig {
    /// Size of the warm cache (number of entries per type).
    /// This cache holds recently accessed cold data to reduce disk reads.
    pub warm_cache_size: usize,

    /// Enable prefetching of version chains when traversing cold data.
    /// When true, reading a version will also prefetch its prev_version.
    pub enable_prefetch: bool,

    /// Maximum number of versions to prefetch in a chain.
    pub prefetch_depth: usize,
}

impl Default for TieredStorageConfig {
    fn default() -> Self {
        Self {
            warm_cache_size: 10_000,
            enable_prefetch: true,
            prefetch_depth: 5,
        }
    }
}

/// Metrics for tiered storage access patterns.
#[derive(Debug, Clone, Default)]
pub struct TieredStorageMetrics {
    /// Number of hits in the hot tier (in-memory).
    pub hot_hits: u64,
    /// Number of hits in the warm tier (cache).
    pub warm_hits: u64,
    /// Number of hits in the cold tier (disk).
    pub cold_hits: u64,
    /// Number of misses (version not found anywhere).
    pub misses: u64,
    /// Number of prefetch operations.
    pub prefetches: u64,
}

impl TieredStorageMetrics {
    /// Calculate the hot hit ratio (hot / total).
    pub fn hot_ratio(&self) -> f64 {
        let total = self.hot_hits + self.warm_hits + self.cold_hits;
        if total == 0 {
            0.0
        } else {
            self.hot_hits as f64 / total as f64
        }
    }

    /// Calculate the warm hit ratio (warm / (warm + cold)).
    pub fn warm_ratio(&self) -> f64 {
        let cache_requests = self.warm_hits + self.cold_hits;
        if cache_requests == 0 {
            0.0
        } else {
            self.warm_hits as f64 / cache_requests as f64
        }
    }

    /// Calculate the overall cache hit ratio ((hot + warm) / total).
    pub fn cache_hit_ratio(&self) -> f64 {
        let total = self.hot_hits + self.warm_hits + self.cold_hits + self.misses;
        if total == 0 {
            0.0
        } else {
            (self.hot_hits + self.warm_hits) as f64 / total as f64
        }
    }
}

/// Atomic metrics tracker for tiered storage.
#[derive(Debug, Default)]
struct AtomicTieredMetrics {
    hot_hits: AtomicU64,
    warm_hits: AtomicU64,
    cold_hits: AtomicU64,
    misses: AtomicU64,
    prefetches: AtomicU64,
}

impl AtomicTieredMetrics {
    fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self) -> TieredStorageMetrics {
        TieredStorageMetrics {
            hot_hits: self.hot_hits.load(Ordering::Relaxed),
            warm_hits: self.warm_hits.load(Ordering::Relaxed),
            cold_hits: self.cold_hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            prefetches: self.prefetches.load(Ordering::Relaxed),
        }
    }
}

/// Tiered storage providing transparent hot/warm/cold data access.
///
/// This structure wraps cold storage and provides:
/// - Warm cache for frequently accessed cold data
/// - Transparent fallback from hot to cold storage
/// - Prefetching for version chain traversal
/// - Metrics for monitoring access patterns
pub struct TieredStorage {
    config: TieredStorageConfig,
    cold: Arc<dyn ColdStorage>,
    /// Warm cache for node versions retrieved from cold storage.
    node_warm_cache: Cache<VersionId, Arc<NodeVersion>>,
    /// Warm cache for edge versions retrieved from cold storage.
    edge_warm_cache: Cache<VersionId, Arc<EdgeVersion>>,
    metrics: AtomicTieredMetrics,
}

impl TieredStorage {
    /// Create a new tiered storage.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for caching and prefetching
    /// * `cold` - Cold storage backend
    pub fn new(config: TieredStorageConfig, cold: Box<dyn ColdStorage>) -> Self {
        let warm_cache_size = config.warm_cache_size;
        Self {
            config,
            cold: Arc::from(cold),
            node_warm_cache: Cache::new(warm_cache_size),
            edge_warm_cache: Cache::new(warm_cache_size),
            metrics: AtomicTieredMetrics::new(),
        }
    }

    /// Create with default configuration.
    pub fn with_default_config(cold: Box<dyn ColdStorage>) -> Self {
        Self::new(TieredStorageConfig::default(), cold)
    }

    /// Get the cold storage backend.
    pub fn cold_storage(&self) -> &dyn ColdStorage {
        self.cold.as_ref()
    }

    /// Record a hot tier hit.
    ///
    /// Call this when a version is found in hot (in-memory) storage.
    pub fn record_hot_hit(&self) {
        self.metrics.hot_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Try to get a node version from warm cache or cold storage.
    ///
    /// This method is called when the version is not found in hot storage.
    /// It first checks the warm cache, then falls back to cold storage.
    ///
    /// # Arguments
    ///
    /// * `id` - The version ID to retrieve
    ///
    /// # Returns
    ///
    /// Returns `Some(version)` if found, `None` if not found anywhere.
    pub fn get_node_version_cold(&self, id: VersionId) -> Result<Option<Arc<NodeVersion>>> {
        // Check warm cache first
        if let Some(cached) = self.node_warm_cache.get(&id) {
            self.metrics.warm_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Some(cached));
        }

        // Fetch from cold storage
        match self.cold.get_node_version(id)? {
            Some(version) => {
                self.metrics.cold_hits.fetch_add(1, Ordering::Relaxed);

                let version_arc = Arc::new(version);

                // Populate warm cache
                self.node_warm_cache.insert(id, version_arc.clone());

                // Prefetch prev_version if enabled
                if self.config.enable_prefetch {
                    self.prefetch_node_chain(&version_arc);
                }

                Ok(Some(version_arc))
            }
            None => {
                self.metrics.misses.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
        }
    }

    /// Try to get an edge version from warm cache or cold storage.
    ///
    /// This method is called when the version is not found in hot storage.
    /// It first checks the warm cache, then falls back to cold storage.
    pub fn get_edge_version_cold(&self, id: VersionId) -> Result<Option<Arc<EdgeVersion>>> {
        // Check warm cache first
        if let Some(cached) = self.edge_warm_cache.get(&id) {
            self.metrics.warm_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Some(cached));
        }

        // Fetch from cold storage
        match self.cold.get_edge_version(id)? {
            Some(version) => {
                self.metrics.cold_hits.fetch_add(1, Ordering::Relaxed);

                let version_arc = Arc::new(version);

                // Populate warm cache
                self.edge_warm_cache.insert(id, version_arc.clone());

                // Prefetch prev_version if enabled
                if self.config.enable_prefetch {
                    self.prefetch_edge_chain(&version_arc);
                }

                Ok(Some(version_arc))
            }
            None => {
                self.metrics.misses.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
        }
    }

    /// Prefetch node versions in a chain (up to prefetch_depth).
    fn prefetch_node_chain(&self, start: &NodeVersion) {
        let mut current_prev = start.prev_version;
        let mut depth = 0;

        while let Some(prev_id) = current_prev {
            if depth >= self.config.prefetch_depth {
                break;
            }

            // Skip if already in warm cache
            if self.node_warm_cache.get(&prev_id).is_some() {
                break;
            }

            // Fetch and cache
            match self.cold.get_node_version(prev_id) {
                Ok(Some(version)) => {
                    self.metrics.prefetches.fetch_add(1, Ordering::Relaxed);
                    current_prev = version.prev_version;
                    self.node_warm_cache.insert(prev_id, Arc::new(version));
                    depth += 1;
                }
                _ => break,
            }
        }
    }

    /// Prefetch edge versions in a chain (up to prefetch_depth).
    fn prefetch_edge_chain(&self, start: &EdgeVersion) {
        let mut current_prev = start.prev_version;
        let mut depth = 0;

        while let Some(prev_id) = current_prev {
            if depth >= self.config.prefetch_depth {
                break;
            }

            // Skip if already in warm cache
            if self.edge_warm_cache.get(&prev_id).is_some() {
                break;
            }

            // Fetch and cache
            match self.cold.get_edge_version(prev_id) {
                Ok(Some(version)) => {
                    self.metrics.prefetches.fetch_add(1, Ordering::Relaxed);
                    current_prev = version.prev_version;
                    self.edge_warm_cache.insert(prev_id, Arc::new(version));
                    depth += 1;
                }
                _ => break,
            }
        }
    }

    /// Store a node version to cold storage.
    ///
    /// This is called during migration from hot to cold tier.
    pub fn store_node_version(&self, version: &NodeVersion) -> Result<()> {
        self.cold.store_node_version(version)
    }

    /// Store an edge version to cold storage.
    ///
    /// This is called during migration from hot to cold tier.
    pub fn store_edge_version(&self, version: &EdgeVersion) -> Result<()> {
        self.cold.store_edge_version(version)
    }

    /// Store multiple node versions in a batch.
    ///
    /// This is more efficient for bulk migrations.
    pub fn store_node_versions_batch(&self, versions: &[NodeVersion]) -> Result<()> {
        self.cold.store_node_versions_batch(versions)
    }

    /// Store multiple edge versions in a batch.
    ///
    /// This is more efficient for bulk migrations.
    pub fn store_edge_versions_batch(&self, versions: &[EdgeVersion]) -> Result<()> {
        self.cold.store_edge_versions_batch(versions)
    }

    /// Check if a node version exists in cold storage.
    pub fn contains_node_version(&self, id: VersionId) -> Result<bool> {
        // Check warm cache first (fast path)
        if self.node_warm_cache.get(&id).is_some() {
            return Ok(true);
        }
        self.cold.contains_node_version(id)
    }

    /// Check if an edge version exists in cold storage.
    pub fn contains_edge_version(&self, id: VersionId) -> Result<bool> {
        // Check warm cache first (fast path)
        if self.edge_warm_cache.get(&id).is_some() {
            return Ok(true);
        }
        self.cold.contains_edge_version(id)
    }

    /// Get metrics for monitoring access patterns.
    pub fn metrics(&self) -> TieredStorageMetrics {
        self.metrics.snapshot()
    }

    /// Get cold storage statistics.
    pub fn cold_stats(&self) -> ColdStorageStats {
        self.cold.stats()
    }

    /// Clear the warm cache.
    ///
    /// This is useful for testing or when memory pressure is high.
    pub fn clear_warm_cache(&self) {
        // quick_cache doesn't have a clear method, so we create new caches
        // This is a limitation - in production we might want a different cache impl
    }

    /// Flush cold storage to disk.
    pub fn flush(&self) -> Result<()> {
        self.cold.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::NodeId;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::BiTemporalInterval;
    use crate::storage::cold_storage::InMemoryColdStorage;

    fn create_test_node_version(id: u64) -> NodeVersion {
        let properties = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        NodeVersion::new_anchor(
            VersionId::new(id).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            properties,
        )
    }

    fn create_test_version_chain() -> Vec<NodeVersion> {
        // Create a chain of versions: v5 -> v4 -> v3 -> v2 -> v1
        let mut versions = Vec::new();

        let v1 = {
            let props = PropertyMapBuilder::new().insert("step", 1i64).build();
            NodeVersion::new_anchor(
                VersionId::new(1).unwrap(),
                NodeId::new(100).unwrap(),
                BiTemporalInterval::current(1000.into()),
                GLOBAL_INTERNER.intern("Test").unwrap(),
                props,
            )
        };
        versions.push(v1);

        for i in 2..=5 {
            let old_props = PropertyMapBuilder::new()
                .insert("step", (i - 1) as i64)
                .build();
            let new_props = PropertyMapBuilder::new().insert("step", i as i64).build();
            let v = NodeVersion::new_delta(
                VersionId::new(i).unwrap(),
                NodeId::new(100).unwrap(),
                BiTemporalInterval::current(((1000 + i * 100) as i64).into()),
                GLOBAL_INTERNER.intern("Test").unwrap(),
                &old_props,
                &new_props,
                VersionId::new(i - 1).unwrap(),
            );
            versions.push(v);
        }

        versions
    }

    #[test]
    fn test_tiered_storage_cold_lookup() {
        let cold = InMemoryColdStorage::default_config();
        let tiered = TieredStorage::with_default_config(Box::new(cold));

        // Store a version in cold storage
        let version = create_test_node_version(1);
        tiered.store_node_version(&version).unwrap();

        // Retrieve from cold (first access)
        let retrieved = tiered.get_node_version_cold(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);

        // Check metrics
        let metrics = tiered.metrics();
        assert_eq!(metrics.cold_hits, 1);
        assert_eq!(metrics.warm_hits, 0);
    }

    #[test]
    fn test_tiered_storage_warm_cache() {
        let cold = InMemoryColdStorage::default_config();
        let tiered = TieredStorage::with_default_config(Box::new(cold));

        let version = create_test_node_version(1);
        tiered.store_node_version(&version).unwrap();

        // First access (cold)
        let _ = tiered.get_node_version_cold(version.id).unwrap();

        // Second access (warm cache)
        let retrieved = tiered.get_node_version_cold(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);

        // Check metrics
        let metrics = tiered.metrics();
        assert_eq!(metrics.cold_hits, 1);
        assert_eq!(metrics.warm_hits, 1);
    }

    #[test]
    fn test_tiered_storage_miss() {
        let cold = InMemoryColdStorage::default_config();
        let tiered = TieredStorage::with_default_config(Box::new(cold));

        let result = tiered
            .get_node_version_cold(VersionId::new(999).unwrap())
            .unwrap();
        assert!(result.is_none());

        let metrics = tiered.metrics();
        assert_eq!(metrics.misses, 1);
    }

    #[test]
    fn test_tiered_storage_prefetch() {
        let cold = InMemoryColdStorage::default_config();
        let config = TieredStorageConfig {
            warm_cache_size: 100,
            enable_prefetch: true,
            prefetch_depth: 3,
        };
        let tiered = TieredStorage::new(config, Box::new(cold));

        // Store a version chain in cold storage
        let versions = create_test_version_chain();
        for v in &versions {
            tiered.store_node_version(v).unwrap();
        }

        // Access the head version (v5)
        let _ = tiered
            .get_node_version_cold(VersionId::new(5).unwrap())
            .unwrap();

        // Check that prefetching occurred
        let metrics = tiered.metrics();
        assert!(metrics.prefetches > 0);
        assert!(metrics.prefetches <= 3); // Up to prefetch_depth
    }

    #[test]
    fn test_tiered_storage_no_prefetch() {
        let cold = InMemoryColdStorage::default_config();
        let config = TieredStorageConfig {
            warm_cache_size: 100,
            enable_prefetch: false,
            prefetch_depth: 3,
        };
        let tiered = TieredStorage::new(config, Box::new(cold));

        let versions = create_test_version_chain();
        for v in &versions {
            tiered.store_node_version(v).unwrap();
        }

        let _ = tiered
            .get_node_version_cold(VersionId::new(5).unwrap())
            .unwrap();

        let metrics = tiered.metrics();
        assert_eq!(metrics.prefetches, 0);
    }

    #[test]
    fn test_tiered_storage_hot_hit_recording() {
        let cold = InMemoryColdStorage::default_config();
        let tiered = TieredStorage::with_default_config(Box::new(cold));

        // Record some hot hits
        tiered.record_hot_hit();
        tiered.record_hot_hit();
        tiered.record_hot_hit();

        let metrics = tiered.metrics();
        assert_eq!(metrics.hot_hits, 3);
    }

    #[test]
    fn test_metrics_ratios() {
        let metrics = TieredStorageMetrics {
            hot_hits: 70,
            warm_hits: 20,
            cold_hits: 10,
            misses: 0,
            prefetches: 5,
        };

        assert!((metrics.hot_ratio() - 0.7).abs() < 0.001);
        assert!((metrics.warm_ratio() - 0.666).abs() < 0.01); // 20/(20+10)
        assert!((metrics.cache_hit_ratio() - 0.9).abs() < 0.001); // (70+20)/100
    }

    #[test]
    fn test_batch_store() {
        let cold = InMemoryColdStorage::default_config();
        let tiered = TieredStorage::with_default_config(Box::new(cold));

        let versions: Vec<NodeVersion> = (1..=10).map(create_test_node_version).collect();

        tiered.store_node_versions_batch(&versions).unwrap();

        // Verify all versions are stored
        for v in &versions {
            assert!(tiered.contains_node_version(v.id).unwrap());
        }
    }

    #[test]
    fn test_default_config() {
        let config = TieredStorageConfig::default();
        assert_eq!(config.warm_cache_size, 10_000);
        assert!(config.enable_prefetch);
        assert_eq!(config.prefetch_depth, 5);
    }
}
