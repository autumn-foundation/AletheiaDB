//! Historical storage engine with anchor+delta compression.
//!
//! This module implements temporal versioning using version chains. Each node
//! and edge can have multiple versions over time, linked together in a chain
//! ordered by transaction time.
//!
//! The anchor+delta strategy minimizes storage overhead:
//! - Anchors are created every N versions (configurable)
//! - Deltas store only changed properties
//! - Reconstruction walks backward to nearest anchor and applies deltas forward
//! - TinyLFU cache reduces redundant delta chain traversals for concurrent reads

use crate::core::error::{Result, StorageError};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::observer::Observer;
use crate::core::property::PropertyMap;
use crate::core::version::{AnchorConfig, EdgeVersion, NodeVersion, VersionData};
use quick_cache::sync::Cache;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

// Submodules
/// Configuration and retention policies.
pub mod config;
/// Basic version lookup operations.
pub mod lookup;
/// Persistence layer integration.
pub mod persistence;
/// High-level temporal queries.
pub mod query;
/// Property reconstruction logic.
pub mod reconstruction;
/// Statistics and metrics.
pub mod stats;
/// Tiered storage integration.
pub mod tiered;
/// Write operations (adding versions).
pub mod write;

pub use config::{
    ANCHOR_CACHE_SIZE_RATIO, DEFAULT_MAX_VERSION_AGE_MS, DEFAULT_MAX_VERSIONS_PER_ENTITY,
    DEFAULT_RECONSTRUCTION_CACHE_SIZE, MAX_RECONSTRUCTION_DEPTH, MIN_ANCHOR_CACHE_SIZE,
    PreAnchorHook, RetentionPolicy,
};
pub use stats::{CacheMetrics, HistoricalStats};

/// Historical storage for versioned nodes and edges.
///
/// This storage engine maintains version chains for all temporal data,
/// using anchor+delta compression to minimize storage overhead.
/// A TinyLFU cache reduces redundant delta chain traversals for concurrent reads.
pub struct HistoricalStorage {
    /// Configuration for anchor creation strategy
    pub(crate) config: AnchorConfig,
    /// Retention policy for version pruning (DoS protection)
    pub(crate) retention_policy: RetentionPolicy,
    /// Maximum depth for version reconstruction (DoS protection)
    pub(crate) max_reconstruction_depth: usize,
    /// All node versions, indexed by version ID
    pub(crate) node_versions: HashMap<VersionId, NodeVersion>,
    /// All edge versions, indexed by version ID
    pub(crate) edge_versions: HashMap<VersionId, EdgeVersion>,
    /// Head version ID for each node (most recent)
    pub(crate) node_version_heads: HashMap<NodeId, VersionId>,
    /// Head version ID for each edge (most recent)
    pub(crate) edge_version_heads: HashMap<EdgeId, VersionId>,
    /// Cached version counts per node (for O(1) capacity checks)
    pub(crate) node_version_counts: HashMap<NodeId, usize>,
    /// Cached version counts per edge (for O(1) capacity checks)
    pub(crate) edge_version_counts: HashMap<EdgeId, usize>,
    /// Versions since last anchor per node (for O(1) anchor interval checks)
    pub(crate) node_versions_since_anchor: HashMap<NodeId, usize>,
    /// Versions since last anchor per edge (for O(1) anchor interval checks)
    pub(crate) edge_versions_since_anchor: HashMap<EdgeId, usize>,
    /// Cached count of node anchor versions for O(1) stats() (Issue #212)
    pub(crate) cached_node_anchor_count: usize,
    /// Cached count of node delta versions for O(1) stats() (Issue #212)
    pub(crate) cached_node_delta_count: usize,
    /// Cached count of edge anchor versions for O(1) stats() (Issue #212)
    pub(crate) cached_edge_anchor_count: usize,
    /// Cached count of edge delta versions for O(1) stats() (Issue #212)
    pub(crate) cached_edge_delta_count: usize,
    /// TinyLFU cache for reconstructed node properties (reduces lock contention)
    pub(crate) node_property_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// TinyLFU cache for reconstructed edge properties
    pub(crate) edge_property_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// Dedicated cache for node anchor properties.
    pub(crate) node_anchor_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// Dedicated cache for edge anchor properties.
    pub(crate) edge_anchor_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// Primary cache hit counter for adaptive sizing.
    pub(crate) primary_cache_hits: Arc<AtomicU64>,
    /// Anchor cache hit counter for adaptive sizing.
    pub(crate) anchor_cache_hits: Arc<AtomicU64>,
    /// Full reconstruction counter for adaptive sizing.
    pub(crate) full_reconstructions: Arc<AtomicU64>,
    /// Observers subscribed to storage events
    pub(crate) observers: Vec<Observer>,
    /// Pre-anchor hook for node anchors (called before storage).
    pub(crate) pre_node_anchor_hook: Option<PreAnchorHook>,
    /// Pre-anchor hook for edge anchors (called before storage).
    pub(crate) pre_edge_anchor_hook: Option<PreAnchorHook>,
    /// Optional tiered storage for cold data access.
    pub(crate) tiered_storage: Option<Arc<super::tiered_storage::TieredStorage>>,
    /// Temporal indexes for O(log n) version lookups (Issue #209).
    pub(crate) temporal_indexes: Option<Arc<crate::index::temporal::TemporalIndexes>>,
    /// Temporal adjacency index for fast temporal graph traversal.
    pub(crate) temporal_adjacency_index:
        Option<Arc<crate::index::temporal_adjacency::TemporalAdjacencyIndex>>,
}

impl HistoricalStorage {
    /// Create a new historical storage with default configuration.
    pub fn new() -> Self {
        Self::with_config(AnchorConfig::default())
    }

    /// Create a new historical storage with custom configuration.
    pub fn with_config(config: AnchorConfig) -> Self {
        Self::with_config_and_retention(config, RetentionPolicy::default())
    }

    /// Create a new historical storage with custom configuration and retention policy.
    pub fn with_config_and_retention(
        config: AnchorConfig,
        retention_policy: RetentionPolicy,
    ) -> Self {
        Self::with_config_retention_and_cache_size(
            config,
            retention_policy,
            DEFAULT_RECONSTRUCTION_CACHE_SIZE,
        )
    }

    /// Create a new historical storage from unified configuration.
    pub fn from_unified_config(config: crate::config::HistoricalConfig) -> Self {
        let retention_policy = RetentionPolicy::new(
            config.max_versions_per_entity,
            DEFAULT_MAX_VERSION_AGE_MS, // Keep existing default for age
        );

        let anchor_config = AnchorConfig {
            anchor_interval: config.anchor_interval,
            max_delta_chain: config.max_delta_chain,
        };

        let mut storage = Self::with_config_retention_and_cache_size(
            anchor_config,
            retention_policy,
            config.reconstruction_cache_size,
        );

        // Override max_reconstruction_depth from config
        storage.max_reconstruction_depth = config.max_reconstruction_depth;

        storage
    }

    /// Create a new historical storage with full customization including cache size.
    pub fn with_config_retention_and_cache_size(
        config: AnchorConfig,
        retention_policy: RetentionPolicy,
        cache_size: usize,
    ) -> Self {
        // Calculate anchor cache size: typically 10-20% of entities become anchors
        // depending on anchor_interval (Improvement #1: Issue #338)
        let anchor_cache_size = (cache_size / ANCHOR_CACHE_SIZE_RATIO).max(MIN_ANCHOR_CACHE_SIZE);

        HistoricalStorage {
            config,
            retention_policy,
            max_reconstruction_depth: MAX_RECONSTRUCTION_DEPTH,
            node_versions: HashMap::new(),
            edge_versions: HashMap::new(),
            node_version_heads: HashMap::new(),
            edge_version_heads: HashMap::new(),
            node_version_counts: HashMap::new(),
            edge_version_counts: HashMap::new(),
            node_versions_since_anchor: HashMap::new(),
            edge_versions_since_anchor: HashMap::new(),
            cached_node_anchor_count: 0,
            cached_node_delta_count: 0,
            cached_edge_anchor_count: 0,
            cached_edge_delta_count: 0,
            node_property_cache: Arc::new(Cache::new(cache_size)),
            edge_property_cache: Arc::new(Cache::new(cache_size)),
            node_anchor_cache: Arc::new(Cache::new(anchor_cache_size)),
            edge_anchor_cache: Arc::new(Cache::new(anchor_cache_size)),
            primary_cache_hits: Arc::new(AtomicU64::new(0)),
            anchor_cache_hits: Arc::new(AtomicU64::new(0)),
            full_reconstructions: Arc::new(AtomicU64::new(0)),
            observers: Vec::new(),
            pre_node_anchor_hook: None,
            pre_edge_anchor_hook: None,
            tiered_storage: None,
            temporal_indexes: None,
            temporal_adjacency_index: None,
        }
    }

    /// Add an observer to receive storage events.
    pub fn add_observer(&mut self, observer: Observer) {
        self.observers.push(observer);
    }

    /// Register a pre-anchor hook for nodes.
    pub fn register_pre_node_anchor_hook(&mut self, hook: PreAnchorHook) {
        self.pre_node_anchor_hook = Some(hook);
    }

    /// Register a pre-anchor hook for edges.
    pub fn register_pre_edge_anchor_hook(&mut self, hook: PreAnchorHook) {
        self.pre_edge_anchor_hook = Some(hook);
    }

    /// Extract version metadata and data for copy-out reconstruction (nodes).
    pub fn extract_node_version_data(
        &self,
        version_id: VersionId,
    ) -> Result<(VersionId, NodeId, InternedString, VersionData)> {
        let version = self
            .node_versions
            .get(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        // Clone the version data - these are cheap copies (Arc-based)
        Ok((
            version.id,
            version.node_id,
            version.label,
            version.data.clone(),
        ))
    }

    /// Extract version metadata and data for copy-out reconstruction (edges).
    pub fn extract_edge_version_data(
        &self,
        version_id: VersionId,
    ) -> Result<(
        VersionId,
        EdgeId,
        InternedString,
        NodeId,
        NodeId,
        VersionData,
    )> {
        let version = self
            .edge_versions
            .get(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        Ok((
            version.id,
            version.edge_id,
            version.label,
            version.source,
            version.target,
            version.data.clone(),
        ))
    }

    /// Check if the cache should be resized based on hit rate (Improvement #3).
    pub fn should_resize_cache(&self, threshold: f64, min_operations: u64) -> Option<f64> {
        let metrics = self.cache_metrics();
        let total = metrics.total_operations();

        // Need enough operations to make meaningful assessment
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
}

impl Default for HistoricalStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
