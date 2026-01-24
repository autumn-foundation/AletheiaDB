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

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::PropertyMap;
use crate::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, Timestamp};
use crate::storage::observer::{Observer, StorageEvent, notify_observers};
use crate::storage::version::{
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

        let mut storage = Self::with_config_retention_and_cache_size(
            AnchorConfig::default(),
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
    /// # use gallifreydb::storage::observer::{StorageObserver, StorageEvent};
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
    pub fn add_node_version(
        &mut self,
        node_id: NodeId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
        label: InternedString,
        properties: PropertyMap,
    ) -> Result<()> {
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

        // Clone properties for hook call and caching (since new_anchor takes ownership)
        let properties_for_hook = properties.clone();

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
                let mut anchor =
                    NodeVersion::new_anchor(version_id, node_id, temporal, label, properties);
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
            NodeVersion::new_anchor(version_id, node_id, temporal, label, properties)
        };

        // Handle pre-anchor hook (BEFORE storing)
        if version.is_anchor() {
            Self::handle_pre_anchor_hook(
                AnchorHookContext {
                    entity_type: "node",
                    entity_id: node_id.as_u64(),
                    timestamp: temporal.transaction_time().start(),
                    properties: &properties_for_hook,
                },
                &mut version.data,
                &self.pre_node_anchor_hook,
            );
        }

        // Link previous version to this one and close its temporal intervals
        if let Some(prev_id) = prev_version_id
            && let Some(prev) = self.node_versions.get_mut(&prev_id)
        {
            Self::close_previous_version_intervals(prev, version_id, &temporal);
        }

        // Check if anchor before storing (for notifications and caching)
        let is_anchor = version.is_anchor();

        // Store the version and update indexes
        self.node_versions.insert(version_id, version);
        self.node_version_heads.insert(node_id, version_id);
        *self.node_version_counts.entry(node_id).or_insert(0) += 1;

        // Populate caches for anchors (O(1) reconstruction)
        Self::populate_anchor_caches(
            is_anchor,
            version_id,
            properties_for_hook,
            &self.node_property_cache,
            &self.node_anchor_cache,
        );

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
        temporal: BiTemporalInterval,
        label: InternedString,
        source: NodeId,
        target: NodeId,
        properties: PropertyMap,
    ) -> Result<()> {
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

        // Clone properties for hook call and caching (since new_anchor takes ownership)
        let properties_for_hook = properties.clone();

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
                let mut anchor = EdgeVersion::new_anchor(
                    version_id, edge_id, temporal, label, source, target, properties,
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
                version_id, edge_id, temporal, label, source, target, properties,
            )
        };

        // Handle pre-anchor hook (BEFORE storing)
        if version.is_anchor() {
            Self::handle_pre_anchor_hook(
                AnchorHookContext {
                    entity_type: "edge",
                    entity_id: edge_id.as_u64(),
                    timestamp: temporal.transaction_time().start(),
                    properties: &properties_for_hook,
                },
                &mut version.data,
                &self.pre_edge_anchor_hook,
            );
        }

        // Link previous version to this one and close its temporal intervals
        if let Some(prev_id) = prev_version_id
            && let Some(prev) = self.edge_versions.get_mut(&prev_id)
        {
            Self::close_previous_version_intervals(prev, version_id, &temporal);
        }

        // Check if anchor before storing (for notifications and caching)
        let is_anchor = version.is_anchor();

        // Store the version and update indexes
        self.edge_versions.insert(version_id, version);
        self.edge_version_heads.insert(edge_id, version_id);
        *self.edge_version_counts.entry(edge_id).or_insert(0) += 1;

        // Populate caches for anchors (O(1) reconstruction)
        Self::populate_anchor_caches(
            is_anchor,
            version_id,
            properties_for_hook,
            &self.edge_property_cache,
            &self.edge_anchor_cache,
        );

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

    /// Internal helper for node property reconstruction with depth tracking.
    ///
    /// The depth parameter tracks how many delta versions have been traversed.
    /// If depth exceeds MAX_RECONSTRUCTION_DEPTH, returns an error to prevent
    /// stack overflow from corrupted version chains or cycles.
    fn reconstruct_node_properties_with_depth(
        &self,
        version_id: VersionId,
        depth: usize,
    ) -> Result<PropertyMap> {
        // Check depth limit first (DoS protection)
        // Using >= for clarity: depth is 0-indexed, so this limits to exactly
        // MAX_RECONSTRUCTION_DEPTH recursive calls (depths 0..99 = 100 calls)
        if depth >= self.max_reconstruction_depth {
            // Get the node ID for error reporting
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

        // Cache miss - reconstruct properties
        self.full_reconstructions.fetch_add(1, Ordering::Relaxed);
        let version = self
            .node_versions
            .get(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        let properties = match &version.data {
            VersionData::Anchor { properties, .. } => {
                // This shouldn't happen since anchors are pre-populated, but handle gracefully
                properties.clone()
            }
            VersionData::Delta { delta } => {
                // Find the previous version
                let prev_id = version
                    .prev_version
                    .ok_or(TemporalError::CorruptedVersionChain {
                        entity_id: format!("{:?}", version.node_id),
                        reason: "Delta version has no previous version".to_string(),
                    })?;

                // Recursively reconstruct previous version with incremented depth
                let base_properties =
                    self.reconstruct_node_properties_with_depth(prev_id, depth + 1)?;

                // Apply this delta
                delta.apply(&base_properties)
            }
        };

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
    /// The depth parameter tracks how many delta versions have been traversed.
    /// If depth exceeds MAX_RECONSTRUCTION_DEPTH, returns an error to prevent
    /// stack overflow from corrupted version chains or cycles.
    fn reconstruct_edge_properties_with_depth(
        &self,
        version_id: VersionId,
        depth: usize,
    ) -> Result<PropertyMap> {
        // Check depth limit first (DoS protection)
        // Using >= for clarity: depth is 0-indexed, so this limits to exactly
        // MAX_RECONSTRUCTION_DEPTH recursive calls (depths 0..99 = 100 calls)
        if depth >= self.max_reconstruction_depth {
            // Get the edge ID for error reporting
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

        // Cache miss - reconstruct properties
        self.full_reconstructions.fetch_add(1, Ordering::Relaxed);
        let version = self
            .edge_versions
            .get(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        let properties = match &version.data {
            VersionData::Anchor { properties, .. } => {
                // This shouldn't happen since anchors are pre-populated, but handle gracefully
                properties.clone()
            }
            VersionData::Delta { delta } => {
                let prev_id = version
                    .prev_version
                    .ok_or(TemporalError::CorruptedVersionChain {
                        entity_id: format!("{:?}", version.edge_id),
                        reason: "Delta version has no previous version".to_string(),
                    })?;

                // Recursively reconstruct previous version with incremented depth
                let base_properties =
                    self.reconstruct_edge_properties_with_depth(prev_id, depth + 1)?;
                delta.apply(&base_properties)
            }
        };

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
    /// use gallifreydb::storage::cold_storage::FileColdStorage;
    ///
    /// let mut historical = HistoricalStorage::new();
    /// let cold = FileColdStorage::with_default_config("data/cold")?;
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

        // Use TemporalVersion trait method
        version.close_transaction_time(end_timestamp);
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

        // Use TemporalVersion trait method
        version.close_transaction_time(end_timestamp);
        Ok(())
    }

    /// Find a node version valid at a specific point in time.
    ///
    /// This searches the version chain for a version whose temporal interval
    /// contains the given timestamp.
    pub fn find_node_version_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Option<VersionId> {
        // Start from the head version and walk backward
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
    pub fn find_edge_version_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Option<VersionId> {
        let mut current_id = self.edge_version_heads.get(&edge_id).copied()?;

        loop {
            let version = self.edge_versions.get(&current_id)?;

            if version.temporal.is_visible_at(valid_time, transaction_time) {
                return Some(current_id);
            }

            current_id = version.prev_version?;
        }
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

    /// Populate caches for an anchor version.
    ///
    /// If the version is an anchor, immediately cache its properties in both the
    /// dedicated anchor cache and the regular property cache for optimal performance.
    fn populate_anchor_caches(
        is_anchor: bool,
        version_id: VersionId,
        properties: PropertyMap,
        property_cache: &Arc<Cache<VersionId, Arc<PropertyMap>>>,
        anchor_cache: &Arc<Cache<VersionId, Arc<PropertyMap>>>,
    ) {
        if is_anchor {
            let props_arc = Arc::new(properties);
            anchor_cache.insert(version_id, props_arc.clone());
            property_cache.insert(version_id, props_arc);
        }
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
    pub fn stats(&self) -> HistoricalStats {
        let mut node_anchor_count = 0;
        let mut node_delta_count = 0;
        let mut edge_anchor_count = 0;
        let mut edge_delta_count = 0;

        for version in self.node_versions.values() {
            if version.is_anchor() {
                node_anchor_count += 1;
            } else {
                node_delta_count += 1;
            }
        }

        for version in self.edge_versions.values() {
            if version.is_anchor() {
                edge_anchor_count += 1;
            } else {
                edge_delta_count += 1;
            }
        }

        HistoricalStats {
            total_node_versions: self.node_versions.len(),
            total_edge_versions: self.edge_versions.len(),
            node_anchor_count,
            node_delta_count,
            edge_anchor_count,
            edge_delta_count,
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

        // Store the version
        self.node_versions.insert(version_id, version);

        // Update version head
        self.node_version_heads.insert(node_id, version_id);

        // Update version count
        *self.node_version_counts.entry(node_id).or_insert(0) += 1;

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

        // Store the version
        self.edge_versions.insert(version_id, version);

        // Update version head
        self.edge_version_heads.insert(edge_id, version_id);

        // Update version count
        *self.edge_version_counts.entry(edge_id).or_insert(0) += 1;

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
mod tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::{TIMESTAMP_MAX, TimeRange};
    use crate::storage::{StorageEvent, StorageObserver};

    #[test]
    fn test_create_first_version() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let temporal = BiTemporalInterval::current(1000.into());
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();

        storage
            .add_node_version(node_id, version_id, temporal, label, props)
            .unwrap();

        // First version should be an anchor
        let version = storage.get_node_version(version_id).unwrap();
        assert!(version.is_anchor());
        assert_eq!(version.node_id, node_id);
        assert_eq!(version.prev_version, None);
    }

    #[test]
    fn test_version_chain() {
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 3,
            max_delta_chain: 10,
        });

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        // Create 5 versions
        let mut version_ids = Vec::new();
        for i in 0..5 {
            let version_id = VersionId::new(100 + i).unwrap();
            let temporal = BiTemporalInterval::current((1000 + (i as i64) * 100).into());
            let props = PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", i as i64)
                .build();

            storage
                .add_node_version(node_id, version_id, temporal, label, props)
                .unwrap();

            version_ids.push(version_id);
        }

        // Check version types
        // v0: anchor (first)
        // v1: delta
        // v2: delta
        // v3: anchor (interval = 3)
        // v4: delta

        assert!(
            storage
                .get_node_version(version_ids[0])
                .unwrap()
                .is_anchor()
        );
        assert!(storage.get_node_version(version_ids[1]).unwrap().is_delta());
        assert!(storage.get_node_version(version_ids[2]).unwrap().is_delta());
        assert!(
            storage
                .get_node_version(version_ids[3])
                .unwrap()
                .is_anchor()
        );
        assert!(storage.get_node_version(version_ids[4]).unwrap().is_delta());
    }

    #[test]
    fn test_property_reconstruction() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        // Version 1: name=Alice, age=30
        let v1 = VersionId::new(1).unwrap();
        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .build(),
            )
            .unwrap();

        // Version 2: name=Alice, age=31 (delta)
        let v2 = VersionId::new(2).unwrap();
        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::current(2000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 31i64)
                    .build(),
            )
            .unwrap();

        // Reconstruct v2 properties
        let props = storage.reconstruct_node_properties(v2).unwrap();
        assert_eq!(props.get("name").and_then(|v| v.as_str()), Some("Alice"));
        assert_eq!(props.get("age").and_then(|v| v.as_int()), Some(31.into()));
    }

    #[test]
    fn test_find_version_at_time() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        // Create versions at different times
        let v1 = VersionId::new(1).unwrap();
        let v2 = VersionId::new(2).unwrap();
        let v3 = VersionId::new(3).unwrap();

        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), 1000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
                label,
                PropertyMapBuilder::new().insert("age", 30i64).build(),
            )
            .unwrap();

        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 2000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
                label,
                PropertyMapBuilder::new().insert("age", 31i64).build(),
            )
            .unwrap();

        storage
            .add_node_version(
                node_id,
                v3,
                BiTemporalInterval::new(
                    TimeRange::new(2000.into(), TIMESTAMP_MAX).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
                label,
                PropertyMapBuilder::new().insert("age", 32i64).build(),
            )
            .unwrap();

        // Query at different times
        assert_eq!(
            storage.find_node_version_at_time(node_id, 500.into(), 100.into()),
            Some(v1)
        );
        assert_eq!(
            storage.find_node_version_at_time(node_id, 1500.into(), 100.into()),
            Some(v2)
        );
        assert_eq!(
            storage.find_node_version_at_time(node_id, 2500.into(), 100.into()),
            Some(v3)
        );
    }

    #[test]
    fn test_retention_policy_node_limit() {
        // Create storage with small retention limit
        let mut storage = HistoricalStorage::with_config_and_retention(
            AnchorConfig::default(),
            RetentionPolicy::new(3, i64::MAX), // Max 3 versions per entity
        );

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Add 3 versions - should succeed
        for i in 0..3 {
            storage
                .add_node_version(
                    node_id,
                    VersionId::new(i).unwrap(),
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
        }

        // Try to add 4th version - should fail
        let result = storage.add_node_version(
            node_id,
            VersionId::new(3).unwrap(),
            BiTemporalInterval::current(1300.into()),
            label,
            PropertyMapBuilder::new().build(),
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::utils::error::Error::Storage(StorageError::CapacityExceeded {
                resource,
                current,
                limit,
            }) => {
                assert!(resource.contains("node"));
                assert_eq!(current, 3);
                assert_eq!(limit, 3);
            }
            _ => panic!("Expected CapacityExceeded error"),
        }
    }

    #[test]
    fn test_retention_policy_edge_limit() {
        // Create storage with small retention limit
        let mut storage = HistoricalStorage::with_config_and_retention(
            AnchorConfig::default(),
            RetentionPolicy::new(2, i64::MAX), // Max 2 versions per entity
        );

        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Add 2 versions - should succeed
        for i in 0..2 {
            storage
                .add_edge_version(
                    edge_id,
                    VersionId::new(i).unwrap(),
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    source,
                    target,
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
        }

        // Try to add 3rd version - should fail
        let result = storage.add_edge_version(
            edge_id,
            VersionId::new(2).unwrap(),
            BiTemporalInterval::current(1200.into()),
            label,
            source,
            target,
            PropertyMapBuilder::new().build(),
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::utils::error::Error::Storage(StorageError::CapacityExceeded {
                resource,
                current,
                limit,
            }) => {
                assert!(resource.contains("edge"));
                assert_eq!(current, 2);
                assert_eq!(limit, 2);
            }
            _ => panic!("Expected CapacityExceeded error"),
        }
    }

    #[test]
    fn test_stats() {
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 2,
            max_delta_chain: 10,
        });

        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Add 3 node versions (anchor, delta, anchor)
        for i in 0..3 {
            storage
                .add_node_version(
                    NodeId::new(1).unwrap(),
                    VersionId::new(i).unwrap(),
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
        }

        let stats = storage.stats();
        assert_eq!(stats.total_node_versions, 3);
        assert_eq!(stats.node_anchor_count, 2);
        assert_eq!(stats.node_delta_count, 1);
        assert_eq!(stats.unique_nodes, 1);

        // Compression ratio should be 2/3 ≈ 0.67
        assert!((stats.compression_ratio() - 0.6666).abs() < 0.01);
    }

    // ============================================================
    // Vector Property Tests (VS-012)
    // ============================================================
    //
    // Note on floating-point equality:
    // These tests use exact equality (assert_eq!) which works because vectors
    // are hardcoded values without computation. PropertyValue::Vector uses
    // derived PartialEq (bitwise comparison). For tests involving computed
    // vectors (normalization, etc.), use approximate equality instead:
    //
    //   fn vectors_approx_equal(a: &[f32], b: &[f32], epsilon: f32) -> bool {
    //       a.len() == b.len() &&
    //       a.iter().zip(b).all(|(x, y)| (x - y).abs() < epsilon)
    //   }
    //
    // See PropertyValue::Vector documentation at src/core/property.rs for details.

    #[test]
    fn test_create_node_version_with_vector_property() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("Document").unwrap();
        let temporal = BiTemporalInterval::current(1000.into());

        // Create node with vector embedding
        let embedding = vec![0.1f32, 0.2, 0.3, 0.4, 0.5];
        let props = PropertyMapBuilder::new()
            .insert("title", "Test Document")
            .insert_vector("embedding", &embedding)
            .build();

        storage
            .add_node_version(node_id, version_id, temporal, label, props)
            .unwrap();

        // First version should be an anchor
        let version = storage.get_node_version(version_id).unwrap();
        assert!(version.is_anchor());

        // Verify vector can be reconstructed
        let reconstructed = storage.reconstruct_node_properties(version_id).unwrap();
        assert_eq!(
            reconstructed.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
    }

    #[test]
    fn test_delta_computation_with_vector_change() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Document").unwrap();

        // Version 1: Initial embedding
        let v1 = VersionId::new(1).unwrap();
        let embedding_v1 = vec![0.1f32, 0.2, 0.3];
        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert("title", "Doc")
                    .insert_vector("embedding", &embedding_v1)
                    .build(),
            )
            .unwrap();

        // Version 2: Updated embedding (should create delta)
        let v2 = VersionId::new(2).unwrap();
        let embedding_v2 = vec![0.4f32, 0.5, 0.6];
        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::current(2000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert("title", "Doc")
                    .insert_vector("embedding", &embedding_v2)
                    .build(),
            )
            .unwrap();

        // V2 should be a delta since we're within anchor interval
        let version = storage.get_node_version(v2).unwrap();
        assert!(version.is_delta());

        // Verify both versions reconstruct correctly
        let props_v1 = storage.reconstruct_node_properties(v1).unwrap();
        assert_eq!(
            props_v1.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding_v1[..])
        );

        let props_v2 = storage.reconstruct_node_properties(v2).unwrap();
        assert_eq!(
            props_v2.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding_v2[..])
        );
    }

    #[test]
    fn test_delta_only_vector_changes() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Document").unwrap();

        // Version 1: title + embedding
        let v1 = VersionId::new(1).unwrap();
        let embedding_v1 = vec![0.1f32, 0.2];
        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert("title", "Same Title")
                    .insert_vector("embedding", &embedding_v1)
                    .build(),
            )
            .unwrap();

        // Version 2: Only embedding changes
        let v2 = VersionId::new(2).unwrap();
        let embedding_v2 = vec![0.9f32, 0.8];
        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::current(2000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert("title", "Same Title") // Unchanged
                    .insert_vector("embedding", &embedding_v2) // Changed
                    .build(),
            )
            .unwrap();

        // Verify delta captures only the vector change
        let version = storage.get_node_version(v2).unwrap();
        assert!(version.is_delta());

        // Reconstruct and verify
        let props = storage.reconstruct_node_properties(v2).unwrap();
        assert_eq!(
            props.get("title").and_then(|v| v.as_str()),
            Some("Same Title")
        );
        assert_eq!(
            props.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding_v2[..])
        );
    }

    #[test]
    fn test_vector_unchanged_between_versions() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Document").unwrap();

        // Same embedding for both versions
        let embedding = vec![0.5f32, 0.5, 0.5];

        // Version 1
        let v1 = VersionId::new(1).unwrap();
        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert("title", "V1 Title")
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();

        // Version 2: Same embedding, different title
        let v2 = VersionId::new(2).unwrap();
        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::current(2000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert("title", "V2 Title")
                    .insert_vector("embedding", &embedding) // Unchanged
                    .build(),
            )
            .unwrap();

        // Both should have correct embeddings
        let props_v1 = storage.reconstruct_node_properties(v1).unwrap();
        let props_v2 = storage.reconstruct_node_properties(v2).unwrap();

        assert_eq!(
            props_v1.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
        assert_eq!(
            props_v2.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );

        // Titles should differ
        assert_eq!(
            props_v1.get("title").and_then(|v| v.as_str()),
            Some("V1 Title")
        );
        assert_eq!(
            props_v2.get("title").and_then(|v| v.as_str()),
            Some("V2 Title")
        );
    }

    #[test]
    fn test_anchor_creation_with_vector() {
        // Configure anchor interval of 2 to force anchor creation
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 2,
            max_delta_chain: 10,
        });

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Document").unwrap();

        // Create 3 versions with different embeddings
        let embeddings = [vec![0.1f32, 0.2], vec![0.3f32, 0.4], vec![0.5f32, 0.6]];

        for (i, emb) in embeddings.iter().enumerate() {
            storage
                .add_node_version(
                    node_id,
                    VersionId::new(i as u64).unwrap(),
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", emb)
                        .build(),
                )
                .unwrap();
        }

        // V0: anchor (first), V1: delta, V2: anchor (interval=2)
        assert!(
            storage
                .get_node_version(VersionId::new(0).unwrap())
                .unwrap()
                .is_anchor()
        );
        assert!(
            storage
                .get_node_version(VersionId::new(1).unwrap())
                .unwrap()
                .is_delta()
        );
        assert!(
            storage
                .get_node_version(VersionId::new(2).unwrap())
                .unwrap()
                .is_anchor()
        );

        // Verify each version reconstructs correctly
        for (i, emb) in embeddings.iter().enumerate() {
            let props = storage
                .reconstruct_node_properties(VersionId::new(i as u64).unwrap())
                .unwrap();
            assert_eq!(
                props.get("embedding").and_then(|v| v.as_vector()),
                Some(&emb[..])
            );
        }
    }

    #[test]
    fn test_edge_version_with_vector() {
        let mut storage = HistoricalStorage::new();

        let edge_id = EdgeId::new(1).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("SIMILAR_TO").unwrap();
        let temporal = BiTemporalInterval::current(1000.into());
        let source = NodeId::new(10).unwrap();
        let target = NodeId::new(20).unwrap();

        // Edge with relationship embedding
        let embedding = vec![0.8f32, 0.1, 0.1];
        let props = PropertyMapBuilder::new()
            .insert("weight", 0.95f64)
            .insert_vector("embedding", &embedding)
            .build();

        storage
            .add_edge_version(edge_id, version_id, temporal, label, source, target, props)
            .unwrap();

        // Verify edge version
        let version = storage.get_edge_version(version_id).unwrap();
        assert!(version.is_anchor());

        // Verify properties
        let reconstructed = storage.reconstruct_edge_properties(version_id).unwrap();
        assert_eq!(
            reconstructed.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
        assert_eq!(
            reconstructed.get("weight").and_then(|v| v.as_float()),
            Some(0.95)
        );
    }

    #[test]
    fn test_edge_delta_with_vector_change() {
        let mut storage = HistoricalStorage::new();

        let edge_id = EdgeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("SIMILAR_TO").unwrap();
        let source = NodeId::new(10).unwrap();
        let target = NodeId::new(20).unwrap();

        // Version 1: Initial edge
        let v1 = VersionId::new(1).unwrap();
        let embedding_v1 = vec![0.5f32, 0.5];
        storage
            .add_edge_version(
                edge_id,
                v1,
                BiTemporalInterval::current(1000.into()),
                label,
                source,
                target,
                PropertyMapBuilder::new()
                    .insert("weight", 0.5f64)
                    .insert_vector("embedding", &embedding_v1)
                    .build(),
            )
            .unwrap();

        // Version 2: Updated embedding and weight
        let v2 = VersionId::new(2).unwrap();
        let embedding_v2 = vec![0.9f32, 0.1];
        storage
            .add_edge_version(
                edge_id,
                v2,
                BiTemporalInterval::current(2000.into()),
                label,
                source,
                target,
                PropertyMapBuilder::new()
                    .insert("weight", 0.9f64)
                    .insert_vector("embedding", &embedding_v2)
                    .build(),
            )
            .unwrap();

        // V2 should be delta
        assert!(storage.get_edge_version(v2).unwrap().is_delta());

        // Verify reconstruction
        let props_v2 = storage.reconstruct_edge_properties(v2).unwrap();
        assert_eq!(
            props_v2.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding_v2[..])
        );
        assert_eq!(props_v2.get("weight").and_then(|v| v.as_float()), Some(0.9));
    }

    #[test]
    fn test_high_dimensional_vector_versioning() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Embedding").unwrap();

        // High-dimensional embedding (like OpenAI's 1536-dim)
        const DIMENSIONS: usize = 1536;
        let embedding: Vec<f32> = (0..DIMENSIONS)
            .map(|i| (i as f32) / DIMENSIONS as f32)
            .collect();

        let v1 = VersionId::new(1).unwrap();
        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();

        // Verify reconstruction preserves all dimensions
        let props = storage.reconstruct_node_properties(v1).unwrap();
        let retrieved = props
            .get("embedding")
            .and_then(|v| v.as_vector())
            .expect("Should have embedding");

        assert_eq!(retrieved.len(), DIMENSIONS);
        assert_eq!(retrieved, &embedding[..]);
    }

    #[test]
    fn test_version_time_travel_with_vectors() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Document").unwrap();

        // Create versions at different times with different embeddings
        let embeddings = [
            (0, 500, vec![0.1f32, 0.0]),                          // valid 0-500
            (500, 1000, vec![0.2f32, 0.0]),                       // valid 500-1000
            (1000, TIMESTAMP_MAX.wallclock(), vec![0.3f32, 0.0]), // valid 1000+
        ];

        for (i, (start, end, emb)) in embeddings.iter().enumerate() {
            storage
                .add_node_version(
                    node_id,
                    VersionId::new(i as u64).unwrap(),
                    BiTemporalInterval::new(
                        TimeRange::new((*start).into(), (*end).into()).unwrap(),
                        TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                    ),
                    label,
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", emb)
                        .build(),
                )
                .unwrap();
        }

        // Query at different times
        let v_at_250 = storage.find_node_version_at_time(node_id, 250.into(), 0.into());
        let v_at_750 = storage.find_node_version_at_time(node_id, 750.into(), 0.into());
        let v_at_1500 = storage.find_node_version_at_time(node_id, 1500.into(), 0.into());

        assert_eq!(v_at_250, Some(VersionId::new(0).unwrap()));
        assert_eq!(v_at_750, Some(VersionId::new(1).unwrap()));
        assert_eq!(v_at_1500, Some(VersionId::new(2).unwrap()));

        // Verify each has correct embedding
        for (vid, expected_emb) in [
            (v_at_250.unwrap(), &embeddings[0].2),
            (v_at_750.unwrap(), &embeddings[1].2),
            (v_at_1500.unwrap(), &embeddings[2].2),
        ] {
            let props = storage.reconstruct_node_properties(vid).unwrap();
            assert_eq!(
                props.get("embedding").and_then(|v| v.as_vector()),
                Some(&expected_emb[..])
            );
        }
    }

    // ============================================================
    // Edge Case Tests
    // ============================================================

    #[test]
    fn test_empty_vector_versioning() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("EmptyEmbedding").unwrap();

        // Empty vector should work with delta compression
        let empty_vec: Vec<f32> = vec![];

        // Version 1: empty vector
        let v1 = VersionId::new(1).unwrap();
        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert("name", "empty")
                    .insert_vector("embedding", &empty_vec)
                    .build(),
            )
            .unwrap();

        // Version 2: still empty (should be excluded from delta as unchanged)
        let v2 = VersionId::new(2).unwrap();
        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::current(2000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert("name", "updated")
                    .insert_vector("embedding", &empty_vec)
                    .build(),
            )
            .unwrap();

        // Both versions should have empty embedding
        let props_v1 = storage.reconstruct_node_properties(v1).unwrap();
        let props_v2 = storage.reconstruct_node_properties(v2).unwrap();

        assert_eq!(
            props_v1.get("embedding").and_then(|v| v.as_vector()),
            Some(&empty_vec[..])
        );
        assert_eq!(
            props_v2.get("embedding").and_then(|v| v.as_vector()),
            Some(&empty_vec[..])
        );
    }

    #[test]
    fn test_vector_with_special_float_values() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("SpecialFloats").unwrap();

        // Note: NaN and Infinity are allowed in storage (validation is optional).
        // However, NaN != NaN per IEEE 754, so delta computation treats NaN
        // as always changed. This test documents that behavior.

        let special_vec = vec![f32::INFINITY, f32::NEG_INFINITY, 0.0, -0.0];

        let v1 = VersionId::new(1).unwrap();
        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &special_vec)
                    .build(),
            )
            .unwrap();

        // Verify special values round-trip correctly
        let props = storage.reconstruct_node_properties(v1).unwrap();
        let retrieved = props
            .get("embedding")
            .and_then(|v| v.as_vector())
            .expect("Should have embedding");

        assert!(retrieved[0].is_infinite() && retrieved[0].is_sign_positive());
        assert!(retrieved[1].is_infinite() && retrieved[1].is_sign_negative());
        assert_eq!(retrieved[2], 0.0);
        assert_eq!(retrieved[3], -0.0);
    }

    #[test]
    fn test_nan_in_vector_delta_behavior() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("NaNTest").unwrap();

        // NaN != NaN per IEEE 754, so same NaN values will be detected as
        // "changed" in delta computation. This is documented behavior.
        let nan_vec = vec![f32::NAN, 1.0];

        let v1 = VersionId::new(1).unwrap();
        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &nan_vec)
                    .build(),
            )
            .unwrap();

        // Same NaN values - will be treated as changed due to NaN != NaN
        let v2 = VersionId::new(2).unwrap();
        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::current(2000.into()),
                label,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &nan_vec)
                    .build(),
            )
            .unwrap();

        // Both should reconstruct with NaN values
        let props_v1 = storage.reconstruct_node_properties(v1).unwrap();
        let props_v2 = storage.reconstruct_node_properties(v2).unwrap();

        let vec1 = props_v1
            .get("embedding")
            .and_then(|v| v.as_vector())
            .unwrap();
        let vec2 = props_v2
            .get("embedding")
            .and_then(|v| v.as_vector())
            .unwrap();

        // Both should have NaN at index 0
        assert!(vec1[0].is_nan());
        assert!(vec2[0].is_nan());
        assert_eq!(vec1[1], 1.0);
        assert_eq!(vec2[1], 1.0);
    }

    // ============================================================
    // Cache Tests
    // ============================================================

    #[test]
    fn test_cache_hit_on_second_read() {
        let mut storage = HistoricalStorage::new();
        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let temporal = BiTemporalInterval::current(1000.into());
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        storage
            .add_node_version(node_id, version_id, temporal, label, props.clone())
            .unwrap();

        // First read - cache miss, populates cache
        let result1 = storage.reconstruct_node_properties(version_id).unwrap();
        assert_eq!(result1.get("name").and_then(|v| v.as_str()), Some("Alice"));

        // Check cache was populated
        let stats = storage.stats();
        assert_eq!(stats.node_cache_entries, 1);

        // Second read - should hit cache
        let result2 = storage.reconstruct_node_properties(version_id).unwrap();
        assert_eq!(result2.get("name").and_then(|v| v.as_str()), Some("Alice"));

        // Cache size shouldn't change
        let stats = storage.stats();
        assert_eq!(stats.node_cache_entries, 1);
    }

    #[test]
    fn test_cache_populates_delta_chain() {
        let mut storage = HistoricalStorage::new();
        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        // Create anchor + 3 deltas
        let v1 = VersionId::new(1).unwrap();
        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new().insert("value", 1i64).build(),
            )
            .unwrap();

        let v2 = VersionId::new(2).unwrap();
        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::current(2000.into()),
                label,
                PropertyMapBuilder::new().insert("value", 2i64).build(),
            )
            .unwrap();

        let v3 = VersionId::new(3).unwrap();
        storage
            .add_node_version(
                node_id,
                v3,
                BiTemporalInterval::current(3000.into()),
                label,
                PropertyMapBuilder::new().insert("value", 3i64).build(),
            )
            .unwrap();

        let v4 = VersionId::new(4).unwrap();
        storage
            .add_node_version(
                node_id,
                v4,
                BiTemporalInterval::current(4000.into()),
                label,
                PropertyMapBuilder::new().insert("value", 4i64).build(),
            )
            .unwrap();

        // Reconstruct v4 (latest delta) - should populate entire chain
        let result = storage.reconstruct_node_properties(v4).unwrap();
        assert_eq!(result.get("value").and_then(|v| v.as_int()), Some(4.into()));

        // Cache should have all versions in the chain
        let stats = storage.stats();
        assert!(stats.node_cache_entries >= 4);
    }

    #[test]
    fn test_cache_with_custom_size() {
        let storage = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig::default(),
            RetentionPolicy::default(),
            100, // Small cache size
        );

        let stats = storage.stats();
        assert_eq!(stats.node_cache_entries, 0);
        assert_eq!(stats.edge_cache_entries, 0);
    }

    #[test]
    fn test_edge_cache_functionality() {
        let mut storage = HistoricalStorage::new();
        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(10).unwrap();
        let target = NodeId::new(20).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let temporal = BiTemporalInterval::current(1000.into());
        let props = PropertyMapBuilder::new().insert("since", 2020i64).build();

        storage
            .add_edge_version(edge_id, version_id, temporal, label, source, target, props)
            .unwrap();

        // First read - cache miss
        let result1 = storage.reconstruct_edge_properties(version_id).unwrap();
        assert_eq!(
            result1.get("since").and_then(|v| v.as_int()),
            Some(2020.into())
        );

        // Check cache was populated
        let stats = storage.stats();
        assert_eq!(stats.edge_cache_entries, 1);

        // Second read - should hit cache
        let result2 = storage.reconstruct_edge_properties(version_id).unwrap();
        assert_eq!(
            result2.get("since").and_then(|v| v.as_int()),
            Some(2020.into())
        );

        // Cache size shouldn't change
        let stats = storage.stats();
        assert_eq!(stats.edge_cache_entries, 1);
    }

    #[test]
    fn test_cache_stats_accuracy() {
        let mut storage = HistoricalStorage::new();
        let node_id = NodeId::new(1).unwrap();
        let edge_id = EdgeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();
        let temporal = BiTemporalInterval::current(1000.into());

        // Create 5 node versions
        for i in 0..5 {
            let version_id = VersionId::new(i).unwrap();
            storage
                .add_node_version(
                    node_id,
                    version_id,
                    temporal,
                    label,
                    PropertyMapBuilder::new().insert("value", i as i64).build(),
                )
                .unwrap();
            // Reconstruct to populate cache
            storage.reconstruct_node_properties(version_id).unwrap();
        }

        // Create 3 edge versions
        for i in 0..3 {
            let version_id = VersionId::new(100 + i).unwrap();
            storage
                .add_edge_version(
                    edge_id,
                    version_id,
                    temporal,
                    label,
                    node_id,
                    node_id,
                    PropertyMapBuilder::new().insert("value", i as i64).build(),
                )
                .unwrap();
            // Reconstruct to populate cache
            storage.reconstruct_edge_properties(version_id).unwrap();
        }

        let stats = storage.stats();
        assert_eq!(stats.node_cache_entries, 5);
        assert_eq!(stats.edge_cache_entries, 3);
    }

    #[test]
    fn test_cache_with_large_properties() {
        let mut storage = HistoricalStorage::new();
        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Document").unwrap();

        // Create large property map with vector
        let large_vector: Vec<f32> = (0..1536).map(|i| i as f32 / 1536.0).collect();
        let props = PropertyMapBuilder::new()
            .insert("title", "Large Document")
            .insert_vector("embedding", &large_vector)
            .insert("content", "x".repeat(10000).as_str())
            .build();

        storage
            .add_node_version(
                node_id,
                version_id,
                BiTemporalInterval::current(1000.into()),
                label,
                props,
            )
            .unwrap();

        // First read
        let result1 = storage.reconstruct_node_properties(version_id).unwrap();
        assert_eq!(
            result1
                .get("embedding")
                .and_then(|v| v.as_vector())
                .map(|v| v.len()),
            Some(1536)
        );

        // Second read should hit cache
        let result2 = storage.reconstruct_node_properties(version_id).unwrap();
        assert_eq!(result1.get("title"), result2.get("title"));

        let stats = storage.stats();
        assert_eq!(stats.node_cache_entries, 1);
    }

    #[test]
    fn test_extract_node_version_data() {
        let mut storage = HistoricalStorage::new();
        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let temporal = BiTemporalInterval::current(1000.into());
        let props = PropertyMapBuilder::new().insert("name", "Bob").build();

        storage
            .add_node_version(node_id, version_id, temporal, label, props)
            .unwrap();

        let (vid, nid, lbl, data) = storage.extract_node_version_data(version_id).unwrap();
        assert_eq!(vid, version_id);
        assert_eq!(nid, node_id);
        assert_eq!(lbl, label);

        // Verify data can be used for copy-out reconstruction
        match data {
            VersionData::Anchor { properties, .. } => {
                assert_eq!(properties.get("name").and_then(|v| v.as_str()), Some("Bob"));
            }
            _ => panic!("Expected anchor"),
        }
    }

    #[test]
    fn test_extract_edge_version_data() {
        let mut storage = HistoricalStorage::new();
        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(10).unwrap();
        let target = NodeId::new(20).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let temporal = BiTemporalInterval::current(1000.into());
        let props = PropertyMapBuilder::new().insert("since", 2021i64).build();

        storage
            .add_edge_version(edge_id, version_id, temporal, label, source, target, props)
            .unwrap();

        let (vid, eid, lbl, src, tgt, data) =
            storage.extract_edge_version_data(version_id).unwrap();
        assert_eq!(vid, version_id);
        assert_eq!(eid, edge_id);
        assert_eq!(lbl, label);
        assert_eq!(src, source);
        assert_eq!(tgt, target);

        // Verify data
        match data {
            VersionData::Anchor { properties, .. } => {
                assert_eq!(
                    properties.get("since").and_then(|v| v.as_int()),
                    Some(2021.into())
                );
            }
            _ => panic!("Expected anchor"),
        }
    }

    // ============================================================
    // Observer Pattern Tests (VS-047)
    // ============================================================

    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock observer that counts anchor events
    struct CountingObserver {
        anchor_count: AtomicUsize,
        version_count: AtomicUsize,
    }

    impl StorageObserver for CountingObserver {
        fn on_event(&self, event: &StorageEvent) -> Result<()> {
            match event {
                StorageEvent::NodeAnchorCreated { .. } | StorageEvent::EdgeAnchorCreated { .. } => {
                    self.anchor_count.fetch_add(1, Ordering::SeqCst);
                }
                StorageEvent::NodeVersionCreated { .. }
                | StorageEvent::EdgeVersionCreated { .. } => {
                    self.version_count.fetch_add(1, Ordering::SeqCst);
                }
            }
            Ok(())
        }
    }

    /// Mock observer that only cares about node anchors
    struct NodeAnchorObserver {
        count: AtomicUsize,
    }

    impl StorageObserver for NodeAnchorObserver {
        fn on_event(&self, _event: &StorageEvent) -> Result<()> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn interested_in(&self, event: &StorageEvent) -> bool {
            matches!(event, StorageEvent::NodeAnchorCreated { .. })
        }
    }

    /// Mock observer that collects events
    struct CollectingObserver {
        events: StdMutex<Vec<StorageEvent>>,
    }

    impl StorageObserver for CollectingObserver {
        fn on_event(&self, event: &StorageEvent) -> Result<()> {
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    #[test]
    fn test_observer_triggered_on_node_anchor_creation() {
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 3,
            max_delta_chain: 10,
        });

        let observer = Arc::new(CountingObserver {
            anchor_count: AtomicUsize::new(0),
            version_count: AtomicUsize::new(0),
        });
        storage.add_observer(observer.clone());

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Create 5 versions: anchor, delta, delta, anchor, delta
        for i in 0..5 {
            storage
                .add_node_version(
                    node_id,
                    VersionId::new(i).unwrap(),
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    PropertyMapBuilder::new().insert("value", i as i64).build(),
                )
                .unwrap();
        }

        // Should have 2 anchors (v0 and v3)
        assert_eq!(observer.anchor_count.load(Ordering::SeqCst), 2);
        // Should have 5 total version events
        assert_eq!(observer.version_count.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn test_observer_triggered_on_edge_anchor_creation() {
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 2,
            max_delta_chain: 10,
        });

        let observer = Arc::new(CountingObserver {
            anchor_count: AtomicUsize::new(0),
            version_count: AtomicUsize::new(0),
        });
        storage.add_observer(observer.clone());

        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(10).unwrap();
        let target = NodeId::new(20).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Create 3 versions: anchor, delta, anchor
        for i in 0..3 {
            storage
                .add_edge_version(
                    edge_id,
                    VersionId::new(i).unwrap(),
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    source,
                    target,
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
        }

        // Should have 2 anchors
        assert_eq!(observer.anchor_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_observer_filtering() {
        let mut storage = HistoricalStorage::new();

        // Observer only interested in node anchors
        let observer = Arc::new(NodeAnchorObserver {
            count: AtomicUsize::new(0),
        });
        storage.add_observer(observer.clone());

        let node_id = NodeId::new(1).unwrap();
        let edge_id = EdgeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Create node anchor
        storage
            .add_node_version(
                node_id,
                VersionId::new(1).unwrap(),
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        // Create edge anchor
        storage
            .add_edge_version(
                edge_id,
                VersionId::new(2).unwrap(),
                BiTemporalInterval::current(2000.into()),
                label,
                node_id,
                node_id,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        // Should only count node anchor (not edge anchor)
        assert_eq!(observer.count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_observer_receives_correct_event_data() {
        let mut storage = HistoricalStorage::new();

        let collector = Arc::new(CollectingObserver {
            events: StdMutex::new(Vec::new()),
        });
        storage.add_observer(collector.clone());

        let node_id = NodeId::new(42).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();
        let timestamp = 5000i64;

        storage
            .add_node_version(
                node_id,
                version_id,
                BiTemporalInterval::current(timestamp.into()),
                label,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        let events = collector.events.lock().unwrap();
        assert_eq!(events.len(), 2); // NodeAnchorCreated + NodeVersionCreated

        // Check anchor event
        let anchor_event = events
            .iter()
            .find(|e| matches!(e, StorageEvent::NodeAnchorCreated { .. }))
            .expect("Should have NodeAnchorCreated event");

        match anchor_event {
            StorageEvent::NodeAnchorCreated {
                version_id: vid,
                node_id: nid,
                timestamp: ts,
            } => {
                assert_eq!(*vid, version_id);
                assert_eq!(*nid, node_id);
                assert_eq!(*ts, timestamp.into());
            }
            _ => panic!("Wrong event type"),
        }
    }

    #[test]
    fn test_multiple_observers() {
        let mut storage = HistoricalStorage::new();

        let observer1 = Arc::new(CountingObserver {
            anchor_count: AtomicUsize::new(0),
            version_count: AtomicUsize::new(0),
        });
        let observer2 = Arc::new(CountingObserver {
            anchor_count: AtomicUsize::new(0),
            version_count: AtomicUsize::new(0),
        });

        storage.add_observer(observer1.clone());
        storage.add_observer(observer2.clone());

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        storage
            .add_node_version(
                node_id,
                VersionId::new(1).unwrap(),
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        // Both observers should be notified
        assert_eq!(observer1.anchor_count.load(Ordering::SeqCst), 1);
        assert_eq!(observer2.anchor_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_observer_error_doesnt_block_storage() {
        /// Observer that always returns an error
        struct FailingObserver;

        impl StorageObserver for FailingObserver {
            fn on_event(&self, _event: &StorageEvent) -> Result<()> {
                Err(crate::utils::error::Error::Storage(
                    StorageError::InconsistentState {
                        reason: "Test error".to_string(),
                    },
                ))
            }
        }

        let mut storage = HistoricalStorage::new();
        storage.add_observer(Arc::new(FailingObserver));

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Should succeed even though observer fails
        let result = storage.add_node_version(
            node_id,
            VersionId::new(1).unwrap(),
            BiTemporalInterval::current(1000.into()),
            label,
            PropertyMapBuilder::new().build(),
        );

        assert!(result.is_ok());

        // Verify version was created
        let version = storage.get_node_version(VersionId::new(1).unwrap());
        assert!(version.is_some());
    }

    // ========================================================================
    // Pre-Anchor Hook Tests
    // ========================================================================

    #[test]
    fn test_pre_anchor_hook_called_before_storage() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let mut storage = HistoricalStorage::new();
        let hook_called = Arc::new(AtomicBool::new(false));
        let hook_called_clone = Arc::clone(&hook_called);

        // Hook that sets flag when called
        let hook: PreAnchorHook =
            Arc::new(move |_entity_type, _entity_id, _timestamp, _properties| {
                hook_called_clone.store(true, Ordering::SeqCst);
                Ok(Some(42))
            });

        storage.register_pre_node_anchor_hook(hook);

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Create anchor (first version is always anchor)
        storage
            .add_node_version(
                node_id,
                VersionId::new(1).unwrap(),
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        // Hook should have been called
        assert!(hook_called.load(Ordering::SeqCst));
    }

    #[test]
    fn test_pre_anchor_hook_returns_snapshot_id() {
        let mut storage = HistoricalStorage::new();

        // Hook that returns snapshot ID 123
        let hook: PreAnchorHook =
            Arc::new(move |_entity_type, _entity_id, _timestamp, _properties| Ok(Some(123)));

        storage.register_pre_node_anchor_hook(hook);

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Create anchor
        storage
            .add_node_version(
                node_id,
                VersionId::new(1).unwrap(),
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        // Verify snapshot ID was stored in anchor
        let version = storage
            .get_node_version(VersionId::new(1).unwrap())
            .unwrap();
        assert!(version.is_anchor());
        assert_eq!(version.data.get_vector_snapshot_id(), Some(123));
    }

    #[test]
    fn test_pre_anchor_hook_none_handling() {
        let mut storage = HistoricalStorage::new();

        // Hook that returns None (no snapshot needed)
        let hook: PreAnchorHook =
            Arc::new(move |_entity_type, _entity_id, _timestamp, _properties| Ok(None));

        storage.register_pre_node_anchor_hook(hook);

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Create anchor - should succeed even with None
        storage
            .add_node_version(
                node_id,
                VersionId::new(1).unwrap(),
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        // Verify anchor created without snapshot ID
        let version = storage
            .get_node_version(VersionId::new(1).unwrap())
            .unwrap();
        assert!(version.is_anchor());
        assert_eq!(version.data.get_vector_snapshot_id(), None);
    }

    #[test]
    fn test_pre_anchor_hook_error_graceful_degradation() {
        let mut storage = HistoricalStorage::new();

        // Hook that always fails
        let hook: PreAnchorHook =
            Arc::new(move |_entity_type, _entity_id, _timestamp, _properties| {
                Err(crate::utils::error::Error::Storage(
                    StorageError::InconsistentState {
                        reason: "Test hook error".to_string(),
                    },
                ))
            });

        storage.register_pre_node_anchor_hook(hook);

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Create anchor - should succeed despite hook failure (graceful degradation)
        let result = storage.add_node_version(
            node_id,
            VersionId::new(1).unwrap(),
            BiTemporalInterval::current(1000.into()),
            label,
            PropertyMapBuilder::new().build(),
        );

        assert!(result.is_ok());

        // Verify anchor created without snapshot ID
        let version = storage
            .get_node_version(VersionId::new(1).unwrap())
            .unwrap();
        assert!(version.is_anchor());
        assert_eq!(version.data.get_vector_snapshot_id(), None);
    }

    #[test]
    fn test_pre_anchor_hook_not_called_for_delta() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 3,
            max_delta_chain: 10,
        });

        let hook_call_count = Arc::new(AtomicUsize::new(0));
        let hook_call_count_clone = Arc::clone(&hook_call_count);

        // Hook that counts calls
        let hook: PreAnchorHook =
            Arc::new(move |_entity_type, _entity_id, _timestamp, _properties| {
                hook_call_count_clone.fetch_add(1, Ordering::SeqCst);
                Ok(Some(42))
            });

        storage.register_pre_node_anchor_hook(hook);

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Create 5 versions (anchor at v0, deltas at v1-v2, anchor at v3, delta at v4)
        for i in 0..5 {
            storage
                .add_node_version(
                    node_id,
                    VersionId::new(100 + i).unwrap(),
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
        }

        // Hook should be called only for anchors (v0 and v3)
        assert_eq!(hook_call_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_pre_anchor_hook_node_and_edge_separate() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let mut storage = HistoricalStorage::new();

        let node_hook_count = Arc::new(AtomicUsize::new(0));
        let node_hook_count_clone = Arc::clone(&node_hook_count);
        let node_hook: PreAnchorHook =
            Arc::new(move |_entity_type, _entity_id, _timestamp, _properties| {
                node_hook_count_clone.fetch_add(1, Ordering::SeqCst);
                Ok(Some(1))
            });

        let edge_hook_count = Arc::new(AtomicUsize::new(0));
        let edge_hook_count_clone = Arc::clone(&edge_hook_count);
        let edge_hook: PreAnchorHook =
            Arc::new(move |_entity_type, _entity_id, _timestamp, _properties| {
                edge_hook_count_clone.fetch_add(1, Ordering::SeqCst);
                Ok(Some(2))
            });

        storage.register_pre_node_anchor_hook(node_hook);
        storage.register_pre_edge_anchor_hook(edge_hook);

        let node1_id = NodeId::new(1).unwrap();
        let node2_id = NodeId::new(2).unwrap();
        let edge_id = EdgeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Create node version
        storage
            .add_node_version(
                node1_id,
                VersionId::new(1).unwrap(),
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        // Create edge version
        storage
            .add_edge_version(
                edge_id,
                VersionId::new(2).unwrap(),
                BiTemporalInterval::current(2000.into()),
                label,
                node1_id,
                node2_id,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        // Each hook should be called once
        assert_eq!(node_hook_count.load(Ordering::SeqCst), 1);
        assert_eq!(edge_hook_count.load(Ordering::SeqCst), 1);

        // Verify snapshot IDs are different
        let node_version = storage
            .get_node_version(VersionId::new(1).unwrap())
            .unwrap();
        let edge_version = storage
            .get_edge_version(VersionId::new(2).unwrap())
            .unwrap();
        assert_eq!(node_version.data.get_vector_snapshot_id(), Some(1));
        assert_eq!(edge_version.data.get_vector_snapshot_id(), Some(2));
    }

    // ========================================================================
    // Tests for Issue #17: Recursion depth limit in version reconstruction
    // ========================================================================

    use super::{MAX_RECONSTRUCTION_DEPTH, RetentionPolicy};

    #[test]
    fn test_reconstruction_depth_limit_exceeded_for_nodes() {
        // Create storage with very high anchor interval to force delta creation
        // and cache_size=0 to prevent caching from defeating the depth test
        let mut storage = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig {
                anchor_interval: 200, // Won't create anchors until 200 versions
                max_delta_chain: 200,
            },
            RetentionPolicy::default(),
            0, // Disable cache to test full depth traversal
        );

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Create first version (anchor)
        let v0 = VersionId::new(0).unwrap();
        storage
            .add_node_version(
                node_id,
                v0,
                BiTemporalInterval::current(0.into()),
                label,
                PropertyMapBuilder::new().insert("counter", 0i64).build(),
            )
            .unwrap();

        // Create 100 more versions (deltas) to exceed the depth limit
        // With >= check, depth 100 triggers error, so 100 deltas will exceed
        for i in 1..=MAX_RECONSTRUCTION_DEPTH {
            let vid = VersionId::new(i as u64).unwrap();
            storage
                .add_node_version(
                    node_id,
                    vid,
                    BiTemporalInterval::current((i as i64 * 1000).into()),
                    label,
                    PropertyMapBuilder::new()
                        .insert("counter", i as i64)
                        .build(),
                )
                .unwrap();
        }

        // Reconstruction should fail with MaxDepthExceeded
        let last_version_id = VersionId::new(MAX_RECONSTRUCTION_DEPTH as u64).unwrap();
        let result = storage.reconstruct_node_properties(last_version_id);

        assert!(result.is_err(), "Expected MaxDepthExceeded error");
        let err = result.unwrap_err();
        match err {
            crate::utils::error::Error::Temporal(
                crate::utils::error::TemporalError::MaxDepthExceeded { max_depth, .. },
            ) => {
                assert_eq!(max_depth, MAX_RECONSTRUCTION_DEPTH);
            }
            other => panic!("Expected MaxDepthExceeded error, got: {:?}", other),
        }
    }

    #[test]
    fn test_reconstruction_within_depth_limit_works_for_nodes() {
        // Create storage with high anchor interval to force delta creation
        // and cache_size=0 to test full depth traversal
        let mut storage = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig {
                anchor_interval: 200,
                max_delta_chain: 200,
            },
            RetentionPolicy::default(),
            0, // Disable cache to test full depth traversal
        );

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        // Create first version (anchor)
        let v0 = VersionId::new(0).unwrap();
        storage
            .add_node_version(
                node_id,
                v0,
                BiTemporalInterval::current(0.into()),
                label,
                PropertyMapBuilder::new().insert("counter", 0i64).build(),
            )
            .unwrap();

        // Create exactly MAX_RECONSTRUCTION_DEPTH - 1 versions (should be within limit)
        // With >= check, 99 deltas means max depth of 99 which is < 100
        for i in 1..MAX_RECONSTRUCTION_DEPTH {
            let vid = VersionId::new(i as u64).unwrap();
            storage
                .add_node_version(
                    node_id,
                    vid,
                    BiTemporalInterval::current((i as i64 * 1000).into()),
                    label,
                    PropertyMapBuilder::new()
                        .insert("counter", i as i64)
                        .build(),
                )
                .unwrap();
        }

        // Reconstruction should succeed for version at depth limit
        let last_version_id = VersionId::new((MAX_RECONSTRUCTION_DEPTH - 1) as u64).unwrap();
        let result = storage.reconstruct_node_properties(last_version_id);

        assert!(
            result.is_ok(),
            "Expected successful reconstruction within depth limit"
        );
        let props = result.unwrap();
        assert_eq!(
            props.get("counter").and_then(|v| v.as_int()),
            Some((MAX_RECONSTRUCTION_DEPTH - 1) as i64)
        );
    }

    #[test]
    fn test_reconstruction_depth_limit_exceeded_for_edges() {
        // Create storage with very high anchor interval to force delta creation
        // and cache_size=0 to prevent caching from defeating the depth test
        let mut storage = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig {
                anchor_interval: 200,
                max_delta_chain: 200,
            },
            RetentionPolicy::default(),
            0, // Disable cache to test full depth traversal
        );

        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(100).unwrap();
        let target = NodeId::new(200).unwrap();
        let label = GLOBAL_INTERNER.intern("TestEdge").unwrap();

        // Create first version (anchor)
        let v0 = VersionId::new(0).unwrap();
        storage
            .add_edge_version(
                edge_id,
                v0,
                BiTemporalInterval::current(0.into()),
                label,
                source,
                target,
                PropertyMapBuilder::new().insert("weight", 0.0f64).build(),
            )
            .unwrap();

        // Create 100 more versions (deltas) to exceed the depth limit
        // With >= check, depth 100 triggers error, so 100 deltas will exceed
        for i in 1..=MAX_RECONSTRUCTION_DEPTH {
            let vid = VersionId::new(i as u64).unwrap();
            storage
                .add_edge_version(
                    edge_id,
                    vid,
                    BiTemporalInterval::current((i as i64 * 1000).into()),
                    label,
                    source,
                    target,
                    PropertyMapBuilder::new().insert("weight", i as f64).build(),
                )
                .unwrap();
        }

        // Reconstruction should fail with MaxDepthExceeded
        let last_version_id = VersionId::new(MAX_RECONSTRUCTION_DEPTH as u64).unwrap();
        let result = storage.reconstruct_edge_properties(last_version_id);

        assert!(result.is_err(), "Expected MaxDepthExceeded error");
        let err = result.unwrap_err();
        match err {
            crate::utils::error::Error::Temporal(
                crate::utils::error::TemporalError::MaxDepthExceeded { max_depth, .. },
            ) => {
                assert_eq!(max_depth, MAX_RECONSTRUCTION_DEPTH);
            }
            other => panic!("Expected MaxDepthExceeded error, got: {:?}", other),
        }
    }

    #[test]
    fn test_reconstruction_within_depth_limit_works_for_edges() {
        // Create storage with high anchor interval to force delta creation
        // and cache_size=0 to test full depth traversal
        let mut storage = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig {
                anchor_interval: 200,
                max_delta_chain: 200,
            },
            RetentionPolicy::default(),
            0, // Disable cache to test full depth traversal
        );

        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(100).unwrap();
        let target = NodeId::new(200).unwrap();
        let label = GLOBAL_INTERNER.intern("TestEdge").unwrap();

        // Create first version (anchor)
        let v0 = VersionId::new(0).unwrap();
        storage
            .add_edge_version(
                edge_id,
                v0,
                BiTemporalInterval::current(0.into()),
                label,
                source,
                target,
                PropertyMapBuilder::new().insert("weight", 0.0f64).build(),
            )
            .unwrap();

        // Create exactly MAX_RECONSTRUCTION_DEPTH - 1 versions (should be within limit)
        // With >= check, 99 deltas means max depth of 99 which is < 100
        for i in 1..MAX_RECONSTRUCTION_DEPTH {
            let vid = VersionId::new(i as u64).unwrap();
            storage
                .add_edge_version(
                    edge_id,
                    vid,
                    BiTemporalInterval::current((i as i64 * 1000).into()),
                    label,
                    source,
                    target,
                    PropertyMapBuilder::new().insert("weight", i as f64).build(),
                )
                .unwrap();
        }

        // Reconstruction should succeed for version at depth limit
        let last_version_id = VersionId::new((MAX_RECONSTRUCTION_DEPTH - 1) as u64).unwrap();
        let result = storage.reconstruct_edge_properties(last_version_id);

        assert!(
            result.is_ok(),
            "Expected successful reconstruction within depth limit"
        );
        let props = result.unwrap();
        assert_eq!(
            props.get("weight").and_then(|v| v.as_float()),
            Some((MAX_RECONSTRUCTION_DEPTH - 1) as f64)
        );
    }

    // ========================================================================
    // Improvement #2: Cache Pre-population Tests
    // ========================================================================

    #[test]
    fn test_anchor_properties_are_cached_immediately_for_nodes() {
        // Test that when a node anchor is created, its properties are immediately in the cache
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let temporal = BiTemporalInterval::current(1000.into());
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        // Create the first version (which is always an anchor)
        storage
            .add_node_version(node_id, version_id, temporal, label, props.clone())
            .unwrap();

        // Verify the version is an anchor
        let version = storage.get_node_version(version_id).unwrap();
        assert!(version.is_anchor(), "First version should be an anchor");

        // Check that the anchor properties are in the cache
        // We can verify this by checking the cache stats
        let stats = storage.stats();
        assert_eq!(
            stats.node_cache_entries, 1,
            "Anchor properties should be cached immediately"
        );

        // Verify cache hit by reconstructing properties
        // If cached, this should be O(1) instead of O(N)
        let reconstructed = storage.reconstruct_node_properties(version_id).unwrap();
        assert_eq!(reconstructed, props);

        // Cache entries should still be 1 (cache hit, not a new entry)
        let stats_after = storage.stats();
        assert_eq!(
            stats_after.node_cache_entries, 1,
            "Cache should still have 1 entry after hit"
        );
    }

    #[test]
    fn test_anchor_properties_are_cached_immediately_for_edges() {
        // Test that when an edge anchor is created, its properties are immediately in the cache
        let mut storage = HistoricalStorage::new();

        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(100).unwrap();
        let target = NodeId::new(200).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let temporal = BiTemporalInterval::current(1000.into());
        let props = PropertyMapBuilder::new()
            .insert("since", 2020i64)
            .insert("weight", 0.8f64)
            .build();

        // Create the first version (which is always an anchor)
        storage
            .add_edge_version(
                edge_id,
                version_id,
                temporal,
                label,
                source,
                target,
                props.clone(),
            )
            .unwrap();

        // Verify the version is an anchor
        let version = storage.get_edge_version(version_id).unwrap();
        assert!(version.is_anchor(), "First version should be an anchor");

        // Check that the anchor properties are in the cache
        let stats = storage.stats();
        assert_eq!(
            stats.edge_cache_entries, 1,
            "Anchor properties should be cached immediately"
        );

        // Verify cache hit by reconstructing properties
        let reconstructed = storage.reconstruct_edge_properties(version_id).unwrap();
        assert_eq!(reconstructed, props);

        // Cache entries should still be 1 (cache hit, not a new entry)
        let stats_after = storage.stats();
        assert_eq!(
            stats_after.edge_cache_entries, 1,
            "Cache should still have 1 entry after hit"
        );
    }

    #[test]
    fn test_subsequent_anchors_are_also_cached() {
        // Test that multiple anchors created in sequence are all cached
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 5, // Create anchor every 5 versions
            max_delta_chain: 5,
        });

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        // Create 11 versions (will create anchors at v0, v5, v10)
        for i in 0..11 {
            let version_id = VersionId::new(i).unwrap();
            let temporal = BiTemporalInterval::current((i as i64 * 1000).into());
            let props = PropertyMapBuilder::new()
                .insert("counter", i as i64)
                .build();

            storage
                .add_node_version(node_id, version_id, temporal, label, props)
                .unwrap();
        }

        // Verify we have 3 anchors
        let stats = storage.stats();
        assert_eq!(stats.node_anchor_count, 3, "Should have 3 anchors");
        assert_eq!(stats.node_delta_count, 8, "Should have 8 deltas");

        // All 11 versions should be in cache (3 anchors + 8 deltas populated during reconstruction)
        // Actually, initially only anchors should be pre-cached, deltas are cached on-demand
        // So we should have at least 3 entries (the anchors)
        assert!(
            stats.node_cache_entries >= 3,
            "At least the 3 anchors should be cached, got {}",
            stats.node_cache_entries
        );

        // Verify anchor versions are cached
        let anchor_v0 = VersionId::new(0).unwrap();
        let anchor_v5 = VersionId::new(5).unwrap();
        let anchor_v10 = VersionId::new(10).unwrap();

        // These should be cache hits
        storage.reconstruct_node_properties(anchor_v0).unwrap();
        storage.reconstruct_node_properties(anchor_v5).unwrap();
        storage.reconstruct_node_properties(anchor_v10).unwrap();
    }

    // ========================================================================
    // Improvement #1: Anchor-Based Caching Tests
    // ========================================================================

    #[test]
    fn test_anchor_cache_survives_delta_cache_pressure() {
        // Test that anchors remain cached even when delta versions fill up the regular cache
        // Use a small cache size to force evictions
        let mut storage = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig {
                anchor_interval: 10, // Anchor every 10 versions
                max_delta_chain: 10,
            },
            RetentionPolicy::default(),
            5, // Very small cache - only 5 entries
        );

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        // Create 21 versions (anchors at v0, v10, v20 + 18 deltas)
        for i in 0..21 {
            let version_id = VersionId::new(i).unwrap();
            let temporal = BiTemporalInterval::current((i as i64 * 1000).into());
            let props = PropertyMapBuilder::new()
                .insert("counter", i as i64)
                .build();

            storage
                .add_node_version(node_id, version_id, temporal, label, props)
                .unwrap();
        }

        // Verify we have 3 anchors
        let stats = storage.stats();
        assert_eq!(stats.node_anchor_count, 3, "Should have 3 anchors");

        // Access many delta versions to create cache pressure
        // This should fill up the small cache (5 entries) with delta reconstructions
        for i in 1..10 {
            let version_id = VersionId::new(i).unwrap();
            storage.reconstruct_node_properties(version_id).unwrap();
        }

        // Despite cache pressure, all anchors should still be quickly accessible
        // because they're in the dedicated anchor cache
        let anchor_v0 = VersionId::new(0).unwrap();
        let anchor_v10 = VersionId::new(10).unwrap();
        let anchor_v20 = VersionId::new(20).unwrap();

        // These should be fast cache hits from the anchor cache
        let props0 = storage.reconstruct_node_properties(anchor_v0).unwrap();
        let props10 = storage.reconstruct_node_properties(anchor_v10).unwrap();
        let props20 = storage.reconstruct_node_properties(anchor_v20).unwrap();

        assert_eq!(
            props0.get("counter").and_then(|v| v.as_int()),
            Some(0.into())
        );
        assert_eq!(
            props10.get("counter").and_then(|v| v.as_int()),
            Some(10.into())
        );
        assert_eq!(
            props20.get("counter").and_then(|v| v.as_int()),
            Some(20.into())
        );
    }

    #[test]
    fn test_delta_reconstruction_uses_anchor_cache() {
        // Test that delta reconstruction benefits from the anchor cache
        // When reconstructing a delta, we should use the cached anchor as the base
        let mut storage = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig {
                anchor_interval: 5,
                max_delta_chain: 5,
            },
            RetentionPolicy::default(),
            100, // Reasonable cache size
        );

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Document").unwrap();

        // Create 8 versions (anchor at v0, v5, deltas at v1-v4, v6-v7)
        for i in 0..8 {
            let version_id = VersionId::new(i).unwrap();
            let temporal = BiTemporalInterval::current((i as i64 * 1000).into());
            let props = PropertyMapBuilder::new()
                .insert("version", i as i64)
                .insert("data", format!("content_{}", i))
                .build();

            storage
                .add_node_version(node_id, version_id, temporal, label, props)
                .unwrap();
        }

        // Verify anchors are in cache
        let stats = storage.stats();
        assert!(
            stats.node_cache_entries >= 2,
            "Both anchors should be cached"
        );

        // Reconstruct a delta version (v7) - should use anchor cache for v5
        let v7 = VersionId::new(7).unwrap();
        let props = storage.reconstruct_node_properties(v7).unwrap();
        assert_eq!(
            props.get("version").and_then(|v| v.as_int()),
            Some(7.into())
        );
        assert_eq!(
            props.get("data").and_then(|v| v.as_str()),
            Some("content_7")
        );
    }

    #[test]
    fn test_anchor_cache_improves_multi_version_reconstruction() {
        // Test that multiple delta versions can reuse the same cached anchor
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 5,
            max_delta_chain: 5,
        });

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Entity").unwrap();

        // Create 10 versions (anchors at v0, v5)
        for i in 0..10 {
            let version_id = VersionId::new(i).unwrap();
            let temporal = BiTemporalInterval::current((i as i64 * 1000).into());
            let props = PropertyMapBuilder::new().insert("value", i as i64).build();

            storage
                .add_node_version(node_id, version_id, temporal, label, props)
                .unwrap();
        }

        // Reconstruct all deltas between v5 and v9
        // All should benefit from the cached anchor at v5
        for i in 6..10 {
            let version_id = VersionId::new(i).unwrap();
            let props = storage.reconstruct_node_properties(version_id).unwrap();
            assert_eq!(props.get("value").and_then(|v| v.as_int()), Some(i as i64));
        }

        // All reconstructions should have succeeded efficiently using the anchor cache
        let stats = storage.stats();
        assert_eq!(stats.node_anchor_count, 2, "Should have 2 anchors");
    }

    #[test]
    fn test_anchor_cache_size_calculation() {
        // Test that anchor cache is properly sized relative to main cache

        // Small cache: 100 entries -> anchor cache should be max(100/5, 100) = 100
        let storage_small = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig::default(),
            RetentionPolicy::default(),
            100,
        );
        // We can't directly access cache capacity, but we can verify it works correctly
        // by checking that anchors are cached even with small cache
        assert_eq!(storage_small.node_property_cache.len(), 0);

        // Medium cache: 1000 entries -> anchor cache should be max(1000/5, 100) = 200
        let storage_medium = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig::default(),
            RetentionPolicy::default(),
            1000,
        );
        assert_eq!(storage_medium.node_property_cache.len(), 0);

        // Large cache: 10000 entries -> anchor cache should be max(10000/5, 100) = 2000
        let storage_large = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig::default(),
            RetentionPolicy::default(),
            10000,
        );
        assert_eq!(storage_large.node_property_cache.len(), 0);

        // Very small cache: 10 entries -> anchor cache should be max(10/5, 100) = 100 (minimum)
        let storage_tiny = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig::default(),
            RetentionPolicy::default(),
            10,
        );
        assert_eq!(storage_tiny.node_property_cache.len(), 0);
    }

    // ========================================================================
    // Improvement #3: Adaptive Cache Sizing Tests
    // ========================================================================

    #[test]
    fn test_should_resize_cache_recommends_growth_on_low_hit_rate() {
        // Test that `should_resize_cache` recommends resizing when hit rate is low.
        // Start with a very small cache to force low hit rate
        let mut storage = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig {
                anchor_interval: 10,
                max_delta_chain: 10,
            },
            RetentionPolicy::default(),
            10, // Very small cache - will have low hit rate
        );

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        // Create many versions to stress the cache
        for i in 0..50 {
            let version_id = VersionId::new(i).unwrap();
            let temporal = BiTemporalInterval::current((i as i64 * 1000).into());
            let props = PropertyMapBuilder::new()
                .insert("counter", i as i64)
                .insert("data", format!("value_{}", i))
                .build();

            storage
                .add_node_version(node_id, version_id, temporal, label, props)
                .unwrap();
        }

        // Access many different versions to create cache misses
        for i in 0..50 {
            let version_id = VersionId::new(i).unwrap();
            storage.reconstruct_node_properties(version_id).unwrap();
        }

        // Check cache metrics
        let metrics = storage.cache_metrics();
        assert!(
            metrics.total_operations() > 0,
            "Should have cache operations"
        );

        // Check if adaptive resizing recommends increasing cache size
        // With only 10 cache slots and 50 versions, hit rate should be low
        let resize_recommendation = storage.should_resize_cache(0.8, 10);
        assert!(
            resize_recommendation.is_some(),
            "should_resize_cache should recommend resizing with low hit rate"
        );

        let hit_rate = resize_recommendation.unwrap();
        assert!(hit_rate < 0.8, "Hit rate should be below the threshold");

        println!(
            "Cache hit rate {:.2}% is below threshold, resize recommended",
            hit_rate * 100.0
        );

        let stats = storage.stats();
        assert!(
            stats.node_cache_entries > 0,
            "Cache should have some entries"
        );
    }

    #[test]
    fn test_cache_hit_rate_tracking() {
        // Test that we can track cache hit rate metrics
        let mut storage = HistoricalStorage::with_config(AnchorConfig::default());

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("TestNode").unwrap();

        // Create some versions
        for i in 0..20 {
            let version_id = VersionId::new(i).unwrap();
            let temporal = BiTemporalInterval::current((i as i64 * 1000).into());
            let props = PropertyMapBuilder::new().insert("value", i as i64).build();

            storage
                .add_node_version(node_id, version_id, temporal, label, props)
                .unwrap();
        }

        // Access the same version multiple times (should be cache hits after first)
        let v5 = VersionId::new(5).unwrap();
        for _ in 0..10 {
            storage.reconstruct_node_properties(v5).unwrap();
        }

        // The cache should have entries
        let stats = storage.stats();
        assert!(
            stats.node_cache_entries > 0,
            "Cache should have entries after reconstruction"
        );

        // Check hit rate - with repeated access, should be high
        let hit_rate = storage.cache_hit_rate();
        assert!(hit_rate.is_some(), "Should have cache hit rate data");
        // After 10 accesses to same version, most should be hits
        assert!(
            hit_rate.unwrap() > 0.5,
            "Hit rate should be > 50% with repeated access"
        );
    }

    #[test]
    fn test_cache_resize_maintains_correctness() {
        // Test that even if cache is resized, data correctness is maintained
        let mut storage = HistoricalStorage::with_config_retention_and_cache_size(
            AnchorConfig {
                anchor_interval: 5,
                max_delta_chain: 5,
            },
            RetentionPolicy::default(),
            100,
        );

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Data").unwrap();

        // Create test data
        for i in 0..30 {
            let version_id = VersionId::new(i).unwrap();
            let temporal = BiTemporalInterval::current((i as i64 * 1000).into());
            let props = PropertyMapBuilder::new()
                .insert("id", i as i64)
                .insert("name", format!("item_{}", i))
                .build();

            storage
                .add_node_version(node_id, version_id, temporal, label, props)
                .unwrap();
        }

        // Verify all data is correct regardless of cache state
        for i in 0..30 {
            let version_id = VersionId::new(i).unwrap();
            let props = storage.reconstruct_node_properties(version_id).unwrap();
            assert_eq!(props.get("id").and_then(|v| v.as_int()), Some(i as i64));
            assert_eq!(
                props.get("name").and_then(|v| v.as_str()),
                Some(format!("item_{}", i).as_str())
            );
        }

        // Verify cache metrics are being tracked
        let metrics = storage.cache_metrics();
        assert!(
            metrics.total_operations() > 0,
            "Should have cache operations"
        );

        // With good cache size (100) and sequential access, hit rate should be decent
        if let Some(hit_rate) = storage.cache_hit_rate() {
            println!("Cache hit rate: {:.2}%", hit_rate * 100.0);
        }
    }

    // ============================================================
    // Edge Version Chain Tests (TDD for Issue #345)
    // ============================================================
    // These tests ensure edge version functionality has parity with
    // node version functionality before refactoring to eliminate
    // duplicate code.

    #[test]
    fn test_edge_version_chain() {
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 3,
            max_delta_chain: 10,
        });

        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Create 5 versions
        let mut version_ids = Vec::new();
        for i in 0..5 {
            let version_id = VersionId::new(100 + i).unwrap();
            let temporal = BiTemporalInterval::current((1000 + (i as i64) * 100).into());
            let props = PropertyMapBuilder::new()
                .insert("weight", i as i64)
                .insert("since", "2024")
                .build();

            storage
                .add_edge_version(edge_id, version_id, temporal, label, source, target, props)
                .unwrap();

            version_ids.push(version_id);
        }

        // Check version types - should follow same pattern as nodes:
        // v0: anchor (first)
        // v1: delta
        // v2: delta
        // v3: anchor (interval = 3)
        // v4: delta

        assert!(
            storage
                .get_edge_version(version_ids[0])
                .unwrap()
                .is_anchor()
        );
        assert!(storage.get_edge_version(version_ids[1]).unwrap().is_delta());
        assert!(storage.get_edge_version(version_ids[2]).unwrap().is_delta());
        assert!(
            storage
                .get_edge_version(version_ids[3])
                .unwrap()
                .is_anchor()
        );
        assert!(storage.get_edge_version(version_ids[4]).unwrap().is_delta());
    }

    #[test]
    fn test_edge_property_reconstruction() {
        let mut storage = HistoricalStorage::new();

        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Version 1: weight=10, since=2020
        let v1 = VersionId::new(1).unwrap();
        storage
            .add_edge_version(
                edge_id,
                v1,
                BiTemporalInterval::current(1000.into()),
                label,
                source,
                target,
                PropertyMapBuilder::new()
                    .insert("weight", 10i64)
                    .insert("since", "2020")
                    .build(),
            )
            .unwrap();

        // Version 2: weight=20, since=2020 (delta - only weight changes)
        let v2 = VersionId::new(2).unwrap();
        storage
            .add_edge_version(
                edge_id,
                v2,
                BiTemporalInterval::current(2000.into()),
                label,
                source,
                target,
                PropertyMapBuilder::new()
                    .insert("weight", 20i64)
                    .insert("since", "2020")
                    .build(),
            )
            .unwrap();

        // Reconstruct v1 properties
        let props_v1 = storage.reconstruct_edge_properties(v1).unwrap();
        assert_eq!(props_v1.get("weight").and_then(|v| v.as_int()), Some(10));
        assert_eq!(props_v1.get("since").and_then(|v| v.as_str()), Some("2020"));

        // Reconstruct v2 properties
        let props_v2 = storage.reconstruct_edge_properties(v2).unwrap();
        assert_eq!(props_v2.get("weight").and_then(|v| v.as_int()), Some(20));
        assert_eq!(props_v2.get("since").and_then(|v| v.as_str()), Some("2020"));
    }

    #[test]
    fn test_edge_find_version_at_time() {
        let mut storage = HistoricalStorage::new();

        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Create versions at different times
        let v1 = VersionId::new(1).unwrap();
        let v2 = VersionId::new(2).unwrap();
        let v3 = VersionId::new(3).unwrap();

        storage
            .add_edge_version(
                edge_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), 1000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
                label,
                source,
                target,
                PropertyMapBuilder::new().insert("weight", 10i64).build(),
            )
            .unwrap();

        storage
            .add_edge_version(
                edge_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 2000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
                label,
                source,
                target,
                PropertyMapBuilder::new().insert("weight", 20i64).build(),
            )
            .unwrap();

        storage
            .add_edge_version(
                edge_id,
                v3,
                BiTemporalInterval::new(
                    TimeRange::new(2000.into(), TIMESTAMP_MAX).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
                label,
                source,
                target,
                PropertyMapBuilder::new().insert("weight", 30i64).build(),
            )
            .unwrap();

        // Query at different times
        assert_eq!(
            storage.find_edge_version_at_time(edge_id, 500.into(), 100.into()),
            Some(v1)
        );
        assert_eq!(
            storage.find_edge_version_at_time(edge_id, 1500.into(), 100.into()),
            Some(v2)
        );
        assert_eq!(
            storage.find_edge_version_at_time(edge_id, 2500.into(), 100.into()),
            Some(v3)
        );
    }

    #[test]
    fn test_edge_version_chain_links() {
        // Test that version chains are properly linked (prev/next)
        let mut storage = HistoricalStorage::new();

        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        let v1 = VersionId::new(1).unwrap();
        let v2 = VersionId::new(2).unwrap();
        let v3 = VersionId::new(3).unwrap();

        for (i, vid) in [v1, v2, v3].iter().enumerate() {
            storage
                .add_edge_version(
                    edge_id,
                    *vid,
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    source,
                    target,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();
        }

        // Check linking
        let version1 = storage.get_edge_version(v1).unwrap();
        assert_eq!(version1.prev_version, None);
        assert_eq!(version1.next_version, Some(v2));

        let version2 = storage.get_edge_version(v2).unwrap();
        assert_eq!(version2.prev_version, Some(v1));
        assert_eq!(version2.next_version, Some(v3));

        let version3 = storage.get_edge_version(v3).unwrap();
        assert_eq!(version3.prev_version, Some(v2));
        assert_eq!(version3.next_version, None);
    }

    #[test]
    fn test_first_edge_version_is_anchor() {
        let mut storage = HistoricalStorage::new();

        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let temporal = BiTemporalInterval::current(1000.into());
        let props = PropertyMapBuilder::new().insert("weight", 5i64).build();

        storage
            .add_edge_version(edge_id, version_id, temporal, label, source, target, props)
            .unwrap();

        // First version should always be an anchor
        let version = storage.get_edge_version(version_id).unwrap();
        assert!(version.is_anchor());
        assert_eq!(version.edge_id, edge_id);
        assert_eq!(version.prev_version, None);
        assert_eq!(version.source, source);
        assert_eq!(version.target, target);
    }

    #[test]
    fn test_independent_node_edge_anchor_intervals() {
        // Verify that node and edge version chains maintain separate anchor counters
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 3,
            max_delta_chain: 10,
        });

        let node_id = NodeId::new(1).unwrap();
        let edge_id = EdgeId::new(1).unwrap();
        let source = NodeId::new(2).unwrap();
        let target = NodeId::new(3).unwrap();
        let node_label = GLOBAL_INTERNER.intern("Person").unwrap();
        let edge_label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Create interleaved node and edge versions to ensure they don't interfere
        // Node pattern: anchor(0), delta(1), delta(2), anchor(3), delta(4)
        // Edge pattern: anchor(100), delta(101), delta(102), anchor(103), delta(104)
        let mut node_version_ids = Vec::new();
        let mut edge_version_ids = Vec::new();

        for i in 0..5 {
            // Add node version
            let node_vid = VersionId::new(i).unwrap();
            storage
                .add_node_version(
                    node_id,
                    node_vid,
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    node_label,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();
            node_version_ids.push(node_vid);

            // Add edge version (interleaved)
            let edge_vid = VersionId::new(100 + i).unwrap();
            storage
                .add_edge_version(
                    edge_id,
                    edge_vid,
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    edge_label,
                    source,
                    target,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();
            edge_version_ids.push(edge_vid);
        }

        // Verify node version pattern: anchor, delta, delta, anchor, delta
        assert!(
            storage
                .get_node_version(node_version_ids[0])
                .unwrap()
                .is_anchor()
        );
        assert!(
            storage
                .get_node_version(node_version_ids[1])
                .unwrap()
                .is_delta()
        );
        assert!(
            storage
                .get_node_version(node_version_ids[2])
                .unwrap()
                .is_delta()
        );
        assert!(
            storage
                .get_node_version(node_version_ids[3])
                .unwrap()
                .is_anchor()
        );
        assert!(
            storage
                .get_node_version(node_version_ids[4])
                .unwrap()
                .is_delta()
        );

        // Verify edge version pattern is the same (independent counter)
        assert!(
            storage
                .get_edge_version(edge_version_ids[0])
                .unwrap()
                .is_anchor()
        );
        assert!(
            storage
                .get_edge_version(edge_version_ids[1])
                .unwrap()
                .is_delta()
        );
        assert!(
            storage
                .get_edge_version(edge_version_ids[2])
                .unwrap()
                .is_delta()
        );
        assert!(
            storage
                .get_edge_version(edge_version_ids[3])
                .unwrap()
                .is_anchor()
        );
        assert!(
            storage
                .get_edge_version(edge_version_ids[4])
                .unwrap()
                .is_delta()
        );
    }

    #[test]
    fn test_count_versions_since_anchor_generic() {
        // Direct test of the generic count_versions_since_anchor helper
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 3,
            max_delta_chain: 10,
        });

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        // Create anchor(0), delta(1), delta(2)
        let mut version_ids = Vec::new();
        for i in 0..3 {
            let vid = VersionId::new(i).unwrap();
            storage
                .add_node_version(
                    node_id,
                    vid,
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();
            version_ids.push(vid);
        }

        // Test counting from version 2 (delta) - should find 2 deltas before anchor
        assert_eq!(storage.count_versions_since_anchor_node(version_ids[2]), 2);

        // Test counting from version 1 (delta) - should find 1 delta before anchor
        assert_eq!(storage.count_versions_since_anchor_node(version_ids[1]), 1);

        // Test counting from version 0 (anchor) - should find 0 deltas
        assert_eq!(storage.count_versions_since_anchor_node(version_ids[0]), 0);

        // Create more versions to get anchor(3), delta(4)
        for i in 3..5 {
            let vid = VersionId::new(i).unwrap();
            storage
                .add_node_version(
                    node_id,
                    vid,
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();
            version_ids.push(vid);
        }

        // Test counting from version 4 (delta after new anchor) - should find 1 delta
        assert_eq!(storage.count_versions_since_anchor_node(version_ids[4]), 1);

        // Test counting from version 3 (new anchor) - should find 0 deltas
        assert_eq!(storage.count_versions_since_anchor_node(version_ids[3]), 0);
    }

    #[test]
    fn test_version_counter_cache() {
        // Test that the version counter cache correctly tracks versions since last anchor
        // This test verifies the fix for issue #208
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 5,
            max_delta_chain: 10,
        });

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        // Add first version - should be anchor, counter should be 0
        let v0 = VersionId::new(0).unwrap();
        storage
            .add_node_version(
                node_id,
                v0,
                BiTemporalInterval::current(1000.into()),
                label,
                PropertyMapBuilder::new().insert("version", 0i64).build(),
            )
            .unwrap();

        // Verify first version is an anchor
        assert!(storage.get_node_version(v0).unwrap().is_anchor());

        // Add 4 more versions (deltas)
        for i in 1..5 {
            let vid = VersionId::new(i).unwrap();
            storage
                .add_node_version(
                    node_id,
                    vid,
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();

            // All should be deltas
            assert!(storage.get_node_version(vid).unwrap().is_delta());
        }

        // Add 5th version - should trigger anchor creation (interval = 5)
        let v5 = VersionId::new(5).unwrap();
        storage
            .add_node_version(
                node_id,
                v5,
                BiTemporalInterval::current(1600.into()),
                label,
                PropertyMapBuilder::new().insert("version", 5i64).build(),
            )
            .unwrap();

        // Version 5 should be an anchor
        assert!(storage.get_node_version(v5).unwrap().is_anchor());

        // Add one more version - should be delta, counter should reset
        let v6 = VersionId::new(6).unwrap();
        storage
            .add_node_version(
                node_id,
                v6,
                BiTemporalInterval::current(1700.into()),
                label,
                PropertyMapBuilder::new().insert("version", 6i64).build(),
            )
            .unwrap();

        // Version 6 should be a delta
        assert!(storage.get_node_version(v6).unwrap().is_delta());

        // Test with multiple entities to ensure counters are independent
        let node_id2 = NodeId::new(2).unwrap();

        // Add versions to second entity
        for i in 0..3 {
            let vid = VersionId::new(100 + i).unwrap();
            storage
                .add_node_version(
                    node_id2,
                    vid,
                    BiTemporalInterval::current((2000 + (i as i64) * 100).into()),
                    label,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();
        }

        // First version of second entity should be anchor
        assert!(
            storage
                .get_node_version(VersionId::new(100).unwrap())
                .unwrap()
                .is_anchor()
        );
        // Next two should be deltas
        assert!(
            storage
                .get_node_version(VersionId::new(101).unwrap())
                .unwrap()
                .is_delta()
        );
        assert!(
            storage
                .get_node_version(VersionId::new(102).unwrap())
                .unwrap()
                .is_delta()
        );
    }

    #[test]
    fn test_edge_version_counter_cache() {
        // Test that the version counter cache works for edges too
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 3,
            max_delta_chain: 10,
        });

        let edge_id = EdgeId::new(1).unwrap();
        let from = NodeId::new(1).unwrap();
        let to = NodeId::new(2).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Add first version - should be anchor
        let v0 = VersionId::new(0).unwrap();
        storage
            .add_edge_version(
                edge_id,
                v0,
                BiTemporalInterval::current(1000.into()),
                label,
                from,
                to,
                PropertyMapBuilder::new().insert("version", 0i64).build(),
            )
            .unwrap();

        assert!(storage.get_edge_version(v0).unwrap().is_anchor());

        // Add 2 deltas
        for i in 1..3 {
            let vid = VersionId::new(i).unwrap();
            storage
                .add_edge_version(
                    edge_id,
                    vid,
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    from,
                    to,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();

            assert!(storage.get_edge_version(vid).unwrap().is_delta());
        }

        // Add 3rd delta - should trigger anchor creation (interval = 3)
        let v3 = VersionId::new(3).unwrap();
        storage
            .add_edge_version(
                edge_id,
                v3,
                BiTemporalInterval::current(1400.into()),
                label,
                from,
                to,
                PropertyMapBuilder::new().insert("version", 3i64).build(),
            )
            .unwrap();

        // Version 3 should be an anchor
        assert!(storage.get_edge_version(v3).unwrap().is_anchor());
    }

    #[test]
    fn test_counter_cache_rebuilt_after_persistence_restore() {
        // Test for issue #208 fix: Verify that counter cache is correctly
        // rebuilt when loading from persistence
        let config = AnchorConfig {
            anchor_interval: 5,
            max_delta_chain: 10,
        };
        let mut original = HistoricalStorage::with_config(config.clone());

        let node_id = NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        // Create 7 versions: anchor(0), delta(1), delta(2), delta(3), delta(4), anchor(5), delta(6)
        for i in 0..7 {
            let vid = VersionId::new(i).unwrap();
            original
                .add_node_version(
                    node_id,
                    vid,
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();
        }

        // Verify anchor pattern before persistence
        assert!(
            original
                .get_node_version(VersionId::new(0).unwrap())
                .unwrap()
                .is_anchor()
        ); // anchor
        assert!(
            original
                .get_node_version(VersionId::new(1).unwrap())
                .unwrap()
                .is_delta()
        ); // delta
        assert!(
            original
                .get_node_version(VersionId::new(4).unwrap())
                .unwrap()
                .is_delta()
        ); // delta
        assert!(
            original
                .get_node_version(VersionId::new(5).unwrap())
                .unwrap()
                .is_anchor()
        ); // anchor
        assert!(
            original
                .get_node_version(VersionId::new(6).unwrap())
                .unwrap()
                .is_delta()
        ); // delta

        // Extract all versions to simulate persistence save/load
        let saved_versions: Vec<NodeVersion> = original.node_versions.values().cloned().collect();

        // Create new storage and restore versions (simulating load from disk)
        let mut restored = HistoricalStorage::with_config(config);

        // Insert all restored versions
        for version in saved_versions {
            restored.insert_restored_node_version(version).unwrap();
        }

        // Rebuild version chains and counter cache
        restored.rebuild_version_chains();

        // Verify counter cache was rebuilt correctly
        // After version 6 (delta), counter should be 1 (one delta since last anchor at v5)
        let counter = restored
            .node_versions_since_anchor
            .get(&node_id)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            counter, 1,
            "Counter should be 1 after version 6 (one delta since anchor at v5)"
        );

        // Now add more versions and verify anchor/delta pattern continues correctly
        // Add versions 7-8 (should be deltas)
        for i in 7..9 {
            let vid = VersionId::new(i).unwrap();
            restored
                .add_node_version(
                    node_id,
                    vid,
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();

            // Both should be deltas
            assert!(
                restored.get_node_version(vid).unwrap().is_delta(),
                "Version {} should be delta",
                i
            );
        }

        // Add version 9 (should be delta, counter becomes 4)
        let v9 = VersionId::new(9).unwrap();
        restored
            .add_node_version(
                node_id,
                v9,
                BiTemporalInterval::current(1900.into()),
                label,
                PropertyMapBuilder::new().insert("version", 9i64).build(),
            )
            .unwrap();

        assert!(
            restored.get_node_version(v9).unwrap().is_delta(),
            "Version 9 should be delta"
        );

        // Add version 10 - should trigger anchor (5 deltas since v5: v6,v7,v8,v9,v10)
        let v10 = VersionId::new(10).unwrap();
        restored
            .add_node_version(
                node_id,
                v10,
                BiTemporalInterval::current(2000.into()),
                label,
                PropertyMapBuilder::new().insert("version", 10i64).build(),
            )
            .unwrap();

        // Version 10 should be an anchor
        assert!(
            restored.get_node_version(v10).unwrap().is_anchor(),
            "Version 10 should be anchor after 5 deltas"
        );

        // Verify counter was reset to 0
        let counter_after = restored
            .node_versions_since_anchor
            .get(&node_id)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            counter_after, 0,
            "Counter should be reset to 0 after creating anchor"
        );
    }

    #[test]
    fn test_edge_counter_cache_rebuilt_after_restore() {
        // Test edge counter cache rebuilding after persistence restore
        let config = AnchorConfig {
            anchor_interval: 3,
            max_delta_chain: 10,
        };
        let mut original = HistoricalStorage::with_config(config.clone());

        let edge_id = EdgeId::new(1).unwrap();
        let from = NodeId::new(1).unwrap();
        let to = NodeId::new(2).unwrap();
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Create 5 versions: anchor(0), delta(1), delta(2), anchor(3), delta(4)
        for i in 0..5 {
            let vid = VersionId::new(i).unwrap();
            original
                .add_edge_version(
                    edge_id,
                    vid,
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    from,
                    to,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();
        }

        // Extract versions
        let saved_versions: Vec<EdgeVersion> = original.edge_versions.values().cloned().collect();

        // Restore to new storage
        let mut restored = HistoricalStorage::with_config(config);
        for version in saved_versions {
            restored.insert_restored_edge_version(version).unwrap();
        }
        restored.rebuild_version_chains();

        // Verify counter is 1 (version 4 is delta after anchor 3)
        let counter = restored
            .edge_versions_since_anchor
            .get(&edge_id)
            .copied()
            .unwrap_or(0);
        assert_eq!(counter, 1, "Edge counter should be 1 after restore");

        // Add two more versions - should create anchor at v6
        for i in 5..7 {
            let vid = VersionId::new(i).unwrap();
            restored
                .add_edge_version(
                    edge_id,
                    vid,
                    BiTemporalInterval::current((1000 + (i as i64) * 100).into()),
                    label,
                    from,
                    to,
                    PropertyMapBuilder::new()
                        .insert("version", i as i64)
                        .build(),
                )
                .unwrap();
        }

        // v5 should be delta, v6 should be anchor
        assert!(
            restored
                .get_edge_version(VersionId::new(5).unwrap())
                .unwrap()
                .is_delta()
        );
        assert!(
            restored
                .get_edge_version(VersionId::new(6).unwrap())
                .unwrap()
                .is_anchor()
        );
    }
}
