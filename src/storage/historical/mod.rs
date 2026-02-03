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

use crate::core::graph::{Edge, Node};
use crate::core::history::{EntityHistory, VersionDiff, VersionInfo};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::observer::{Observer, StorageEvent, notify_observers};
use crate::core::property::PropertyMap;
use crate::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, Timestamp};
use crate::core::version::{
    AnchorConfig, EdgeVersion, EntityVersion, NodeVersion, TemporalVersion, VersionData,
};
use crate::utils::error::{Result, StorageError, TemporalError};
use quick_cache::sync::Cache;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "observability")]
use tracing;

/// Default maximum number of versions per entity (DoS protection)
pub const DEFAULT_MAX_VERSIONS_PER_ENTITY: usize = 1_000;

/// Default maximum age for versions in milliseconds (365 days)
pub const DEFAULT_MAX_VERSION_AGE_MS: i64 = 365 * 24 * 60 * 60 * 1000;

/// Maximum recursion depth for version reconstruction (DoS protection).
///
/// This limit prevents stack overflow from corrupted version chains or cycles.
/// A depth of 100 is sufficient for any legitimate use case since anchors are
/// typically created every 10-20 versions.
pub const MAX_RECONSTRUCTION_DEPTH: usize = 100;

/// Retention policy for version history (DoS protection).
///
/// Controls how many versions are kept and for how long to prevent
/// unbounded memory growth from malicious or buggy clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Maximum number of versions to keep per entity
    pub max_versions_per_entity: usize,
    /// Maximum age of versions in milliseconds (older versions are pruned)
    pub max_age_ms: i64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        RetentionPolicy {
            max_versions_per_entity: DEFAULT_MAX_VERSIONS_PER_ENTITY,
            max_age_ms: DEFAULT_MAX_VERSION_AGE_MS,
        }
    }
}

impl RetentionPolicy {
    /// Create a new retention policy with custom limits
    pub fn new(max_versions_per_entity: usize, max_age_ms: i64) -> Self {
        RetentionPolicy {
            max_versions_per_entity,
            max_age_ms,
        }
    }

    /// Create a policy with no retention limits (unbounded)
    pub fn unbounded() -> Self {
        RetentionPolicy {
            max_versions_per_entity: usize::MAX,
            max_age_ms: i64::MAX,
        }
    }
}

/// Pre-anchor hook for creating snapshots before anchor storage.
///
/// This hook is called **before** storing an anchor to create synchronized snapshots
/// and return a snapshot ID to be stored atomically with the anchor. This enables
/// strong consistency for provenance tracking between graph versioning and vector indexing.
///
/// # Arguments
///
/// * `entity_type` - Type of entity ("node" or "edge")
/// * `entity_id` - ID of the entity as u64
/// * `timestamp` - Transaction timestamp when the anchor is being created
/// * `properties` - Property map that will be stored in the anchor
///
/// # Returns
///
/// * `Ok(Some(snapshot_id))` - Snapshot created successfully, link to anchor
/// * `Ok(None)` - No snapshot needed or empty index
/// * `Err(e)` - Snapshot creation failed (anchor will still be created, graceful degradation)
///
/// # Examples
///
/// ```ignore
/// use gallifreydb::storage::historical::PreAnchorHook;
/// use std::sync::Arc;
///
/// let hook: PreAnchorHook = Arc::new(|entity_type, entity_id, timestamp, properties| {
///     // Create vector snapshot before anchor is stored
///     let snapshot_id = create_vector_snapshot(timestamp, properties)?;
///     Ok(Some(snapshot_id))
/// });
/// ```
pub type PreAnchorHook = Arc<
    dyn Fn(
            /* entity_type */ &str,
            /* entity_id */ u64,
            /* timestamp */ Timestamp,
            /* properties */ &PropertyMap,
        ) -> Result<Option<usize>>
        + Send
        + Sync,
>;

/// Context for pre-anchor hook invocation.
///
/// Groups the parameters needed to invoke a pre-anchor hook, improving
/// readability and reducing the number of function parameters.
struct AnchorHookContext<'a> {
    /// Entity type ("node" or "edge")
    entity_type: &'a str,
    /// Entity ID as u64
    entity_id: u64,
    /// Transaction timestamp
    timestamp: Timestamp,
    /// Properties being stored in the anchor
    properties: &'a PropertyMap,
}

/// Default cache size for reconstructed properties (10,000 entries)
const DEFAULT_RECONSTRUCTION_CACHE_SIZE: usize = 10_000;

/// Anchor cache size ratio relative to main cache (Improvement #1: Issue #338).
///
/// Typically 10-20% of versions become anchors depending on `anchor_interval`.
/// With default interval of 10, we get ~10% anchors. Setting to 1/5 (20%)
/// provides headroom for configurations with smaller intervals.
const ANCHOR_CACHE_SIZE_RATIO: usize = 5; // 20% of main cache

/// Minimum anchor cache size to ensure reasonable performance (Improvement #1: Issue #338).
///
/// Even with very small main caches, we want enough anchor cache to hold
/// at least a few anchors to avoid immediate evictions.
const MIN_ANCHOR_CACHE_SIZE: usize = 100;

/// Historical storage for versioned nodes and edges.
///
/// This storage engine maintains version chains for all temporal data,
/// using anchor+delta compression to minimize storage overhead.
/// A TinyLFU cache reduces redundant delta chain traversals for concurrent reads.
pub struct HistoricalStorage {
    /// Configuration for anchor creation strategy
    config: AnchorConfig,
    /// Retention policy for version pruning (DoS protection)
    retention_policy: RetentionPolicy,
    /// Maximum depth for version reconstruction (DoS protection)
    max_reconstruction_depth: usize,
    /// All node versions, indexed by version ID
    node_versions: HashMap<VersionId, NodeVersion>,
    /// All edge versions, indexed by version ID
    edge_versions: HashMap<VersionId, EdgeVersion>,
    /// Head version ID for each node (most recent)
    node_version_heads: HashMap<NodeId, VersionId>,
    /// Head version ID for each edge (most recent)
    edge_version_heads: HashMap<EdgeId, VersionId>,
    /// Cached version counts per node (for O(1) capacity checks)
    node_version_counts: HashMap<NodeId, usize>,
    /// Cached version counts per edge (for O(1) capacity checks)
    edge_version_counts: HashMap<EdgeId, usize>,
    /// Versions since last anchor per node (for O(1) anchor interval checks)
    ///
    /// Issue #208: Cache the count of versions since the last anchor to avoid
    /// walking the version chain on every add operation. This improves write
    /// performance from O(anchor_interval) to O(1).
    node_versions_since_anchor: HashMap<NodeId, usize>,
    /// Versions since last anchor per edge (for O(1) anchor interval checks)
    ///
    /// Issue #208: Cache the count of versions since the last anchor to avoid
    /// walking the version chain on every add operation. This improves write
    /// performance from O(anchor_interval) to O(1).
    edge_versions_since_anchor: HashMap<EdgeId, usize>,
    /// Cached count of node anchor versions for O(1) stats() (Issue #212)
    ///
    /// This counter is incremented when node anchors are added and enables
    /// constant-time stats retrieval instead of O(versions) iteration.
    cached_node_anchor_count: usize,
    /// Cached count of node delta versions for O(1) stats() (Issue #212)
    ///
    /// This counter is incremented when node deltas are added and enables
    /// constant-time stats retrieval instead of O(versions) iteration.
    cached_node_delta_count: usize,
    /// Cached count of edge anchor versions for O(1) stats() (Issue #212)
    ///
    /// This counter is incremented when edge anchors are added and enables
    /// constant-time stats retrieval instead of O(versions) iteration.
    cached_edge_anchor_count: usize,
    /// Cached count of edge delta versions for O(1) stats() (Issue #212)
    ///
    /// This counter is incremented when edge deltas are added and enables
    /// constant-time stats retrieval instead of O(versions) iteration.
    cached_edge_delta_count: usize,
    /// TinyLFU cache for reconstructed node properties (reduces lock contention)
    node_property_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// TinyLFU cache for reconstructed edge properties
    edge_property_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// Improvement #1: Dedicated cache for node anchor properties.
    ///
    /// This separate cache ensures anchors are never evicted by delta cache pressure,
    /// providing guaranteed O(1) access to anchors even under heavy load. Anchors are
    /// frequently reused as base points for delta reconstruction, so keeping them cached
    /// reduces average reconstruction cost from O(N) to O(M) where M << N.
    node_anchor_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// Improvement #1: Dedicated cache for edge anchor properties.
    ///
    /// This separate cache ensures anchors are never evicted by delta cache pressure,
    /// providing guaranteed O(1) access to anchors even under heavy load.
    edge_anchor_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// Improvement #3: Primary cache hit counter for adaptive sizing.
    ///
    /// Tracks successful lookups in the primary property cache (fast path).
    /// This is the most common case for recently accessed properties.
    primary_cache_hits: Arc<AtomicU64>,
    /// Improvement #3: Anchor cache hit counter for adaptive sizing.
    ///
    /// Tracks successful lookups in the dedicated anchor cache (fallback path).
    /// High values indicate anchors are being evicted from primary cache under
    /// delta pressure, suggesting the primary cache may need to be larger.
    anchor_cache_hits: Arc<AtomicU64>,
    /// Improvement #3: Full reconstruction counter for adaptive sizing.
    ///
    /// Tracks cache misses requiring full property reconstruction from deltas.
    /// High values indicate insufficient cache capacity overall.
    full_reconstructions: Arc<AtomicU64>,
    /// Observers subscribed to storage events
    ///
    /// Multiple components can observe storage events (anchors, deletes, etc.)
    /// for indexing, metrics, logging, or coordination. Observers are notified
    /// asynchronously and errors don't block storage operations.
    observers: Vec<Observer>,
    /// Pre-anchor hook for node anchors (called before storage).
    ///
    /// This hook is called **before** storing a node anchor to create synchronized
    /// snapshots. The returned snapshot ID is stored atomically with the anchor,
    /// enabling strong consistency for provenance tracking.
    pre_node_anchor_hook: Option<PreAnchorHook>,
    /// Pre-anchor hook for edge anchors (called before storage).
    ///
    /// This hook is called **before** storing an edge anchor to create synchronized
    /// snapshots. The returned snapshot ID is stored atomically with the anchor,
    /// enabling strong consistency for provenance tracking.
    pre_edge_anchor_hook: Option<PreAnchorHook>,
    /// Optional tiered storage for cold data access.
    ///
    /// When configured, versions not found in hot storage will be looked up
    /// from cold storage via the tiered storage layer.
    tiered_storage: Option<Arc<super::tiered_storage::TieredStorage>>,
    /// Temporal indexes for O(log n) version lookups (Issue #209).
    ///
    /// When available, `find_node_version_at_time` and `find_edge_version_at_time`
    /// use these indexes for efficient binary search instead of O(n) linear scans
    /// through version chains. This is particularly important for entities with
    /// long version histories (100s-1000s of versions).
    ///
    /// The temporal indexes are maintained externally by the database and shared
    /// with HistoricalStorage for query optimization.
    temporal_indexes: Option<Arc<crate::index::temporal::TemporalIndexes>>,
    /// Temporal adjacency index for fast temporal graph traversal.
    ///
    /// When available, pathfinding queries can efficiently find edges that existed
    /// at specific points in time, including edges that have been deleted.
    temporal_adjacency_index: Option<Arc<crate::index::temporal_adjacency::TemporalAdjacencyIndex>>,
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
    ///
    /// This constructor accepts `crate::config::HistoricalConfig` which consolidates
    /// all historical storage settings (max_versions, max_reconstruction_depth, cache_size).
    ///
    /// # Example
    /// ```ignore
    /// use gallifreydb::config::{HistoricalConfig, HistoricalConfigBuilder};
    /// use gallifreydb::storage::historical::HistoricalStorage;
    ///
    /// let config = HistoricalConfigBuilder::new()
    ///     .max_versions_per_entity(5000).unwrap()
    ///     .max_reconstruction_depth(200).unwrap()
    ///     .reconstruction_cache_size(20000).unwrap()
    ///     .build();
    ///
    /// let storage = HistoricalStorage::from_unified_config(config);
    /// ```
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
    ///
    /// # Arguments
    /// * `config` - Anchor creation configuration
    /// * `retention_policy` - Version retention limits (DoS protection)
    /// * `cache_size` - Maximum number of cached property reconstructions per type (node/edge)
    ///
    /// # Cache Sizing
    /// Consider your workload when sizing the cache:
    /// - Small properties (no vectors): 1,000-10,000 entries
    /// - Large properties (1536-dim vectors ~6KB): 100-1,000 entries
    /// - Default: 10,000 entries
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
    ///
    /// Observers are notified of storage events (anchors, deletes, etc.) for indexing,
    /// metrics, logging, or coordination. Multiple observers can be registered, and all
    /// will be notified of events they're interested in.
    ///
    /// # Arguments
    /// * `observer` - Component implementing the StorageObserver trait
    ///
    /// # Example
    /// ```no_run
    /// # use gallifreydb::storage::historical::HistoricalStorage;
    /// # use gallifreydb::core::observer::{StorageObserver, StorageEvent};
    /// # use std::sync::Arc;
    /// struct VectorIndexObserver;
    ///
    /// impl StorageObserver for VectorIndexObserver {
    ///     fn on_event(&self, event: &StorageEvent) -> gallifreydb::utils::Result<()> {
    ///         match event {
    ///             StorageEvent::NodeAnchorCreated { version_id, timestamp, .. } => {
    ///                 println!("Anchor {} created at {}", version_id, timestamp);
    ///                 Ok(())
    ///             }
    ///             _ => Ok(())
    ///         }
    ///     }
    /// }
    ///
    /// let mut storage = HistoricalStorage::new();
    /// storage.add_observer(Arc::new(VectorIndexObserver));
    /// ```
    pub fn add_observer(&mut self, observer: Observer) {
        self.observers.push(observer);
    }

    /// Register a pre-anchor hook for nodes.
    ///
    /// This hook is called **before** storing a node anchor, allowing the hook
    /// to create synchronized snapshots and return a snapshot ID to be stored
    /// atomically with the anchor. This enables strong consistency for provenance
    /// tracking between graph versioning and vector indexing.
    ///
    /// # Arguments
    /// * `hook` - Function that creates snapshots and returns snapshot IDs
    ///
    /// # Hook Signature
    /// The hook receives:
    /// * `entity_type` - Always "node" for this hook
    /// * `entity_id` - Node ID as u64
    /// * `timestamp` - Transaction timestamp
    /// * `properties` - Property map being stored in the anchor
    ///
    /// The hook returns:
    /// * `Ok(Some(snapshot_id))` - Snapshot created, link to anchor
    /// * `Ok(None)` - No snapshot needed (e.g., empty index)
    /// * `Err(e)` - Error (anchor still created, graceful degradation)
    ///
    /// # Example
    /// ```ignore
    /// use gallifreydb::storage::historical::PreAnchorHook;
    /// use std::sync::Arc;
    ///
    /// let hook: PreAnchorHook = Arc::new(|_entity_type, _entity_id, timestamp, _properties| {
    ///     // Create vector snapshot before anchor storage
    ///     let snapshot_id = temporal_vector_index.create_snapshot_for_anchor(timestamp)?;
    ///     Ok(snapshot_id)
    /// });
    ///
    /// let mut storage = HistoricalStorage::new();
    /// storage.register_pre_node_anchor_hook(hook);
    /// ```
    pub fn register_pre_node_anchor_hook(&mut self, hook: PreAnchorHook) {
        self.pre_node_anchor_hook = Some(hook);
    }

    /// Register a pre-anchor hook for edges.
    ///
    /// This hook is called **before** storing an edge anchor, allowing the hook
    /// to create synchronized snapshots and return a snapshot ID to be stored
    /// atomically with the anchor. This enables strong consistency for provenance
    /// tracking between graph versioning and vector indexing.
    ///
    /// # Arguments
    /// * `hook` - Function that creates snapshots and returns snapshot IDs
    ///
    /// # Hook Signature
    /// The hook receives:
    /// * `entity_type` - Always "edge" for this hook
    /// * `entity_id` - Edge ID as u64
    /// * `timestamp` - Transaction timestamp
    /// * `properties` - Property map being stored in the anchor
    ///
    /// The hook returns:
    /// * `Ok(Some(snapshot_id))` - Snapshot created, link to anchor
    /// * `Ok(None)` - No snapshot needed (e.g., empty index)
    /// * `Err(e)` - Error (anchor still created, graceful degradation)
    ///
    /// # Example
    /// ```ignore
    /// use gallifreydb::storage::historical::PreAnchorHook;
    /// use std::sync::Arc;
    ///
    /// let hook: PreAnchorHook = Arc::new(|_entity_type, _entity_id, timestamp, _properties| {
    ///     // Create vector snapshot before anchor storage
    ///     let snapshot_id = temporal_vector_index.create_snapshot_for_anchor(timestamp)?;
    ///     Ok(snapshot_id)
    /// });
    ///
    /// let mut storage = HistoricalStorage::new();
    /// storage.register_pre_edge_anchor_hook(hook);
    /// ```
    pub fn register_pre_edge_anchor_hook(&mut self, hook: PreAnchorHook) {
        self.pre_edge_anchor_hook = Some(hook);
    }

    /// Add a new version of a node.
    ///
    /// This will automatically determine whether to create an anchor or delta
    /// based on the version chain length.
    /// Returns an error if the version limit for this entity is exceeded (DoS protection).
    #[allow(clippy::too_many_arguments)]
    pub fn add_node_version(
        &mut self,
        node_id: NodeId,
        version_id: VersionId,
        valid_from: Timestamp,
        tx_time: Timestamp,
        label: InternedString,
        properties: PropertyMap,
        is_tombstone: bool,
    ) -> Result<()> {
        // Construct bi-temporal interval from separate dimensions
        let mut temporal = BiTemporalInterval::with_valid_time(valid_from, tx_time);

        // For tombstones, close the valid_time at valid_from to create an empty interval [valid_from, valid_from)
        // This represents "entity is no longer valid starting from this point"
        if is_tombstone {
            temporal = temporal.close_valid_time(valid_from);
        }

        // Check capacity limit using cached count (O(1) operation, DoS protection)
        let version_count = self.node_version_counts.get(&node_id).copied().unwrap_or(0);
        if version_count >= self.retention_policy.max_versions_per_entity {
            return Err(StorageError::CapacityExceeded {
                resource: format!("node {} versions", node_id),
                current: version_count,
                limit: self.retention_policy.max_versions_per_entity,
            }
            .into());
        }

        // Check if this node already has versions
        let prev_version_id = self.node_version_heads.get(&node_id).copied();

        // Create version (anchor or delta based on chain length)
        let mut version = if let Some(prev_id) = prev_version_id {
            // Verify previous version exists (properties reconstructed later via reconstruct_node_properties)
            if !self.node_versions.contains_key(&prev_id) {
                return Err(StorageError::VersionNotFound(prev_id).into());
            }

            // Get cached counter and increment (O(1) instead of O(anchor_interval))
            // Issue #208: Use cached counter to avoid walking version chain
            let current_count = self
                .node_versions_since_anchor
                .get(&node_id)
                .copied()
                .unwrap_or(0);
            let versions_since_anchor = current_count + 1;

            if versions_since_anchor >= self.config.anchor_interval as usize {
                // Create anchor with link to previous version
                // Use properties.clone() here as we need original for caching later
                let mut anchor = NodeVersion::new_anchor(
                    version_id,
                    node_id,
                    temporal,
                    label,
                    properties.clone(),
                );
                anchor.prev_version = Some(prev_id);
                // Reset counter to 0 after creating anchor
                self.node_versions_since_anchor.insert(node_id, 0);
                anchor
            } else {
                // Create delta from previous version
                let old_properties = self.reconstruct_node_properties(prev_id)?;
                // Update counter for next iteration
                self.node_versions_since_anchor
                    .insert(node_id, versions_since_anchor);
                NodeVersion::new_delta(
                    version_id,
                    node_id,
                    temporal,
                    label,
                    &old_properties,
                    &properties,
                    prev_id,
                )
            }
        } else {
            // First version is always an anchor
            // Initialize counter to 0
            self.node_versions_since_anchor.insert(node_id, 0);
            NodeVersion::new_anchor(version_id, node_id, temporal, label, properties.clone())
        };

        // Handle pre-anchor hook (BEFORE storing)
        if version.is_anchor() {
            Self::handle_pre_anchor_hook(
                AnchorHookContext {
                    entity_type: "node",
                    entity_id: node_id.as_u64(),
                    timestamp: temporal.transaction_time().start(),
                    properties: &properties,
                },
                &mut version.data,
                &self.pre_node_anchor_hook,
            );
        }

        // Link previous version to this one and close its temporal intervals
        if let Some(prev_id) = prev_version_id
            && let Some(prev) = self.node_versions.get_mut(&prev_id)
        {
            // Capture the intervals before modification for temporal index update
            let old_temporal = *prev.temporal();

            Self::close_previous_version_intervals(prev, version_id, &temporal);

            // Update temporal indexes to reflect the closed intervals (Issue #209)
            if let Some(ref indexes) = self.temporal_indexes {
                let new_temporal = *prev.temporal();

                // Update valid time end if it was closed
                if old_temporal.valid_time().end() != new_temporal.valid_time().end() {
                    indexes.update_node_valid_time_end(
                        node_id,
                        prev_id,
                        new_temporal.valid_time().end(),
                    );
                }

                // Update transaction time end if it was closed
                if old_temporal.transaction_time().end() != new_temporal.transaction_time().end() {
                    indexes.update_node_transaction_time_end(
                        node_id,
                        prev_id,
                        new_temporal.transaction_time().end(),
                    );
                }
            }
        }

        // Check if anchor before storing (for notifications and caching)
        let is_anchor = version.is_anchor();

        // Store the version and update indexes
        self.node_versions.insert(version_id, version);
        self.node_version_heads.insert(node_id, version_id);
        *self.node_version_counts.entry(node_id).or_insert(0) += 1;

        // Issue #212: Update cached stats counters for O(1) stats() retrieval
        if is_anchor {
            self.cached_node_anchor_count += 1;
        } else {
            self.cached_node_delta_count += 1;
        }

        // Issue #210: Cache properties for ALL versions (anchors and deltas) to avoid
        // reconstructing properties we just added when creating the next delta.
        //
        // BEFORE: Only anchors were cached, causing delta creation to reconstruct
        //         the previous delta's properties even though we just added them.
        // AFTER:  All versions are cached in the main property cache, eliminating
        //         unnecessary reconstructions during consecutive writes.
        let props_arc = Arc::new(properties);
        self.node_property_cache
            .insert(version_id, props_arc.clone());

        // Anchors are also cached in the dedicated anchor cache for fallback
        if is_anchor {
            self.node_anchor_cache.insert(version_id, props_arc);
        }

        // Notify observers
        let timestamp = temporal.transaction_time().start();
        notify_observers(
            &self.observers,
            &StorageEvent::NodeVersionCreated {
                version_id,
                node_id,
                timestamp,
                is_anchor,
            },
        );
        if is_anchor {
            notify_observers(
                &self.observers,
                &StorageEvent::NodeAnchorCreated {
                    version_id,
                    node_id,
                    timestamp,
                },
            );
        }

        Ok(())
    }

    /// Add a new version of an edge.
    /// Returns an error if the version limit for this entity is exceeded (DoS protection).
    #[allow(clippy::too_many_arguments)]
    pub fn add_edge_version(
        &mut self,
        edge_id: EdgeId,
        version_id: VersionId,
        valid_from: Timestamp,
        tx_time: Timestamp,
        label: InternedString,
        source: NodeId,
        target: NodeId,
        properties: PropertyMap,
        is_tombstone: bool,
    ) -> Result<()> {
        // Construct bi-temporal interval from separate dimensions
        let mut temporal = BiTemporalInterval::with_valid_time(valid_from, tx_time);

        // For tombstones, close the valid_time at valid_from to create an empty interval [valid_from, valid_from)
        // This represents "entity is no longer valid starting from this point"
        if is_tombstone {
            temporal = temporal.close_valid_time(valid_from);
        }

        // Check capacity limit using cached count (O(1) operation, DoS protection)
        let version_count = self.edge_version_counts.get(&edge_id).copied().unwrap_or(0);
        if version_count >= self.retention_policy.max_versions_per_entity {
            return Err(StorageError::CapacityExceeded {
                resource: format!("edge {} versions", edge_id),
                current: version_count,
                limit: self.retention_policy.max_versions_per_entity,
            }
            .into());
        }

        // Check if this edge already has versions
        let prev_version_id = self.edge_version_heads.get(&edge_id).copied();

        // Create version (anchor or delta based on chain length)
        let mut version = if let Some(prev_id) = prev_version_id {
            // Verify previous version exists (properties reconstructed later via reconstruct_edge_properties)
            if !self.edge_versions.contains_key(&prev_id) {
                return Err(StorageError::VersionNotFound(prev_id).into());
            }

            // Get cached counter and increment (O(1) instead of O(anchor_interval))
            // Issue #208: Use cached counter to avoid walking version chain
            let current_count = self
                .edge_versions_since_anchor
                .get(&edge_id)
                .copied()
                .unwrap_or(0);
            let versions_since_anchor = current_count + 1;

            if versions_since_anchor >= self.config.anchor_interval as usize {
                // Create anchor with link to previous version
                // Use properties.clone() here as we need original for caching later
                let mut anchor = EdgeVersion::new_anchor(
                    version_id,
                    edge_id,
                    temporal,
                    label,
                    source,
                    target,
                    properties.clone(),
                );
                anchor.prev_version = Some(prev_id);
                // Reset counter to 0 after creating anchor
                self.edge_versions_since_anchor.insert(edge_id, 0);
                anchor
            } else {
                // Create delta from previous version
                let old_properties = self.reconstruct_edge_properties(prev_id)?;
                // Update counter for next iteration
                self.edge_versions_since_anchor
                    .insert(edge_id, versions_since_anchor);
                EdgeVersion::new_delta(
                    version_id,
                    edge_id,
                    temporal,
                    label,
                    source,
                    target,
                    &old_properties,
                    &properties,
                    prev_id,
                )
            }
        } else {
            // First version is always an anchor
            // Initialize counter to 0
            self.edge_versions_since_anchor.insert(edge_id, 0);
            EdgeVersion::new_anchor(
                version_id,
                edge_id,
                temporal,
                label,
                source,
                target,
                properties.clone(),
            )
        };

        // Handle pre-anchor hook (BEFORE storing)
        if version.is_anchor() {
            Self::handle_pre_anchor_hook(
                AnchorHookContext {
                    entity_type: "edge",
                    entity_id: edge_id.as_u64(),
                    timestamp: temporal.transaction_time().start(),
                    properties: &properties,
                },
                &mut version.data,
                &self.pre_edge_anchor_hook,
            );
        }

        // Link previous version to this one and close its temporal intervals
        if let Some(prev_id) = prev_version_id
            && let Some(prev) = self.edge_versions.get_mut(&prev_id)
        {
            // Capture the intervals before modification for temporal index update
            let old_temporal = *prev.temporal();

            Self::close_previous_version_intervals(prev, version_id, &temporal);

            // Update temporal indexes to reflect the closed intervals (Issue #209)
            if let Some(ref indexes) = self.temporal_indexes {
                let new_temporal = *prev.temporal();

                // Update valid time end if it was closed
                if old_temporal.valid_time().end() != new_temporal.valid_time().end() {
                    indexes.update_edge_valid_time_end(
                        edge_id,
                        prev_id,
                        new_temporal.valid_time().end(),
                    );
                }

                // Update transaction time end if it was closed
                if old_temporal.transaction_time().end() != new_temporal.transaction_time().end() {
                    indexes.update_edge_transaction_time_end(
                        edge_id,
                        prev_id,
                        new_temporal.transaction_time().end(),
                    );
                }
            }

            // Update temporal adjacency index to reflect closed valid time
            if let Some(ref adj_index) = self.temporal_adjacency_index {
                let new_temporal = *prev.temporal();
                if old_temporal.valid_time().end() != new_temporal.valid_time().end() {
                    adj_index.close_edge_valid_time(
                        edge_id,
                        source,
                        target,
                        new_temporal.valid_time().end(),
                    );
                }
            }
        }

        // Check if anchor before storing (for notifications and caching)
        let is_anchor = version.is_anchor();

        // Store the version and update indexes
        self.edge_versions.insert(version_id, version);
        self.edge_version_heads.insert(edge_id, version_id);
        *self.edge_version_counts.entry(edge_id).or_insert(0) += 1;

        // Issue #212: Update cached stats counters for O(1) stats() retrieval
        if is_anchor {
            self.cached_edge_anchor_count += 1;
        } else {
            self.cached_edge_delta_count += 1;
        }

        // Issue #210: Cache properties for ALL versions (anchors and deltas) to avoid
        // reconstructing properties we just added when creating the next delta.
        let props_arc = Arc::new(properties);
        self.edge_property_cache
            .insert(version_id, props_arc.clone());

        // Anchors are also cached in the dedicated anchor cache for fallback
        if is_anchor {
            self.edge_anchor_cache.insert(version_id, props_arc);
        }

        // Notify observers
        let timestamp = temporal.transaction_time().start();
        notify_observers(
            &self.observers,
            &StorageEvent::EdgeVersionCreated {
                version_id,
                edge_id,
                timestamp,
                is_anchor,
            },
        );
        if is_anchor {
            notify_observers(
                &self.observers,
                &StorageEvent::EdgeAnchorCreated {
                    version_id,
                    edge_id,
                    timestamp,
                },
            );
        }

        // Update temporal adjacency index if configured
        // Insert after all operations complete so temporal intervals are finalized
        // Skip tombstones - they represent deletions and shouldn't appear in traversal queries
        if !is_tombstone
            && let Some(ref adj_index) = self.temporal_adjacency_index
            && let Err(_e) = adj_index.insert_edge(
                edge_id,
                source,
                target,
                label,
                temporal.valid_time().start(),
                temporal.valid_time().end(),
                temporal.transaction_time().start(),
                temporal.transaction_time().end(),
            )
        {
            #[cfg(feature = "observability")]
            tracing::warn!(
                edge_id = %edge_id,
                source = %source,
                target = %target,
                error = %_e,
                "Failed to insert edge into temporal adjacency index"
            );
        }

        Ok(())
    }

    /// Reconstruct the properties of a node version.
    ///
    /// This walks backward to find the nearest anchor, then applies all deltas
    /// forward to reconstruct the full property state.
    ///
    /// **Cache Behavior**: Properties are cached by VersionId. Since properties are
    /// immutable per version and temporal visibility is checked separately in
    /// `find_node_version_at_time()`, cached properties are always valid and don't
    /// require invalidation when temporal intervals are modified.
    ///
    /// **Depth Limit**: Returns `TemporalError::MaxDepthExceeded` if the delta
    /// chain exceeds `MAX_RECONSTRUCTION_DEPTH` (100). This protects against
    /// stack overflow from corrupted version chains or cycles.
    pub fn reconstruct_node_properties(&self, version_id: VersionId) -> Result<PropertyMap> {
        self.reconstruct_node_properties_with_depth(version_id, 0)
    }

    /// Get a node version from hot or cold storage.
    ///
    /// This is a helper for reconstruction that checks hot storage first (fast path),
    /// then falls back to tiered storage for cold data access.
    ///
    /// Returns `Err(VersionNotFound)` if the version doesn't exist in any tier.
    #[inline]
    fn get_node_version_any_tier(&self, version_id: VersionId) -> Result<Arc<NodeVersion>> {
        if let Some(v) = self.node_versions.get(&version_id) {
            // Fast path: version in hot storage
            Ok(Arc::new(v.clone()))
        } else {
            // Slow path: check cold storage via tiered layer
            self.get_node_version_tiered(version_id)?
                .ok_or(StorageError::VersionNotFound(version_id).into())
        }
    }

    /// Get an edge version from hot or cold storage.
    ///
    /// This is a helper for reconstruction that checks hot storage first (fast path),
    /// then falls back to tiered storage for cold data access.
    ///
    /// Returns `Err(VersionNotFound)` if the version doesn't exist in any tier.
    #[inline]
    fn get_edge_version_any_tier(&self, version_id: VersionId) -> Result<Arc<EdgeVersion>> {
        if let Some(v) = self.edge_versions.get(&version_id) {
            // Fast path: version in hot storage
            Ok(Arc::new(v.clone()))
        } else {
            // Slow path: check cold storage via tiered layer
            self.get_edge_version_tiered(version_id)?
                .ok_or(StorageError::VersionNotFound(version_id).into())
        }
    }

    /// Iterative property reconstruction helper for nodes (Issue #211).
    ///
    /// This function implements the core iterative reconstruction algorithm.
    /// It eliminates intermediate PropertyMap allocations and stack overflow risks.
    ///
    /// # Algorithm
    /// 1. Collect version IDs backwards from target to anchor (O(anchor_interval) IDs)
    /// 2. Extract anchor properties as base state
    /// 3. Apply deltas in forward order (O(anchor_interval) delta applications)
    ///
    /// # Arguments
    /// * `version_id` - The version to reconstruct properties for
    ///
    /// # Returns
    /// * `Ok(PropertyMap)` - Reconstructed properties
    /// * `Err(TemporalError::MaxDepthExceeded)` - Delta chain too deep (DoS protection)
    /// * `Err(StorageError::VersionNotFound)` - Version not found
    /// * `Err(TemporalError::CorruptedVersionChain)` - Invalid chain structure
    fn reconstruct_node_properties_iterative(&self, version_id: VersionId) -> Result<PropertyMap> {
        // Collect version IDs backwards from target to anchor
        // Pre-allocate with anchor_interval capacity to avoid reallocations
        let mut version_ids: Vec<VersionId> =
            Vec::with_capacity(self.config.anchor_interval as usize);
        let mut current_id = version_id;
        let mut chain_length = 0;

        // Walk backwards until we find an anchor or hit depth limit
        loop {
            // Check depth limit for DoS protection
            if chain_length >= self.max_reconstruction_depth {
                let entity_id = self
                    .node_versions
                    .get(&version_id)
                    .map(|v| v.node_id.to_string())
                    .unwrap_or_else(|| format!("version {}", version_id));
                return Err(TemporalError::MaxDepthExceeded {
                    max_depth: MAX_RECONSTRUCTION_DEPTH,
                    entity_id,
                }
                .into());
            }

            let version = self.get_node_version_any_tier(current_id)?;

            let is_anchor = version.is_anchor();
            let prev_id = version.prev_version;

            // Store version ID (we'll process these in reverse)
            version_ids.push(current_id);

            // If we found an anchor, we're done collecting
            if is_anchor {
                break;
            }

            // Get previous version for delta chain traversal
            current_id = prev_id.ok_or_else(|| TemporalError::CorruptedVersionChain {
                entity_id: version.node_id.to_string(),
                reason: "Delta version has no previous version".to_string(),
            })?;

            chain_length += 1;
        }

        // Now reconstruct properties by applying deltas in forward order
        // The last element in version_ids is the anchor (base state)
        let anchor_id =
            version_ids
                .last()
                .copied()
                .ok_or_else(|| TemporalError::CorruptedVersionChain {
                    entity_id: format!("version {}", version_id),
                    reason: "Empty version chain during reconstruction".to_string(),
                })?;

        let anchor_version = self.get_node_version_any_tier(anchor_id)?;

        let mut properties = match &anchor_version.data {
            VersionData::Anchor { properties, .. } => properties.clone(),
            VersionData::Delta { .. } => {
                // This should never happen due to the is_anchor() check above
                return Err(TemporalError::CorruptedVersionChain {
                    entity_id: anchor_version.node_id.to_string(),
                    reason: "Expected anchor at base of version chain".to_string(),
                }
                .into());
            }
        };

        // Apply deltas in forward order (reverse of collection order)
        // Skip the last element (anchor) since we already have its properties
        for &vid in version_ids.iter().rev().skip(1) {
            let version = self.get_node_version_any_tier(vid)?;

            match &version.data {
                VersionData::Delta { delta } => {
                    properties = delta.apply(&properties);
                }
                VersionData::Anchor { .. } => {
                    // This should never happen - only the last element should be an anchor
                    return Err(TemporalError::CorruptedVersionChain {
                        entity_id: version.node_id.to_string(),
                        reason: "Found anchor in middle of delta chain".to_string(),
                    }
                    .into());
                }
            }
        }

        Ok(properties)
    }

    /// Iterative property reconstruction helper for edges (Issue #211).
    ///
    /// Mirrors the node reconstruction algorithm for consistency. See
    /// `reconstruct_node_properties_iterative` for algorithm details.
    fn reconstruct_edge_properties_iterative(&self, version_id: VersionId) -> Result<PropertyMap> {
        // Collect version IDs backwards from target to anchor
        // Pre-allocate with anchor_interval capacity to avoid reallocations
        let mut version_ids: Vec<VersionId> =
            Vec::with_capacity(self.config.anchor_interval as usize);
        let mut current_id = version_id;
        let mut chain_length = 0;

        // Walk backwards until we find an anchor or hit depth limit
        loop {
            // Check depth limit for DoS protection
            if chain_length >= self.max_reconstruction_depth {
                let entity_id = self
                    .edge_versions
                    .get(&version_id)
                    .map(|v| v.edge_id.to_string())
                    .unwrap_or_else(|| format!("version {}", version_id));
                return Err(TemporalError::MaxDepthExceeded {
                    max_depth: MAX_RECONSTRUCTION_DEPTH,
                    entity_id,
                }
                .into());
            }

            let version = self.get_edge_version_any_tier(current_id)?;

            let is_anchor = version.is_anchor();
            let prev_id = version.prev_version;

            // Store version ID (we'll process these in reverse)
            version_ids.push(current_id);

            // If we found an anchor, we're done collecting
            if is_anchor {
                break;
            }

            // Get previous version for delta chain traversal
            current_id = prev_id.ok_or_else(|| TemporalError::CorruptedVersionChain {
                entity_id: version.edge_id.to_string(),
                reason: "Delta version has no previous version".to_string(),
            })?;

            chain_length += 1;
        }

        // Now reconstruct properties by applying deltas in forward order
        // The last element in version_ids is the anchor (base state)
        let anchor_id =
            version_ids
                .last()
                .copied()
                .ok_or_else(|| TemporalError::CorruptedVersionChain {
                    entity_id: format!("version {}", version_id),
                    reason: "Empty version chain during reconstruction".to_string(),
                })?;

        let anchor_version = self.get_edge_version_any_tier(anchor_id)?;

        let mut properties = match &anchor_version.data {
            VersionData::Anchor { properties, .. } => properties.clone(),
            VersionData::Delta { .. } => {
                // This should never happen due to the is_anchor() check above
                return Err(TemporalError::CorruptedVersionChain {
                    entity_id: anchor_version.edge_id.to_string(),
                    reason: "Expected anchor at base of version chain".to_string(),
                }
                .into());
            }
        };

        // Apply deltas in forward order (reverse of collection order)
        // Skip the last element (anchor) since we already have its properties
        for &vid in version_ids.iter().rev().skip(1) {
            let version = self.get_edge_version_any_tier(vid)?;

            match &version.data {
                VersionData::Delta { delta } => {
                    properties = delta.apply(&properties);
                }
                VersionData::Anchor { .. } => {
                    // This should never happen - only the last element should be an anchor
                    return Err(TemporalError::CorruptedVersionChain {
                        entity_id: version.edge_id.to_string(),
                        reason: "Found anchor in middle of delta chain".to_string(),
                    }
                    .into());
                }
            }
        }

        Ok(properties)
    }

    /// Internal helper for node property reconstruction with depth tracking.
    ///
    /// Note (Issue #211): The iterative implementation only caches the final
    /// reconstructed PropertyMap, not intermediate versions. This reduces memory
    /// allocations at the cost of slightly lower cache hit rates compared to
    /// the previous recursive approach.
    ///
    /// The depth parameter is kept for API compatibility but unused in the
    /// iterative implementation.
    fn reconstruct_node_properties_with_depth(
        &self,
        version_id: VersionId,
        _depth: usize, // Kept for API compatibility but unused in iterative implementation
    ) -> Result<PropertyMap> {
        // Dual-cache lookup strategy (Improvement #1 & #2: Issue #338)
        //
        // 1. Check regular cache first (holds all versions: anchors + deltas)
        // 2. If not found, check dedicated anchor cache (holds only anchors)
        //
        // Anchors are stored in BOTH caches during pre-population for redundancy:
        // - Regular cache provides fast access when anchor is still in LRU window
        // - Anchor cache acts as fallback when regular cache evicts due to delta pressure
        //
        // This fallback only triggers after regular cache eviction, providing
        // guaranteed O(1) anchor access even under heavy cache pressure.
        if let Some(cached) = self.node_property_cache.get(&version_id) {
            self.primary_cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(cached.as_ref().clone());
        }

        // Fallback to dedicated anchor cache (survives delta cache pressure)
        if let Some(cached) = self.node_anchor_cache.get(&version_id) {
            self.anchor_cache_hits.fetch_add(1, Ordering::Relaxed);
            // Re-populate main cache to make this anchor "hot" again
            // This prevents repeatedly falling back to anchor cache for frequently accessed anchors
            self.node_property_cache.insert(version_id, cached.clone());
            return Ok(cached.as_ref().clone());
        }

        // Cache miss - reconstruct properties using iterative helper
        self.full_reconstructions.fetch_add(1, Ordering::Relaxed);

        let properties = self.reconstruct_node_properties_iterative(version_id)?;

        // Populate cache for future reads
        self.node_property_cache
            .insert(version_id, Arc::new(properties.clone()));

        Ok(properties)
    }

    /// Reconstruct the properties of an edge version.
    ///
    /// **Cache Behavior**: Same as `reconstruct_node_properties()` - properties are
    /// immutable per VersionId, so caching doesn't require invalidation.
    ///
    /// **Depth Limit**: Returns `TemporalError::MaxDepthExceeded` if the delta
    /// chain exceeds `MAX_RECONSTRUCTION_DEPTH` (100). This protects against
    /// stack overflow from corrupted version chains or cycles.
    pub fn reconstruct_edge_properties(&self, version_id: VersionId) -> Result<PropertyMap> {
        self.reconstruct_edge_properties_with_depth(version_id, 0)
    }

    /// Internal helper for edge property reconstruction with depth tracking.
    ///
    /// Note (Issue #211): The iterative implementation only caches the final
    /// reconstructed PropertyMap, not intermediate versions. This reduces memory
    /// allocations at the cost of slightly lower cache hit rates compared to
    /// the previous recursive approach.
    ///
    /// The depth parameter is kept for API compatibility but unused in the
    /// iterative implementation.
    fn reconstruct_edge_properties_with_depth(
        &self,
        version_id: VersionId,
        _depth: usize, // Kept for API compatibility but unused in iterative implementation
    ) -> Result<PropertyMap> {
        // Dual-cache lookup strategy (Improvement #1 & #2: Issue #338)
        //
        // 1. Check regular cache first (holds all versions: anchors + deltas)
        // 2. If not found, check dedicated anchor cache (holds only anchors)
        //
        // Anchors are stored in BOTH caches during pre-population for redundancy:
        // - Regular cache provides fast access when anchor is still in LRU window
        // - Anchor cache acts as fallback when regular cache evicts due to delta pressure
        //
        // This fallback only triggers after regular cache eviction, providing
        // guaranteed O(1) anchor access even under heavy cache pressure.
        if let Some(cached) = self.edge_property_cache.get(&version_id) {
            self.primary_cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(cached.as_ref().clone());
        }

        // Fallback to dedicated anchor cache (survives delta cache pressure)
        if let Some(cached) = self.edge_anchor_cache.get(&version_id) {
            self.anchor_cache_hits.fetch_add(1, Ordering::Relaxed);
            // Re-populate main cache to make this anchor "hot" again
            // This prevents repeatedly falling back to anchor cache for frequently accessed anchors
            self.edge_property_cache.insert(version_id, cached.clone());
            return Ok(cached.as_ref().clone());
        }

        // Cache miss - reconstruct properties using iterative helper
        self.full_reconstructions.fetch_add(1, Ordering::Relaxed);

        let properties = self.reconstruct_edge_properties_iterative(version_id)?;

        // Populate cache for future reads
        self.edge_property_cache
            .insert(version_id, Arc::new(properties.clone()));

        Ok(properties)
    }

    /// Get a node version by ID.
    pub fn get_node_version(&self, version_id: VersionId) -> Option<&NodeVersion> {
        self.node_versions.get(&version_id)
    }

    /// Get an edge version by ID.
    pub fn get_edge_version(&self, version_id: VersionId) -> Option<&EdgeVersion> {
        self.edge_versions.get(&version_id)
    }

    /// Get the current version ID for a node.
    pub fn get_current_node_version(&self, node_id: NodeId) -> Option<VersionId> {
        self.node_version_heads.get(&node_id).copied()
    }

    /// Get the current version ID for an edge.
    pub fn get_current_edge_version(&self, edge_id: EdgeId) -> Option<VersionId> {
        self.edge_version_heads.get(&edge_id).copied()
    }

    /// Get all node versions for all nodes.
    ///
    /// Returns a map of NodeId -> `Vec<NodeVersion>` for recovery property tests.
    /// This walks through all node versions and groups them by entity ID.
    pub fn get_all_node_versions(&self) -> std::collections::HashMap<NodeId, Vec<&NodeVersion>> {
        let mut result: std::collections::HashMap<NodeId, Vec<&NodeVersion>> =
            std::collections::HashMap::new();

        for version in self.node_versions.values() {
            result.entry(version.node_id).or_default().push(version);
        }

        result
    }

    /// Get all edge versions for all edges.
    ///
    /// Returns a map of EdgeId -> `Vec<EdgeVersion>` for recovery property tests.
    /// This walks through all edge versions and groups them by entity ID.
    pub fn get_all_edge_versions(&self) -> std::collections::HashMap<EdgeId, Vec<&EdgeVersion>> {
        let mut result: std::collections::HashMap<EdgeId, Vec<&EdgeVersion>> =
            std::collections::HashMap::new();

        for version in self.edge_versions.values() {
            result.entry(version.edge_id).or_default().push(version);
        }

        result
    }

    // ========================================================================
    // Tiered Storage Integration
    // ========================================================================

    /// Configure tiered storage for this historical storage.
    ///
    /// When tiered storage is configured, versions not found in hot storage
    /// will be looked up from cold storage via the tiered storage layer.
    ///
    /// # Arguments
    ///
    /// * `tiered` - The tiered storage instance to use
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::storage::historical::HistoricalStorage;
    /// use gallifreydb::storage::tiered_storage::TieredStorage;
    /// use gallifreydb::storage::redb_cold_storage::RedbColdStorage;
    ///
    /// let mut historical = HistoricalStorage::new();
    /// let cold = RedbColdStorage::with_default_config("data/cold.redb")?;
    /// let tiered = TieredStorage::with_default_config(Box::new(cold));
    /// historical.set_tiered_storage(Arc::new(tiered));
    /// ```
    pub fn set_tiered_storage(&mut self, tiered: Arc<super::tiered_storage::TieredStorage>) {
        self.tiered_storage = Some(tiered);
    }

    /// Get the tiered storage instance, if configured.
    pub fn tiered_storage(&self) -> Option<&super::tiered_storage::TieredStorage> {
        self.tiered_storage.as_deref()
    }

    /// Check if tiered storage is enabled.
    pub fn has_tiered_storage(&self) -> bool {
        self.tiered_storage.is_some()
    }

    /// Set the temporal indexes for optimized version lookups (Issue #209).
    ///
    /// When temporal indexes are configured, `find_node_version_at_time` and
    /// `find_edge_version_at_time` will use O(log n) binary search instead of
    /// O(n) linear scans through version chains.
    ///
    /// This is typically called during database initialization to share the
    /// temporal indexes between the database and historical storage.
    pub fn set_temporal_indexes(&mut self, indexes: Arc<crate::index::temporal::TemporalIndexes>) {
        self.temporal_indexes = Some(indexes);
    }

    /// Set the temporal adjacency index for this storage.
    ///
    /// When the temporal adjacency index is set, it will be automatically updated
    /// when edges are added or modified, enabling efficient temporal pathfinding
    /// queries that can find paths through deleted edges.
    ///
    /// This is typically called during database initialization.
    pub fn set_temporal_adjacency_index(
        &mut self,
        index: Arc<crate::index::temporal_adjacency::TemporalAdjacencyIndex>,
    ) {
        self.temporal_adjacency_index = Some(index);
    }

    /// Get a reference to the temporal adjacency index if configured.
    ///
    /// Used by persistence layer to save the index to disk.
    pub fn get_temporal_adjacency_index(
        &self,
    ) -> Option<&Arc<crate::index::temporal_adjacency::TemporalAdjacencyIndex>> {
        self.temporal_adjacency_index.as_ref()
    }

    /// Get outgoing edges from a node at a specific point in time.
    ///
    /// This method uses the temporal adjacency index to efficiently find all
    /// edges that were valid at the specified time, including edges that have
    /// been deleted in current storage.
    ///
    /// # Arguments
    ///
    /// * `source` - The source node ID
    /// * `valid_time` - The valid time to query
    /// * `tx_time` - The transaction time to query
    ///
    /// # Returns
    ///
    /// A vector of edge IDs that were valid at the specified time. Returns an
    /// empty vector if no temporal adjacency index is configured.
    pub fn get_outgoing_edges_at_time(
        &self,
        source: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        if let Some(ref index) = self.temporal_adjacency_index {
            index.get_outgoing_at_time(source, valid_time, tx_time)
        } else {
            Vec::new()
        }
    }

    /// Get incoming edges to a node at a specific point in time.
    ///
    /// This method uses the temporal adjacency index to efficiently find all
    /// edges that were valid at the specified time, including edges that have
    /// been deleted in current storage.
    ///
    /// # Arguments
    ///
    /// * `target` - The target node ID
    /// * `valid_time` - The valid time to query
    /// * `tx_time` - The transaction time to query
    ///
    /// # Returns
    ///
    /// A vector of edge IDs that were valid at the specified time. Returns an
    /// empty vector if no temporal adjacency index is configured.
    pub fn get_incoming_edges_at_time(
        &self,
        target: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        if let Some(ref index) = self.temporal_adjacency_index {
            index.get_incoming_at_time(target, valid_time, tx_time)
        } else {
            Vec::new()
        }
    }

    /// Get a node version from any tier (hot or cold).
    ///
    /// This method first checks hot storage, then falls back to cold storage
    /// via the tiered storage layer (if configured).
    ///
    /// # Arguments
    ///
    /// * `version_id` - The version ID to retrieve
    ///
    /// # Returns
    ///
    /// Returns `Ok(Some(version))` if found in either tier, `Ok(None)` if not found,
    /// or an error if cold storage access fails.
    pub fn get_node_version_tiered(
        &self,
        version_id: VersionId,
    ) -> Result<Option<Arc<NodeVersion>>> {
        // Check hot storage first
        if let Some(version) = self.node_versions.get(&version_id) {
            if let Some(ref tiered) = self.tiered_storage {
                tiered.record_hot_hit();
            }
            return Ok(Some(Arc::new(version.clone())));
        }

        // Fall back to cold storage if tiered storage is configured
        if let Some(ref tiered) = self.tiered_storage {
            return tiered.get_node_version_cold(version_id);
        }

        Ok(None)
    }

    /// Get an edge version from any tier (hot or cold).
    ///
    /// This method first checks hot storage, then falls back to cold storage
    /// via the tiered storage layer (if configured).
    pub fn get_edge_version_tiered(
        &self,
        version_id: VersionId,
    ) -> Result<Option<Arc<EdgeVersion>>> {
        // Check hot storage first
        if let Some(version) = self.edge_versions.get(&version_id) {
            if let Some(ref tiered) = self.tiered_storage {
                tiered.record_hot_hit();
            }
            return Ok(Some(Arc::new(version.clone())));
        }

        // Fall back to cold storage if tiered storage is configured
        if let Some(ref tiered) = self.tiered_storage {
            return tiered.get_edge_version_cold(version_id);
        }

        Ok(None)
    }

    /// Migrate old versions from hot storage to cold storage.
    ///
    /// This method identifies versions that meet the migration policy criteria
    /// and moves them to cold storage. The migration service handles the actual
    /// transfer, and this method removes migrated versions from hot storage.
    ///
    /// # Arguments
    ///
    /// * `migration_service` - The migration service to use for transferring versions
    ///
    /// # Returns
    ///
    /// Returns the number of versions migrated, or an error if migration fails.
    pub fn migrate_to_cold(
        &mut self,
        migration_service: &super::migration::MigrationService,
    ) -> Result<usize> {
        use std::time::Instant;

        if self.tiered_storage.is_none() {
            return Ok(0);
        }

        let mut total_migrated = 0;

        // Identify node version candidates
        let node_candidates = migration_service.identify_node_candidates(
            &self.node_versions,
            &self.node_version_heads,
            &self.node_version_counts,
            Instant::now(),
        );

        // Collect versions to migrate
        let node_versions_to_migrate: Vec<NodeVersion> = node_candidates
            .iter()
            .filter_map(|c| self.node_versions.get(&c.version_id).cloned())
            .collect();

        // Migrate to cold storage
        if !node_versions_to_migrate.is_empty() {
            let migrated = migration_service.migrate_node_versions(&node_versions_to_migrate)?;
            total_migrated += migrated;

            // Remove migrated versions from hot storage
            for candidate in &node_candidates[..migrated] {
                if let Some(version) = self.node_versions.remove(&candidate.version_id) {
                    // Update version count
                    if let Some(count) = self.node_version_counts.get_mut(&version.node_id) {
                        *count = count.saturating_sub(1);
                    }
                    // Issue #212: Update cached stats counters when migrating to cold storage
                    if version.is_anchor() {
                        self.cached_node_anchor_count =
                            self.cached_node_anchor_count.saturating_sub(1);
                    } else {
                        self.cached_node_delta_count =
                            self.cached_node_delta_count.saturating_sub(1);
                    }
                }
            }
        }

        // Identify edge version candidates
        let edge_candidates = migration_service.identify_edge_candidates(
            &self.edge_versions,
            &self.edge_version_heads,
            &self.edge_version_counts,
            Instant::now(),
        );

        // Collect versions to migrate
        let edge_versions_to_migrate: Vec<EdgeVersion> = edge_candidates
            .iter()
            .filter_map(|c| self.edge_versions.get(&c.version_id).cloned())
            .collect();

        // Migrate to cold storage
        if !edge_versions_to_migrate.is_empty() {
            let migrated = migration_service.migrate_edge_versions(&edge_versions_to_migrate)?;
            total_migrated += migrated;

            // Remove migrated versions from hot storage
            for candidate in &edge_candidates[..migrated] {
                if let Some(version) = self.edge_versions.remove(&candidate.version_id)
                    && let Some(count) = self.edge_version_counts.get_mut(&version.edge_id)
                {
                    *count = count.saturating_sub(1);
                    // Issue #212: Update cached stats counters when migrating to cold storage
                    if version.is_anchor() {
                        self.cached_edge_anchor_count =
                            self.cached_edge_anchor_count.saturating_sub(1);
                    } else {
                        self.cached_edge_delta_count =
                            self.cached_edge_delta_count.saturating_sub(1);
                    }
                }
            }
        }

        Ok(total_migrated)
    }

    /// Get the total number of versions in hot storage.
    pub fn hot_version_count(&self) -> usize {
        self.node_versions.len() + self.edge_versions.len()
    }

    /// Get the estimated memory usage of hot storage in bytes.
    pub fn hot_memory_usage(&self) -> usize {
        let node_size = self.node_versions.len() * std::mem::size_of::<NodeVersion>();
        let edge_size = self.edge_versions.len() * std::mem::size_of::<EdgeVersion>();
        node_size + edge_size
    }

    /// Close the transaction time of a node version.
    ///
    /// This marks the version as no longer being the "current knowledge" after
    /// the specified timestamp. Used when a node is deleted or superseded.
    ///
    /// # Arguments
    /// * `version_id` - The version to close
    /// * `end_timestamp` - The timestamp at which this version is no longer valid
    ///
    /// # Returns
    /// `Ok(())` if successful, `Err` if version not found
    pub fn close_node_version_transaction_time(
        &mut self,
        version_id: VersionId,
        end_timestamp: Timestamp,
    ) -> Result<()> {
        let version = self
            .node_versions
            .get_mut(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        // Get the node ID before closing (needed for temporal index update)
        let node_id = version.node_id;

        // Use TemporalVersion trait method
        version.close_transaction_time(end_timestamp);

        // Update temporal index to reflect the closed interval (Issue #209)
        if let Some(ref indexes) = self.temporal_indexes {
            indexes.update_node_transaction_time_end(node_id, version_id, end_timestamp);
        }

        Ok(())
    }

    /// Close the transaction time of an edge version.
    ///
    /// This marks the version as no longer being the "current knowledge" after
    /// the specified timestamp. Used when an edge is deleted or superseded.
    pub fn close_edge_version_transaction_time(
        &mut self,
        version_id: VersionId,
        end_timestamp: Timestamp,
    ) -> Result<()> {
        let version = self
            .edge_versions
            .get_mut(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        // Get the edge ID and node IDs before closing (needed for index updates)
        let edge_id = version.edge_id;
        let source = version.source;
        let target = version.target;

        // Use TemporalVersion trait method
        version.close_transaction_time(end_timestamp);

        // Update temporal index to reflect the closed interval (Issue #209)
        if let Some(ref indexes) = self.temporal_indexes {
            indexes.update_edge_transaction_time_end(edge_id, version_id, end_timestamp);
        }

        // Update temporal adjacency index to reflect the closed transaction time
        if let Some(ref adj_index) = self.temporal_adjacency_index {
            adj_index.close_edge_transaction_time(edge_id, source, target, end_timestamp);
        }

        Ok(())
    }

    /// Find a node version valid at a specific point in time.
    ///
    /// **Performance (Issue #209)**:
    /// - **With temporal indexes**: O(log N) binary search where N = version count
    /// - **Without temporal indexes**: O(N) linear scan through version chain
    ///
    /// When temporal indexes are configured via `set_temporal_indexes()`, this
    /// method uses efficient binary search. Otherwise, it falls back to walking
    /// the version chain. For entities with 100s-1000s of versions, the temporal
    /// index provides significant performance improvements (10-100x faster).
    ///
    /// # Arguments
    /// * `node_id` - The node to query
    /// * `valid_time` - When the fact was true in reality
    /// * `transaction_time` - When the fact was recorded in the database
    ///
    /// # Returns
    /// The version ID visible at the given bi-temporal point, or None if no
    /// version exists at that time.
    pub fn find_node_version_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Option<VersionId> {
        // Fast path: Use temporal index if available (O(log n)) - Issue #209
        // The temporal indexes are now properly updated when intervals are closed
        if let Some(ref indexes) = self.temporal_indexes {
            return indexes
                .find_node_version_at_point_iter(node_id, valid_time, transaction_time)
                .find(|&version_id| {
                    // Robustness check: verify visibility against actual version data
                    self.node_versions
                        .get(&version_id)
                        .map(|v| v.temporal.is_visible_at(valid_time, transaction_time))
                        .unwrap_or(false)
                });
        }

        // Fallback: Linear scan through version chain (O(n))
        // This is only used when temporal indexes are not configured
        let mut current_id = self.node_version_heads.get(&node_id).copied()?;

        loop {
            let version = self.node_versions.get(&current_id)?;

            // Check if this version's temporal interval contains the query time
            if version.temporal.is_visible_at(valid_time, transaction_time) {
                return Some(current_id);
            }

            // Move to previous version
            current_id = version.prev_version?;
        }
    }

    /// Find an edge version valid at a specific point in time.
    ///
    /// **Performance (Issue #209)**:
    /// - **With temporal indexes**: O(log N) binary search where N = version count
    /// - **Without temporal indexes**: O(N) linear scan through version chain
    ///
    /// When temporal indexes are configured via `set_temporal_indexes()`, this
    /// method uses efficient binary search. Otherwise, it falls back to walking
    /// the version chain. For entities with 100s-1000s of versions, the temporal
    /// index provides significant performance improvements (10-100x faster).
    ///
    /// # Arguments
    /// * `edge_id` - The edge to query
    /// * `valid_time` - When the fact was true in reality
    /// * `transaction_time` - When the fact was recorded in the database
    ///
    /// # Returns
    /// The version ID visible at the given bi-temporal point, or None if no
    /// version exists at that time.
    pub fn find_edge_version_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Option<VersionId> {
        // Fast path: Use temporal index if available (O(log n)) - Issue #209
        // The temporal indexes are now properly updated when intervals are closed
        if let Some(ref indexes) = self.temporal_indexes {
            return indexes
                .find_edge_version_at_point_iter(edge_id, valid_time, transaction_time)
                .find(|&version_id| {
                    // Robustness check: verify visibility against actual version data
                    self.edge_versions
                        .get(&version_id)
                        .map(|v| v.temporal.is_visible_at(valid_time, transaction_time))
                        .unwrap_or(false)
                });
        }

        // Fallback: Linear scan through version chain (O(n))
        // This is only used when temporal indexes are not configured
        let mut current_id = self.edge_version_heads.get(&edge_id).copied()?;

        loop {
            let version = self.edge_versions.get(&current_id)?;

            if version.temporal.is_visible_at(valid_time, transaction_time) {
                return Some(current_id);
            }

            current_id = version.prev_version?;
        }
    }

    /// Get a node as it existed at a specific point in bi-temporal space.
    ///
    /// Uses the temporal index for O(log n) candidate lookup, then verifies
    /// visibility (handles closed intervals from deletions).
    pub fn get_node_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_node_at_time").entered();

        let version_id = self
            .find_node_version_at_time(node_id, valid_time, transaction_time)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        // Note: find_node_version_at_time already checked visibility
        let version = self
            .get_node_version(version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        let properties = self.reconstruct_node_properties(version_id)?;

        Ok(Node::new(
            version.node_id,
            version.label,
            properties,
            version.id,
        ))
    }

    /// Get an edge as it existed at a specific point in bi-temporal space.
    ///
    /// Uses the temporal index for O(log n) candidate lookup, then verifies
    /// visibility (handles closed intervals from deletions).
    pub fn get_edge_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_edge_at_time").entered();

        let version_id = self
            .find_edge_version_at_time(edge_id, valid_time, transaction_time)
            .ok_or(StorageError::EdgeNotFound(edge_id))?;

        // Note: find_edge_version_at_time already checked visibility
        let version = self
            .get_edge_version(version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        let properties = self.reconstruct_edge_properties(version_id)?;

        Ok(Edge::new(
            version.edge_id,
            version.label,
            version.source,
            version.target,
            properties,
            version.id,
        ))
    }

    /// Get multiple nodes as they existed at a specific point in bi-temporal space.
    ///
    /// This retrieves nodes in batch to minimize overhead.
    /// If a node is not found or not visible at the time, the Option will be None.
    pub fn get_nodes_at_time(
        &self,
        node_ids: &[NodeId],
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Vec<(NodeId, Option<Node>)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_nodes_at_time").entered();

        let mut results = Vec::with_capacity(node_ids.len());

        for &node_id in node_ids {
            let node = if let Some(version_id) =
                self.find_node_version_at_time(node_id, valid_time, transaction_time)
            {
                // We found a visible version. Reconstruct it.
                match self.reconstruct_node_properties(version_id) {
                    Ok(properties) => {
                        let version = self
                            .node_versions
                            .get(&version_id)
                            .ok_or(StorageError::VersionNotFound(version_id))?;
                        Some(Node::new(
                            version.node_id,
                            version.label,
                            properties,
                            version.id,
                        ))
                    }
                    Err(_e) => {
                        #[cfg(feature = "observability")]
                        tracing::error!(
                            version_id = %version_id,
                            node_id = %node_id,
                            error = %_e,
                            "Property reconstruction failed in batch query"
                        );
                        None
                    }
                }
            } else {
                None
            };
            results.push((node_id, node));
        }

        Ok(results)
    }

    /// Get multiple edges as they existed at a specific point in bi-temporal space.
    ///
    /// This retrieves edges in batch to minimize overhead.
    /// If an edge is not found or not visible at the time, the Option will be None.
    pub fn get_edges_at_time(
        &self,
        edge_ids: &[EdgeId],
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Vec<(EdgeId, Option<Edge>)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_edges_at_time").entered();

        let mut results = Vec::with_capacity(edge_ids.len());

        for &edge_id in edge_ids {
            let edge = if let Some(version_id) =
                self.find_edge_version_at_time(edge_id, valid_time, transaction_time)
            {
                // We found a visible version. Reconstruct it.
                match self.reconstruct_edge_properties(version_id) {
                    Ok(properties) => {
                        let version = self
                            .edge_versions
                            .get(&version_id)
                            .ok_or(StorageError::VersionNotFound(version_id))?;
                        Some(Edge::new(
                            version.edge_id,
                            version.label,
                            version.source,
                            version.target,
                            properties,
                            version.id,
                        ))
                    }
                    Err(_e) => {
                        #[cfg(feature = "observability")]
                        tracing::error!(
                            version_id = %version_id,
                            edge_id = %edge_id,
                            error = %_e,
                            "Property reconstruction failed in batch query"
                        );
                        None
                    }
                }
            } else {
                None
            };
            results.push((edge_id, edge));
        }

        Ok(results)
    }

    /// Get the complete version history of a node.
    ///
    /// Returns all versions in chronological order (oldest first).
    pub fn get_node_history(&self, node_id: NodeId) -> Result<EntityHistory> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_node_history").entered();

        // Get the current version ID
        let current_version_id = self
            .get_current_node_version(node_id)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        // Traverse the version chain backwards to get all versions in order
        let mut version_ids = Vec::new();
        let mut current_id = Some(current_version_id);

        while let Some(vid) = current_id {
            version_ids.push(vid);
            current_id = self.get_node_version(vid).and_then(|v| v.prev_version);
        }

        // Reverse to get oldest-first order
        version_ids.reverse();

        // Build VersionInfo for each version
        let mut versions = Vec::with_capacity(version_ids.len());
        for (version_number, version_id) in version_ids.iter().enumerate() {
            if let Some(version) = self.get_node_version(*version_id) {
                let properties = self.reconstruct_node_properties(*version_id)?;

                versions.push(VersionInfo {
                    version_number: (version_number + 1) as u64, // 1-indexed
                    version_id: *version_id,
                    temporal: version.temporal,
                    properties,
                    label: GLOBAL_INTERNER
                        .resolve(version.label)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| version.label.to_string()),
                });
            }
        }

        Ok(EntityHistory { versions })
    }

    /// Get a node at a specific logical version number.
    ///
    /// Version numbers are 1-indexed (1 = first version, 2 = second version, etc.).
    pub fn get_node_at_version(&self, node_id: NodeId, version_number: u64) -> Result<Node> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_node_at_version").entered();

        // Get the current version ID
        let current_version_id = self
            .get_current_node_version(node_id)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        // Traverse the version chain backwards to collect all versions
        let mut version_ids = Vec::new();
        let mut current_id = Some(current_version_id);

        while let Some(vid) = current_id {
            version_ids.push(vid);
            current_id = self.get_node_version(vid).and_then(|v| v.prev_version);
        }

        // Reverse to get oldest-first order
        version_ids.reverse();

        // Convert 1-indexed version number to 0-indexed array index
        let index = version_number
            .checked_sub(1)
            .ok_or(StorageError::NodeNotFound(node_id))? as usize;

        // Get the version ID at that index
        let version_id = version_ids
            .get(index)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        // Reconstruct the node from that version
        let version = self
            .get_node_version(*version_id)
            .ok_or(StorageError::VersionNotFound(*version_id))?;

        let properties = self.reconstruct_node_properties(*version_id)?;

        Ok(Node::new(
            version.node_id,
            version.label,
            properties,
            version.id,
        ))
    }

    /// Compute the difference between two versions of a node.
    ///
    /// Shows which properties were added, removed, or modified.
    pub fn diff_node_versions(
        &self,
        node_id: NodeId,
        from_version: VersionId,
        to_version: VersionId,
    ) -> Result<VersionDiff> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("diff_node_versions").entered();

        // Validate that both versions belong to the requested node
        let from_ver = self
            .get_node_version(from_version)
            .ok_or(StorageError::VersionNotFound(from_version))?;
        let to_ver = self
            .get_node_version(to_version)
            .ok_or(StorageError::VersionNotFound(to_version))?;

        if from_ver.node_id != node_id {
            return Err(StorageError::InconsistentState {
                reason: format!(
                    "Version {} belongs to node {}, not node {}",
                    from_version, from_ver.node_id, node_id
                ),
            }
            .into());
        }
        if to_ver.node_id != node_id {
            return Err(StorageError::InconsistentState {
                reason: format!(
                    "Version {} belongs to node {}, not node {}",
                    to_version, to_ver.node_id, node_id
                ),
            }
            .into());
        }

        // Reconstruct both versions
        let from_props = self.reconstruct_node_properties(from_version)?;
        let to_props = self.reconstruct_node_properties(to_version)?;

        // Compute diff
        Ok(VersionDiff::compute(
            &from_props,
            &to_props,
            from_version,
            to_version,
        ))
    }

    /// Get the complete version history of an edge.
    ///
    /// Returns all versions in chronological order (oldest first).
    pub fn get_edge_history(&self, edge_id: EdgeId) -> Result<EntityHistory> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_edge_history").entered();

        // Get the current version ID
        let current_version_id = self
            .get_current_edge_version(edge_id)
            .ok_or(StorageError::EdgeNotFound(edge_id))?;

        // Traverse the version chain backwards to get all versions
        let mut version_ids = Vec::new();
        let mut current_id = Some(current_version_id);

        while let Some(vid) = current_id {
            version_ids.push(vid);
            current_id = self.get_edge_version(vid).and_then(|v| v.prev_version);
        }

        // Reverse to get oldest-first order
        version_ids.reverse();

        // Build VersionInfo for each version
        let mut versions = Vec::with_capacity(version_ids.len());
        for (version_number, version_id) in version_ids.iter().enumerate() {
            if let Some(version) = self.get_edge_version(*version_id) {
                let properties = self.reconstruct_edge_properties(*version_id)?;

                versions.push(VersionInfo {
                    version_number: (version_number + 1) as u64, // 1-indexed
                    version_id: *version_id,
                    temporal: version.temporal,
                    properties,
                    label: GLOBAL_INTERNER
                        .resolve(version.label)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| version.label.to_string()),
                });
            }
        }

        Ok(EntityHistory { versions })
    }

    /// Compute the difference between two versions of an edge.
    ///
    /// Shows which properties were added, removed, or modified.
    pub fn diff_edge_versions(
        &self,
        edge_id: EdgeId,
        from_version: VersionId,
        to_version: VersionId,
    ) -> Result<VersionDiff> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("diff_edge_versions").entered();

        // Validate that both versions belong to the requested edge
        let from_ver = self
            .get_edge_version(from_version)
            .ok_or(StorageError::VersionNotFound(from_version))?;
        let to_ver = self
            .get_edge_version(to_version)
            .ok_or(StorageError::VersionNotFound(to_version))?;

        if from_ver.edge_id != edge_id {
            return Err(StorageError::InconsistentState {
                reason: format!(
                    "Version {} belongs to edge {}, not edge {}",
                    from_version, from_ver.edge_id, edge_id
                ),
            }
            .into());
        }
        if to_ver.edge_id != edge_id {
            return Err(StorageError::InconsistentState {
                reason: format!(
                    "Version {} belongs to edge {}, not edge {}",
                    to_version, to_ver.edge_id, edge_id
                ),
            }
            .into());
        }

        // Reconstruct both versions
        let from_props = self.reconstruct_edge_properties(from_version)?;
        let to_props = self.reconstruct_edge_properties(to_version)?;

        // Compute diff
        Ok(VersionDiff::compute(
            &from_props,
            &to_props,
            from_version,
            to_version,
        ))
    }

    /// Count versions since the last anchor using a generic version lookup function.
    ///
    /// This is a generic helper that works for both nodes and edges, reducing code duplication.
    /// The `get_version` closure provides type-specific version lookup.
    ///
    /// # Note (Issue #208)
    /// This method is no longer used in production code (replaced by cached counters for O(1) performance),
    /// but is retained for testing purposes to verify cache correctness.
    #[cfg(test)]
    fn count_versions_since_anchor<'a, V: EntityVersion + 'a>(
        &'a self,
        version_id: VersionId,
        get_version: impl Fn(VersionId) -> Option<&'a V>,
    ) -> usize {
        let mut count = 0;
        let mut current_id = version_id;

        loop {
            if let Some(version) = get_version(current_id) {
                if version.is_anchor() {
                    return count;
                }
                count += 1;

                if let Some(prev_id) = version.prev_version() {
                    current_id = prev_id;
                } else {
                    return count;
                }
            } else {
                return count;
            }
        }
    }

    /// Count how many versions exist since the last anchor (for a node).
    ///
    /// Note: The closure overhead is negligible and typically optimized away by LLVM.
    /// If profiling shows this is a hotspot, consider monomorphizing.
    ///
    /// # Note (Issue #208)
    /// This method is no longer used in production code, retained for testing only.
    #[cfg(test)]
    fn count_versions_since_anchor_node(&self, version_id: VersionId) -> usize {
        self.count_versions_since_anchor(version_id, |vid| self.node_versions.get(&vid))
    }

    /// Count how many versions exist since the last anchor (for an edge).
    ///
    /// Note: The closure overhead is negligible and typically optimized away by LLVM.
    /// If profiling shows this is a hotspot, consider monomorphizing.
    ///
    /// # Note (Issue #208)
    /// This method is no longer used in production code, retained for testing only.
    #[cfg(test)]
    #[allow(dead_code)]
    fn count_versions_since_anchor_edge(&self, version_id: VersionId) -> usize {
        self.count_versions_since_anchor(version_id, |vid| self.edge_versions.get(&vid))
    }

    /// Handle pre-anchor hook invocation with proper logging.
    ///
    /// This helper method encapsulates the common pattern of calling pre-anchor hooks
    /// and handling their results (success with snapshot ID, success without snapshot,
    /// or graceful degradation on failure).
    fn handle_pre_anchor_hook(
        context: AnchorHookContext<'_>,
        version_data: &mut VersionData,
        hook: &Option<PreAnchorHook>,
    ) {
        if let Some(hook_fn) = hook {
            match hook_fn(
                context.entity_type,
                context.entity_id,
                context.timestamp,
                context.properties,
            ) {
                Ok(Some(snapshot_id)) => {
                    version_data.set_vector_snapshot_id(snapshot_id);
                    #[cfg(feature = "observability")]
                    tracing::debug!(
                        "Pre-anchor hook returned snapshot ID {} for {} {}",
                        snapshot_id,
                        context.entity_type,
                        context.entity_id
                    );
                }
                Ok(None) => {
                    #[cfg(feature = "observability")]
                    tracing::debug!(
                        "Pre-anchor hook returned None for {} {} (no snapshot needed)",
                        context.entity_type,
                        context.entity_id
                    );
                }
                Err(_e) => {
                    #[cfg(feature = "observability")]
                    tracing::warn!(
                        "Pre-anchor hook failed for {} {} at timestamp {}: {} (anchor will still be created)",
                        context.entity_type,
                        context.entity_id,
                        context.timestamp,
                        _e
                    );
                }
            }
        }
    }

    /// Close the temporal intervals of a previous version when a new version is created.
    ///
    /// This helper handles the common logic of linking versions together and closing
    /// the temporal intervals of the previous version at the new version's start time.
    fn close_previous_version_intervals<V: EntityVersion>(
        prev_version: &mut V,
        new_version_id: VersionId,
        new_temporal: &BiTemporalInterval,
    ) {
        prev_version.set_next_version(Some(new_version_id));

        // Work on a local copy, apply modifications, then write back
        let mut prev_temporal = *prev_version.temporal();

        if prev_temporal.is_currently_valid()
            && new_temporal.valid_time().start() > prev_temporal.valid_time().start()
        {
            prev_temporal = prev_temporal.close_valid_time(new_temporal.valid_time().start());
        }

        if prev_temporal.is_currently_recorded()
            && new_temporal.transaction_time().start() > prev_temporal.transaction_time().start()
        {
            prev_temporal =
                prev_temporal.close_transaction_time(new_temporal.transaction_time().start());
        }

        *prev_version.temporal_mut() = prev_temporal;
    }

    /// Extract version metadata and data for copy-out reconstruction (nodes).
    ///
    /// This method copies the necessary version chain data while holding the lock,
    /// allowing reconstruction to proceed outside the lock.
    ///
    /// Returns (version metadata, label, data) that can be used for lock-free reconstruction.
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
    /// - Primary cache hits (fast path)
    /// - Anchor cache hits (fallback path)
    /// - Full reconstructions (slow path)
    ///
    /// This provides actionable insights for cache tuning:
    /// - High anchor_cache_hits → increase primary cache size
    /// - High full_reconstructions → increase overall cache capacity
    ///
    /// # Example
    /// ```no_run
    /// # use gallifreydb::storage::historical::HistoricalStorage;
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
    ///
    /// # Example
    /// ```no_run
    /// # use gallifreydb::storage::historical::HistoricalStorage;
    /// let storage = HistoricalStorage::new();
    /// // ... perform some operations ...
    /// if let Some(hit_rate) = storage.cache_hit_rate() {
    ///     println!("Cache hit rate: {:.2}%", hit_rate * 100.0);
    /// }
    /// ```
    pub fn cache_hit_rate(&self) -> Option<f64> {
        self.cache_metrics().hit_rate()
    }

    /// Check if the cache should be resized based on hit rate (Improvement #3).
    ///
    /// Returns true if the cache hit rate is below the threshold (default 80%)
    /// and there have been enough operations to make a meaningful assessment
    /// (at least 100 operations).
    ///
    /// # Arguments
    /// * `threshold` - Minimum acceptable hit rate (0.0 to 1.0). Defaults to 0.8 (80%).
    /// * `min_operations` - Minimum number of cache operations before assessment. Defaults to 100.
    ///
    /// # Returns
    /// * `Some(current_hit_rate)` - If resizing is recommended, returns current hit rate
    /// * `None` - If cache performance is acceptable or insufficient data
    ///
    /// # Example
    /// ```no_run
    /// # use gallifreydb::storage::historical::HistoricalStorage;
    /// let storage = HistoricalStorage::new();
    /// // ... perform some operations ...
    ///
    /// if let Some(hit_rate) = storage.should_resize_cache(0.8, 100) {
    ///     println!("Cache hit rate ({:.2}%) is low, consider doubling cache size", hit_rate * 100.0);
    ///     // Create new storage with larger cache:
    ///     // let new_storage = HistoricalStorage::with_config_retention_and_cache_size(
    ///     //     config, retention, current_size * 2
    ///     // );
    /// }
    /// ```
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

    /// Get an iterator over all node versions (test-only helper).
    ///
    /// This method provides access to the node versions for integration test
    /// verification purposes. It is public to allow access from integration tests
    /// but is hidden from documentation and marked with `__test_` prefix to
    /// discourage production use.
    ///
    /// **Warning**: This method exposes internal implementation details and
    /// should only be used in tests.
    #[doc(hidden)]
    pub fn __test_get_node_versions_iterator(&self) -> impl Iterator<Item = &NodeVersion> {
        self.node_versions.values()
    }

    /// Get all node versions for persistence.
    ///
    /// This is a crate-internal method used by the index persistence layer.
    pub(crate) fn get_node_versions(&self) -> &HashMap<VersionId, NodeVersion> {
        &self.node_versions
    }

    /// Get all edge versions for persistence.
    ///
    /// This is a crate-internal method used by the index persistence layer.
    pub(crate) fn get_edge_versions(&self) -> &HashMap<VersionId, EdgeVersion> {
        &self.edge_versions
    }

    /// Reserve capacity for batch restoration from persistence.
    ///
    /// Pre-allocating capacity improves restoration performance by reducing
    /// reallocations during bulk insertion. Call this before restoring
    /// persisted versions.
    ///
    /// # Arguments
    ///
    /// * `node_versions` - Expected number of node versions to restore
    /// * `edge_versions` - Expected number of edge versions to restore
    pub(crate) fn reserve_restoration_capacity(
        &mut self,
        node_versions: usize,
        edge_versions: usize,
    ) {
        self.node_versions.reserve(node_versions);
        self.edge_versions.reserve(edge_versions);
        // Conservatively estimate unique entities as half of versions
        // (typical case: each entity has ~2 versions on average)
        self.node_version_heads.reserve(node_versions / 2);
        self.edge_version_heads.reserve(edge_versions / 2);
        self.node_version_counts.reserve(node_versions / 2);
        self.edge_version_counts.reserve(edge_versions / 2);
    }

    /// Insert a restored node version directly into storage.
    ///
    /// This is used during index loading to restore persisted versions.
    /// Unlike normal version insertion, this bypasses transaction processing
    /// since the data comes from a trusted source (our own persistence layer).
    ///
    /// # Errors
    ///
    /// Returns an error if the version ID or node ID is invalid.
    pub(crate) fn insert_restored_node_version(&mut self, version: NodeVersion) -> Result<()> {
        let version_id = version.id;
        let node_id = version.node_id;
        let is_anchor = version.is_anchor();

        // Store the version
        self.node_versions.insert(version_id, version);

        // Update version head
        self.node_version_heads.insert(node_id, version_id);

        // Update version count
        *self.node_version_counts.entry(node_id).or_insert(0) += 1;

        // Issue #212: Update cached stats counters during persistence restore
        if is_anchor {
            self.cached_node_anchor_count += 1;
        } else {
            self.cached_node_delta_count += 1;
        }

        Ok(())
    }

    /// Insert a restored edge version directly into storage.
    ///
    /// This is used during index loading to restore persisted versions.
    /// Unlike normal version insertion, this bypasses transaction processing
    /// since the data comes from a trusted source (our own persistence layer).
    ///
    /// # Errors
    ///
    /// Returns an error if the version ID or edge ID is invalid.
    pub(crate) fn insert_restored_edge_version(&mut self, version: EdgeVersion) -> Result<()> {
        let version_id = version.id;
        let edge_id = version.edge_id;
        let is_anchor = version.is_anchor();

        // Store the version
        self.edge_versions.insert(version_id, version);

        // Update version head
        self.edge_version_heads.insert(edge_id, version_id);

        // Update version count
        *self.edge_version_counts.entry(edge_id).or_insert(0) += 1;

        // Issue #212: Update cached stats counters during persistence restore
        if is_anchor {
            self.cached_edge_anchor_count += 1;
        } else {
            self.cached_edge_delta_count += 1;
        }

        Ok(())
    }

    /// Rebuild version chains after restoration from persistence.
    ///
    /// This method reconstructs the `prev_version` and `next_version` links for all
    /// versions, and ensures version heads point to the correct (latest tx_time) version.
    /// Must be called after all versions have been inserted via `insert_restored_node_version`
    /// and `insert_restored_edge_version`.
    ///
    /// # Version Chain Semantics
    ///
    /// - Versions are ordered by transaction time (tx_time start)
    /// - `prev_version` points to the temporally previous version (earlier tx_time)
    /// - `next_version` points to the temporally next version (later tx_time)
    /// - Version heads point to the version with the latest tx_time
    pub(crate) fn rebuild_version_chains(&mut self) {
        // === Rebuild node version chains ===

        // Group versions by node ID
        let mut node_versions_by_id: HashMap<NodeId, Vec<VersionId>> = HashMap::new();
        for (vid, version) in &self.node_versions {
            node_versions_by_id
                .entry(version.node_id)
                .or_default()
                .push(*vid);
        }

        // For each node, sort versions by tx_time and link them
        for (node_id, mut version_ids) in node_versions_by_id {
            // Sort by transaction time start (ascending order)
            // Phase 2: Use TIMESTAMP_MAX instead of i64::MAX
            use crate::core::temporal::TIMESTAMP_MAX;
            version_ids.sort_by_key(|vid| {
                self.node_versions
                    .get(vid)
                    .map(|v| v.temporal.transaction_time().start())
                    .unwrap_or(TIMESTAMP_MAX)
            });

            // Link prev/next
            for i in 0..version_ids.len() {
                let vid = version_ids[i];

                // Link to previous version (earlier in time)
                let prev = if i > 0 {
                    Some(version_ids[i - 1])
                } else {
                    None
                };

                // Link to next version (later in time)
                let next = if i < version_ids.len() - 1 {
                    Some(version_ids[i + 1])
                } else {
                    None
                };

                if let Some(version) = self.node_versions.get_mut(&vid) {
                    version.prev_version = prev;
                    version.next_version = next;
                }
            }

            // Set head to the latest version (last in sorted order)
            if let Some(&latest_vid) = version_ids.last() {
                self.node_version_heads.insert(node_id, latest_vid);

                // Issue #208: Rebuild counter cache for anchor interval checks
                // Count versions since last anchor by walking backwards from head
                let mut count = 0;
                let mut current_id = latest_vid;

                while let Some(version) = self.node_versions.get(&current_id) {
                    if version.is_anchor() {
                        // Found anchor, counter is 0
                        break;
                    }
                    // Delta version, increment counter and continue
                    count += 1;
                    if let Some(prev_id) = version.prev_version {
                        current_id = prev_id;
                    } else {
                        // No more versions (shouldn't happen, first is always anchor)
                        break;
                    }
                }

                self.node_versions_since_anchor.insert(node_id, count);
            }
        }

        // === Rebuild edge version chains ===

        // Group versions by edge ID
        let mut edge_versions_by_id: HashMap<EdgeId, Vec<VersionId>> = HashMap::new();
        for (vid, version) in &self.edge_versions {
            edge_versions_by_id
                .entry(version.edge_id)
                .or_default()
                .push(*vid);
        }

        // For each edge, sort versions by tx_time and link them
        for (edge_id, mut version_ids) in edge_versions_by_id {
            // Sort by transaction time start (ascending order)
            // Phase 2: Use TIMESTAMP_MAX (already imported above)
            version_ids.sort_by_key(|vid| {
                self.edge_versions
                    .get(vid)
                    .map(|v| v.temporal.transaction_time().start())
                    .unwrap_or(TIMESTAMP_MAX)
            });

            // Link prev/next
            for i in 0..version_ids.len() {
                let vid = version_ids[i];

                // Link to previous version (earlier in time)
                let prev = if i > 0 {
                    Some(version_ids[i - 1])
                } else {
                    None
                };

                // Link to next version (later in time)
                let next = if i < version_ids.len() - 1 {
                    Some(version_ids[i + 1])
                } else {
                    None
                };

                if let Some(version) = self.edge_versions.get_mut(&vid) {
                    version.prev_version = prev;
                    version.next_version = next;
                }
            }

            // Set head to the latest version (last in sorted order)
            if let Some(&latest_vid) = version_ids.last() {
                self.edge_version_heads.insert(edge_id, latest_vid);

                // Issue #208: Rebuild counter cache for anchor interval checks
                // Count versions since last anchor by walking backwards from head
                let mut count = 0;
                let mut current_id = latest_vid;

                while let Some(version) = self.edge_versions.get(&current_id) {
                    if version.is_anchor() {
                        // Found anchor, counter is 0
                        break;
                    }
                    // Delta version, increment counter and continue
                    count += 1;
                    if let Some(prev_id) = version.prev_version {
                        current_id = prev_id;
                    } else {
                        // No more versions (shouldn't happen, first is always anchor)
                        break;
                    }
                }

                self.edge_versions_since_anchor.insert(edge_id, count);
            }
        }
    }

    /// Create an MVCC snapshot of historical storage at the specified LSN.
    ///
    /// This provides snapshot isolation for checkpoint operations, capturing
    /// all node and edge versions at a consistent point in time.
    ///
    /// # Snapshot Isolation
    ///
    /// The snapshot captures Arc references to all versions. Concurrent
    /// modifications after snapshot creation do NOT affect the snapshot's
    /// iteration.
    ///
    /// # Memory Overhead
    ///
    /// - Iterates once over version HashMaps to collect Arc references
    /// - Memory: ~8 bytes per version (just Arc pointers)
    /// - For 10M versions: ~80MB overhead
    ///
    /// # Arguments
    ///
    /// * `lsn` - LSN at which snapshot is taken (for tracking)
    ///
    /// # Returns
    ///
    /// A snapshot that provides isolated iteration over versions.
    pub fn create_snapshot(
        &self,
        lsn: crate::storage::wal::LSN,
    ) -> crate::storage::snapshot::HistoricalStorageSnapshot {
        use crate::storage::snapshot::HistoricalStorageSnapshot;
        use std::sync::Arc;

        // Collect Arc references to all node versions
        let node_versions: Vec<Arc<NodeVersion>> = self
            .node_versions
            .values()
            .map(|version| Arc::new(version.clone()))
            .collect();

        // Collect Arc references to all edge versions
        let edge_versions: Vec<Arc<EdgeVersion>> = self
            .edge_versions
            .values()
            .map(|version| Arc::new(version.clone()))
            .collect();

        HistoricalStorageSnapshot::new(lsn, node_versions, edge_versions)
    }

    /// **Test-only helper**: Remove a node version from hot storage.
    ///
    /// This is used in tests to simulate version migration to cold storage.
    /// In production, versions are migrated by the `MigrationService` which
    /// atomically moves versions from hot to cold storage.
    ///
    /// # Safety
    /// This method directly modifies internal state and should only be used
    /// in tests. It does not update caches or notify observers.
    #[doc(hidden)]
    pub fn __test_remove_node_version(&mut self, version_id: VersionId) {
        self.node_versions.remove(&version_id);
    }

    /// **Test-only helper**: Clear the property reconstruction cache.
    ///
    /// This is used in tests to force actual property reconstruction instead
    /// of returning cached values. This is essential for testing that reconstruction
    /// works correctly when versions are in cold storage.
    ///
    /// # Safety
    /// This method clears caches and should only be used in tests where you
    /// want to verify reconstruction behavior without cache interference.
    #[doc(hidden)]
    pub fn __test_clear_property_cache(&self) {
        self.node_property_cache.clear();
        self.node_anchor_cache.clear();
    }
}

impl Default for HistoricalStorage {
    fn default() -> Self {
        Self::new()
    }
}

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
mod tests;
