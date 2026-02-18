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
//! use aletheiadb::storage::tiered_storage::{TieredStorage, TieredStorageConfig};
//! use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
//! use std::sync::Arc;
//!
//! // Create tiered storage
//! let config = TieredStorageConfig::default();
//! let cold = RedbColdStorage::with_default_config("data/cold.redb")?;
//! let tiered = TieredStorage::new(config, Arc::new(cold));
//!
//! // Transparently access data from any tier
//! let version = tiered.get_node_version(version_id)?;
//! ```

use crate::core::id::VersionId;
use crate::core::version::{EdgeVersion, EntityVersion, NodeVersion};
use crate::storage::redb_cold_storage::{ColdStorageStats, RedbColdStorage};
use crate::utils::error::Result;
use parking_lot::Mutex;
use quick_cache::sync::Cache;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

/// Number of latency samples to keep for percentile calculation.
const LATENCY_SAMPLE_SIZE: usize = 1000;

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
    /// Cold read latency percentiles (p50, p95, p99).
    pub cold_latency: LatencyPercentiles,
}

/// Latency percentiles for cold storage reads.
#[derive(Debug, Clone, Default)]
pub struct LatencyPercentiles {
    /// 50th percentile (median) latency in microseconds.
    pub p50_us: u64,
    /// 95th percentile latency in microseconds.
    pub p95_us: u64,
    /// 99th percentile latency in microseconds.
    pub p99_us: u64,
    /// Minimum latency observed in microseconds.
    pub min_us: u64,
    /// Maximum latency observed in microseconds.
    pub max_us: u64,
    /// Average latency in microseconds.
    pub avg_us: u64,
    /// Number of samples used for calculation.
    pub sample_count: usize,
}

impl LatencyPercentiles {
    /// Check if latency meets the target (p50 < 1ms).
    pub fn meets_target(&self) -> bool {
        self.p50_us < 1000 // Less than 1ms
    }
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

/// Latency tracker for cold storage operations.
///
/// Uses a circular buffer to track recent latencies and compute percentiles.
#[derive(Debug)]
struct LatencyTracker {
    /// Circular buffer of latency samples (in microseconds).
    /// Uses VecDeque for O(1) pop_front() instead of Vec::remove(0)'s O(n).
    samples: Mutex<VecDeque<u64>>,
    /// Maximum number of samples to keep.
    max_samples: usize,
}

impl Default for LatencyTracker {
    fn default() -> Self {
        Self::new(LATENCY_SAMPLE_SIZE)
    }
}

impl LatencyTracker {
    /// Create a new latency tracker with the given sample size.
    fn new(max_samples: usize) -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(max_samples)),
            max_samples,
        }
    }

    /// Record a latency sample.
    fn record(&self, duration: Duration) {
        if self.max_samples == 0 {
            return;
        }
        let us = duration.as_micros() as u64;
        let mut samples = self.samples.lock();
        if samples.len() >= self.max_samples {
            samples.pop_front();
        }
        samples.push_back(us);
    }

    /// Calculate latency percentiles from the current samples.
    fn percentiles(&self) -> LatencyPercentiles {
        let samples = self.samples.lock();
        if samples.is_empty() {
            return LatencyPercentiles::default();
        }

        let mut sorted: Vec<u64> = samples.iter().copied().collect();
        sorted.sort_unstable();

        let len = sorted.len();
        let p50_idx = (len as f64 * 0.50) as usize;
        let p95_idx = (len as f64 * 0.95) as usize;
        let p99_idx = (len as f64 * 0.99) as usize;

        let sum: u64 = sorted.iter().sum();
        let avg = sum / len as u64;

        LatencyPercentiles {
            p50_us: sorted.get(p50_idx).copied().unwrap_or(0),
            p95_us: sorted.get(p95_idx.min(len - 1)).copied().unwrap_or(0),
            p99_us: sorted.get(p99_idx.min(len - 1)).copied().unwrap_or(0),
            min_us: sorted.first().copied().unwrap_or(0),
            max_us: sorted.last().copied().unwrap_or(0),
            avg_us: avg,
            sample_count: len,
        }
    }
}

/// Atomic metrics tracker for tiered storage.
#[derive(Debug)]
struct AtomicTieredMetrics {
    hot_hits: AtomicU64,
    warm_hits: AtomicU64,
    cold_hits: AtomicU64,
    misses: AtomicU64,
    prefetches: AtomicU64,
    cold_latency: LatencyTracker,
}

impl Default for AtomicTieredMetrics {
    fn default() -> Self {
        Self {
            hot_hits: AtomicU64::new(0),
            warm_hits: AtomicU64::new(0),
            cold_hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            prefetches: AtomicU64::new(0),
            cold_latency: LatencyTracker::default(),
        }
    }
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
            cold_latency: self.cold_latency.percentiles(),
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
    cold: Arc<RedbColdStorage>,
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
    pub fn new(config: TieredStorageConfig, cold: Arc<RedbColdStorage>) -> Self {
        let warm_cache_size = config.warm_cache_size;
        Self {
            config,
            cold,
            node_warm_cache: Cache::new(warm_cache_size),
            edge_warm_cache: Cache::new(warm_cache_size),
            metrics: AtomicTieredMetrics::new(),
        }
    }

    /// Create with default configuration.
    pub fn with_default_config(cold: Arc<RedbColdStorage>) -> Self {
        Self::new(TieredStorageConfig::default(), cold)
    }

    /// Get the cold storage backend.
    pub fn cold_storage(&self) -> &RedbColdStorage {
        &self.cold
    }

    /// Record a hot tier hit.
    ///
    /// Call this when a version is found in hot (in-memory) storage.
    pub fn record_hot_hit(&self) {
        self.metrics.hot_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Generic helper to get a version through the cache.
    fn get_version_through_cache<V, F>(
        &self,
        id: VersionId,
        cache: &Cache<VersionId, Arc<V>>,
        fetch_fn: F,
    ) -> Result<Option<Arc<V>>>
    where
        V: EntityVersion + 'static,
        F: Fn(VersionId) -> Result<Option<V>>,
    {
        // Check warm cache first
        if let Some(cached) = cache.get(&id) {
            self.metrics.warm_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Some(cached));
        }

        // Fetch from cold storage with latency tracking
        let start = Instant::now();
        let result = fetch_fn(id)?;
        let elapsed = start.elapsed();

        match result {
            Some(version) => {
                self.metrics.cold_hits.fetch_add(1, Ordering::Relaxed);
                self.metrics.cold_latency.record(elapsed);

                let version_arc = Arc::new(version);

                // Populate warm cache
                cache.insert(id, version_arc.clone());

                // Prefetch prev_version if enabled
                if self.config.enable_prefetch {
                    self.prefetch_chain(&*version_arc, cache, &fetch_fn);
                }

                Ok(Some(version_arc))
            }
            None => {
                self.metrics.misses.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
        }
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
        self.get_version_through_cache(id, &self.node_warm_cache, |id| {
            self.cold.get_node_version(id)
        })
    }

    /// Try to get an edge version from warm cache or cold storage.
    ///
    /// This method is called when the version is not found in hot storage.
    /// It first checks the warm cache, then falls back to cold storage.
    pub fn get_edge_version_cold(&self, id: VersionId) -> Result<Option<Arc<EdgeVersion>>> {
        self.get_version_through_cache(id, &self.edge_warm_cache, |id| {
            self.cold.get_edge_version(id)
        })
    }

    /// Prefetch versions in a chain (up to prefetch_depth).
    fn prefetch_chain<V, F>(&self, start: &V, cache: &Cache<VersionId, Arc<V>>, fetch_fn: &F)
    where
        V: EntityVersion + 'static,
        F: Fn(VersionId) -> Result<Option<V>>,
    {
        let mut current_prev = start.prev_version();
        let mut depth = 0;

        while let Some(prev_id) = current_prev {
            if depth >= self.config.prefetch_depth {
                break;
            }

            // Skip if already in warm cache
            if cache.get(&prev_id).is_some() {
                break;
            }

            // Fetch and cache
            match fetch_fn(prev_id) {
                Ok(Some(version)) => {
                    self.metrics.prefetches.fetch_add(1, Ordering::Relaxed);
                    current_prev = version.prev_version();
                    cache.insert(prev_id, Arc::new(version));
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

    /// Flush cold storage to disk.
    pub fn flush(&self) -> Result<()> {
        self.cold.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::{EdgeId, NodeId};
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::BiTemporalInterval;
    use crate::storage::redb_cold_storage::RedbColdStorage;

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

    fn create_test_edge_version(id: u64) -> EdgeVersion {
        let properties = PropertyMapBuilder::new().insert("weight", 1.0f64).build();

        EdgeVersion::new_anchor(
            VersionId::new(id).unwrap(),
            EdgeId::new(200).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(100).unwrap(),
            NodeId::new(101).unwrap(),
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
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

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
    fn test_tiered_storage_edge_cold_lookup() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        // Store a version in cold storage
        let version = create_test_edge_version(1);
        tiered.store_edge_version(&version).unwrap();

        // Retrieve from cold (first access)
        let retrieved = tiered.get_edge_version_cold(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);

        // Check metrics
        let metrics = tiered.metrics();
        assert_eq!(metrics.cold_hits, 1);
        assert_eq!(metrics.warm_hits, 0);
    }

    #[test]
    fn test_tiered_storage_edge_warm_cache() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        let version = create_test_edge_version(1);
        tiered.store_edge_version(&version).unwrap();

        // First access (cold)
        let _ = tiered.get_edge_version_cold(version.id).unwrap();

        // Second access (warm cache)
        let retrieved = tiered.get_edge_version_cold(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);

        // Check metrics
        let metrics = tiered.metrics();
        assert_eq!(metrics.cold_hits, 1);
        assert_eq!(metrics.warm_hits, 1);
    }

    #[test]
    fn test_tiered_storage_warm_cache() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

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
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        let result = tiered
            .get_node_version_cold(VersionId::new(999).unwrap())
            .unwrap();
        assert!(result.is_none());

        let metrics = tiered.metrics();
        assert_eq!(metrics.misses, 1);
    }

    #[test]
    fn test_tiered_storage_prefetch() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

        let config = TieredStorageConfig {
            warm_cache_size: 100,
            enable_prefetch: true,
            prefetch_depth: 3,
        };
        let tiered = TieredStorage::new(config, Arc::new(cold));

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
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

        let config = TieredStorageConfig {
            warm_cache_size: 100,
            enable_prefetch: false,
            prefetch_depth: 3,
        };
        let tiered = TieredStorage::new(config, Arc::new(cold));

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
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

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
            cold_latency: LatencyPercentiles::default(),
        };

        assert!((metrics.hot_ratio() - 0.7).abs() < 0.001);
        assert!((metrics.warm_ratio() - 0.666).abs() < 0.01); // 20/(20+10)
        assert!((metrics.cache_hit_ratio() - 0.9).abs() < 0.001); // (70+20)/100
    }

    #[test]
    fn test_batch_store() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

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

    // ========================================================================
    // Latency tracking tests
    // ========================================================================

    #[test]
    fn test_latency_tracker_empty() {
        let tracker = LatencyTracker::new(100);
        let percentiles = tracker.percentiles();

        assert_eq!(percentiles.sample_count, 0);
        assert_eq!(percentiles.p50_us, 0);
        assert_eq!(percentiles.p95_us, 0);
        assert_eq!(percentiles.p99_us, 0);
    }

    #[test]
    fn test_latency_tracker_zero_max_samples() {
        let tracker = LatencyTracker::new(0);
        tracker.record(Duration::from_micros(500));

        let percentiles = tracker.percentiles();
        assert_eq!(percentiles.sample_count, 0);
        assert_eq!(percentiles.p50_us, 0);
    }

    #[test]
    fn test_latency_percentiles_even_samples() {
        // Verify behavior for even number of samples (e.g. 4)
        // Values: 10, 20, 30, 40
        // Median (p50):
        // Index = 4 * 0.5 = 2.0 -> index 2 -> value 30.
        // Usually median is (20+30)/2 = 25, but this implementation picks upper middle.

        let tracker = LatencyTracker::new(10);
        tracker.record(Duration::from_micros(10));
        tracker.record(Duration::from_micros(20));
        tracker.record(Duration::from_micros(30));
        tracker.record(Duration::from_micros(40));

        let percentiles = tracker.percentiles();
        assert_eq!(percentiles.sample_count, 4);
        assert_eq!(percentiles.p50_us, 30);
    }

    #[test]
    fn test_latency_tracker_single_sample() {
        let tracker = LatencyTracker::new(100);
        tracker.record(Duration::from_micros(500));

        let percentiles = tracker.percentiles();
        assert_eq!(percentiles.sample_count, 1);
        assert_eq!(percentiles.p50_us, 500);
        assert_eq!(percentiles.min_us, 500);
        assert_eq!(percentiles.max_us, 500);
    }

    #[test]
    fn test_latency_tracker_percentiles() {
        let tracker = LatencyTracker::new(100);

        // Add 100 samples with values 1-100 microseconds
        for i in 1..=100 {
            tracker.record(Duration::from_micros(i));
        }

        let percentiles = tracker.percentiles();
        assert_eq!(percentiles.sample_count, 100);
        assert_eq!(percentiles.min_us, 1);
        assert_eq!(percentiles.max_us, 100);
        // p50 at index 50 (0-indexed) = value 51
        assert_eq!(percentiles.p50_us, 51);
        // p95 at index 95 = value 96
        assert_eq!(percentiles.p95_us, 96);
        // p99 at index 99 = value 100
        assert_eq!(percentiles.p99_us, 100);
        // avg = sum(1..=100)/100 = 5050/100 = 50
        assert_eq!(percentiles.avg_us, 50);
    }

    #[test]
    fn test_latency_tracker_circular_buffer() {
        let tracker = LatencyTracker::new(10);

        // Add 20 samples, only last 10 should be kept
        for i in 1..=20 {
            tracker.record(Duration::from_micros(i));
        }

        let percentiles = tracker.percentiles();
        assert_eq!(percentiles.sample_count, 10);
        assert_eq!(percentiles.min_us, 11); // Oldest kept is 11
        assert_eq!(percentiles.max_us, 20);
    }

    #[test]
    fn test_latency_percentiles_meets_target() {
        let good = LatencyPercentiles {
            p50_us: 500,
            p95_us: 900,
            p99_us: 950,
            min_us: 100,
            max_us: 1000,
            avg_us: 500,
            sample_count: 100,
        };
        assert!(good.meets_target());

        let bad = LatencyPercentiles {
            p50_us: 1500, // Exceeds 1ms target
            p95_us: 2000,
            p99_us: 3000,
            min_us: 500,
            max_us: 5000,
            avg_us: 1500,
            sample_count: 100,
        };
        assert!(!bad.meets_target());
    }

    #[test]
    fn test_cold_latency_tracking() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        // Store some versions
        for i in 1..=10 {
            let version = create_test_node_version(i);
            tiered.store_node_version(&version).unwrap();
        }

        // Access them from cold storage (first access bypasses warm cache)
        for i in 1..=10 {
            let _ = tiered
                .get_node_version_cold(VersionId::new(i).unwrap())
                .unwrap();
        }

        let metrics = tiered.metrics();
        assert_eq!(metrics.cold_hits, 10);
        assert!(metrics.cold_latency.sample_count > 0);
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        // Wrap TieredStorage in Arc for thread safety
        let tiered = Arc::new(TieredStorage::with_default_config(Arc::new(cold)));

        // Store a version first so threads have something to read
        let version = create_test_node_version(1);
        tiered.store_node_version(&version).unwrap();

        let mut handles = vec![];

        // Reduce concurrency to avoid CI resource exhaustion (especially on Windows)
        for _ in 0..4 {
            let tiered_clone = Arc::clone(&tiered);
            let handle = thread::spawn(move || {
                for _ in 0..50 {
                    // All threads hammer the same version
                    let _ = tiered_clone
                        .get_node_version_cold(VersionId::new(1).unwrap())
                        .unwrap();
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let metrics = tiered.metrics();
        // Total operations = 4 threads * 50 loops = 200
        // One cold hit (first access), rest warm hits
        // Note: Due to race conditions on initial cache population, multiple threads might
        // miss the cache and hit cold storage simultaneously.
        assert_eq!(metrics.cold_hits + metrics.warm_hits, 200);
        assert!(metrics.cold_hits >= 1);

        // Verify latency tracker recorded samples safely
        assert!(metrics.cold_latency.sample_count > 0);
    }

    #[test]
    fn test_prefetch_limits() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Chain of 10 versions
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

        for i in 2..=10 {
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

        let config = TieredStorageConfig {
            warm_cache_size: 100,
            enable_prefetch: true,
            prefetch_depth: 3,
        };
        let tiered = TieredStorage::new(config, Arc::new(cold));

        for v in &versions {
            tiered.store_node_version(v).unwrap();
        }

        // Fetch head (v10)
        let _ = tiered
            .get_node_version_cold(VersionId::new(10).unwrap())
            .unwrap();

        let metrics = tiered.metrics();
        // Prefetches should be exactly 3: v9, v8, v7
        assert_eq!(metrics.prefetches, 3);

        // Verify what is in cache implicitly by checking hit counts

        // Fetch v9 -> warm hit (was prefetched)
        let _ = tiered
            .get_node_version_cold(VersionId::new(9).unwrap())
            .unwrap();
        assert_eq!(tiered.metrics().warm_hits, 1);

        // Fetch v7 -> warm hit (was prefetched)
        let _ = tiered
            .get_node_version_cold(VersionId::new(7).unwrap())
            .unwrap();
        assert_eq!(tiered.metrics().warm_hits, 2);

        // Fetch v6 -> cold hit (was NOT prefetched because depth=3 from v10 covers v9, v8, v7)
        let _ = tiered
            .get_node_version_cold(VersionId::new(6).unwrap())
            .unwrap();

        let final_metrics = tiered.metrics();
        assert_eq!(final_metrics.warm_hits, 2); // Unchanged
        // Initial v10 + v6 = 2 cold hits
        assert_eq!(final_metrics.cold_hits, 2);
    }

    #[test]
    fn test_cache_eviction() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Use a small cache
        let config = TieredStorageConfig {
            warm_cache_size: 5,
            enable_prefetch: false,
            prefetch_depth: 0,
        };
        let tiered = TieredStorage::new(config, Arc::new(cold));

        // Store 10 versions
        for i in 1..=10 {
            let version = create_test_node_version(i);
            tiered.store_node_version(&version).unwrap();
        }

        // Access all 10 versions to populate cache
        // This will cause 10 cold hits.
        for i in 1..=10 {
            tiered
                .get_node_version_cold(VersionId::new(i).unwrap())
                .unwrap();
        }

        // Now access them again. Since cache size is 5, we expect at most 5 warm hits
        // (the recently accessed ones), and at least 5 cold hits (the evicted ones).
        let metrics_before = tiered.metrics();

        for i in 1..=10 {
            tiered
                .get_node_version_cold(VersionId::new(i).unwrap())
                .unwrap();
        }

        let metrics_after = tiered.metrics();
        let warm_hits = metrics_after.warm_hits - metrics_before.warm_hits;
        let cold_hits = metrics_after.cold_hits - metrics_before.cold_hits;

        assert!(
            warm_hits <= 5,
            "Expected at most 5 warm hits, got {}",
            warm_hits
        );
        assert!(
            cold_hits >= 5,
            "Expected at least 5 cold hits, got {}",
            cold_hits
        );
    }

    #[test]
    fn test_error_propagation_writes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        // Enable write failure injection
        tiered.cold_storage().set_fail_writes(true);

        let version = create_test_node_version(1);
        let result = tiered.store_node_version(&version);

        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("Simulated write failure"));

        // Also test batch store
        let result = tiered.store_node_versions_batch(&[version]);
        assert!(result.is_err());
    }

    #[test]
    fn test_prefetch_latency_metric_behavior() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

        let config = TieredStorageConfig {
            warm_cache_size: 100,
            enable_prefetch: true,
            prefetch_depth: 3,
        };
        let tiered = TieredStorage::new(config, Arc::new(cold));

        // Create a chain of versions: v4 -> v3 -> v2 -> v1
        let mut versions = Vec::new();
        let v1 = create_test_node_version(1);
        versions.push(v1);

        for i in 2..=4 {
            let v = NodeVersion::new_delta(
                VersionId::new(i).unwrap(),
                NodeId::new(100).unwrap(),
                BiTemporalInterval::current(((1000 + i * 100) as i64).into()),
                GLOBAL_INTERNER.intern("Test").unwrap(),
                &PropertyMapBuilder::new().build(),
                &PropertyMapBuilder::new().build(),
                VersionId::new(i - 1).unwrap(),
            );
            versions.push(v);
        }

        // Store all in cold storage
        for v in &versions {
            tiered.store_node_version(v).unwrap();
        }

        // Access the head version (v4)
        // This should trigger:
        // 1. Fetch v4 (cold miss) -> records latency
        // 2. Prefetch v3 (cold) -> DOES NOT record latency in cold_latency metric
        // 3. Prefetch v2 (cold) -> DOES NOT record latency
        // 4. Prefetch v1 (cold) -> DOES NOT record latency
        let _ = tiered
            .get_node_version_cold(VersionId::new(4).unwrap())
            .unwrap();

        let metrics = tiered.metrics();

        // Verify we only have 1 latency sample despite doing 4 disk reads
        assert_eq!(
            metrics.cold_latency.sample_count, 1,
            "Should record latency only for the requested item, not prefetches"
        );

        // Verify we did prefetch
        assert_eq!(metrics.prefetches, 3, "Should have prefetched 3 items");

        // Verify cache state: all 4 should be in warm cache
        for i in 1..=4 {
            // Since we just accessed them (or prefetched them), they should be in warm cache
            // Note: get_node_version_cold increments warm_hits if found
            // We expect returns Ok(Some(_))
            assert!(
                tiered
                    .get_node_version_cold(VersionId::new(i).unwrap())
                    .unwrap()
                    .is_some()
            );
        }

        let final_metrics = tiered.metrics();
        // Initial cold hit (v4) + 4 warm hits (checking v1..v4)
        // Total cold hits: 1 (v4 initial)
        // Total warm hits: 4 (subsequent checks)
        assert_eq!(final_metrics.cold_hits, 1);
        assert_eq!(final_metrics.warm_hits, 4);
    }
}
