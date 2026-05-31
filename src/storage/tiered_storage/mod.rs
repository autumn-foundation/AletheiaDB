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

use crate::core::error::Result;
use crate::core::id::VersionId;
use crate::core::version::{EdgeVersion, EntityVersion, NodeVersion};
use crate::storage::redb_cold_storage::{ColdStorageStats, RedbColdStorage};
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
mod tests;
