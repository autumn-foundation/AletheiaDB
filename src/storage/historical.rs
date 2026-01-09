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
use crate::core::temporal::{BiTemporalInterval, Timestamp};
use crate::storage::observer::{Observer, StorageEvent, notify_observers};
use crate::storage::version::{
    AnchorConfig, EdgeVersion, NodeVersion, TemporalVersion, VersionData,
};
use crate::utils::error::{Result, StorageError, TemporalError};
use quick_cache::sync::Cache;
use std::collections::HashMap;
use std::sync::Arc;

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

/// Default cache size for reconstructed properties (10,000 entries)
const DEFAULT_RECONSTRUCTION_CACHE_SIZE: usize = 10_000;

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
    /// TinyLFU cache for reconstructed node properties (reduces lock contention)
    node_property_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
    /// TinyLFU cache for reconstructed edge properties
    edge_property_cache: Arc<Cache<VersionId, Arc<PropertyMap>>>,
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
        HistoricalStorage {
            config,
            retention_policy,
            node_versions: HashMap::new(),
            edge_versions: HashMap::new(),
            node_version_heads: HashMap::new(),
            edge_version_heads: HashMap::new(),
            node_version_counts: HashMap::new(),
            edge_version_counts: HashMap::new(),
            node_property_cache: Arc::new(Cache::new(cache_size)),
            edge_property_cache: Arc::new(Cache::new(cache_size)),
            observers: Vec::new(),
            pre_node_anchor_hook: None,
            pre_edge_anchor_hook: None,
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

        // Clone properties for hook call (since new_anchor takes ownership)
        let properties_for_hook = properties.clone();

        let mut version = if let Some(prev_id) = prev_version_id {
            // Get the previous version (verify it exists)
            let _prev_version = self
                .node_versions
                .get(&prev_id)
                .ok_or(StorageError::VersionNotFound(prev_id))?;

            // Count versions since last anchor (including this new version)
            let versions_since_anchor = self.count_versions_since_anchor_node(prev_id) + 1;

            // Decide whether to create anchor or delta
            if versions_since_anchor >= self.config.anchor_interval as usize {
                // Create anchor
                NodeVersion::new_anchor(version_id, node_id, temporal, label, properties)
            } else {
                // Create delta from previous version
                let old_properties = self.reconstruct_node_properties(prev_id)?;
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
            NodeVersion::new_anchor(version_id, node_id, temporal, label, properties)
        };

        // Call pre-anchor hook if this is an anchor (BEFORE storing)
        if version.is_anchor() {
            let timestamp = temporal.transaction_time().start();
            if let Some(ref hook) = self.pre_node_anchor_hook {
                match hook("node", node_id.as_u64(), timestamp, &properties_for_hook) {
                    Ok(Some(snapshot_id)) => {
                        // Set snapshot ID in anchor data (strong consistency)
                        version.data.set_vector_snapshot_id(snapshot_id);
                        #[cfg(feature = "observability")]
                        tracing::debug!(
                            "Pre-anchor hook returned snapshot ID {} for node {}",
                            snapshot_id,
                            node_id
                        );
                    }
                    Ok(None) => {
                        // Hook returned None, no snapshot needed (e.g., empty index)
                        #[cfg(feature = "observability")]
                        tracing::debug!(
                            "Pre-anchor hook returned None for node {} (no snapshot needed)",
                            node_id
                        );
                    }
                    Err(_e) => {
                        // Hook failed - log but don't block anchor creation (graceful degradation)
                        #[cfg(feature = "observability")]
                        tracing::warn!(
                            "Pre-anchor hook failed for {} {} at timestamp {}: {} (anchor will still be created)",
                            "node",
                            node_id,
                            timestamp,
                            _e
                        );
                    }
                }
            }
        }

        // Link the previous version to this one
        if let Some(prev_id) = prev_version_id
            && let Some(prev) = self.node_versions.get_mut(&prev_id)
        {
            prev.next_version = Some(version_id);
        }

        // Check if this is an anchor before storing (for observer notification)
        let is_anchor = version.is_anchor();

        // Store the version
        self.node_versions.insert(version_id, version);
        self.node_version_heads.insert(node_id, version_id);

        // Increment cached version count (for O(1) capacity checks)
        *self.node_version_counts.entry(node_id).or_insert(0) += 1;

        // Notify observers - emit appropriate events
        let timestamp = temporal.transaction_time().start();

        // Emit version created event (for all versions)
        let version_event = StorageEvent::NodeVersionCreated {
            version_id,
            node_id,
            timestamp,
            is_anchor,
        };
        notify_observers(&self.observers, &version_event);

        // Emit anchor created event (only for anchors)
        if is_anchor {
            let anchor_event = StorageEvent::NodeAnchorCreated {
                version_id,
                node_id,
                timestamp,
            };
            notify_observers(&self.observers, &anchor_event);
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

        let prev_version_id = self.edge_version_heads.get(&edge_id).copied();

        // Clone properties for hook call (since new_anchor takes ownership)
        let properties_for_hook = properties.clone();

        let mut version = if let Some(prev_id) = prev_version_id {
            let _prev_version = self
                .edge_versions
                .get(&prev_id)
                .ok_or(StorageError::VersionNotFound(prev_id))?;

            let versions_since_anchor = self.count_versions_since_anchor_edge(prev_id) + 1;

            if versions_since_anchor >= self.config.anchor_interval as usize {
                EdgeVersion::new_anchor(
                    version_id, edge_id, temporal, label, source, target, properties,
                )
            } else {
                let old_properties = self.reconstruct_edge_properties(prev_id)?;
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
            EdgeVersion::new_anchor(
                version_id, edge_id, temporal, label, source, target, properties,
            )
        };

        // Call pre-anchor hook if this is an anchor (BEFORE storing)
        if version.is_anchor() {
            let timestamp = temporal.transaction_time().start();
            if let Some(ref hook) = self.pre_edge_anchor_hook {
                match hook("edge", edge_id.as_u64(), timestamp, &properties_for_hook) {
                    Ok(Some(snapshot_id)) => {
                        // Set snapshot ID in anchor data (strong consistency)
                        version.data.set_vector_snapshot_id(snapshot_id);
                        #[cfg(feature = "observability")]
                        tracing::debug!(
                            "Pre-anchor hook returned snapshot ID {} for edge {}",
                            snapshot_id,
                            edge_id
                        );
                    }
                    Ok(None) => {
                        // Hook returned None, no snapshot needed (e.g., empty index)
                        #[cfg(feature = "observability")]
                        tracing::debug!(
                            "Pre-anchor hook returned None for edge {} (no snapshot needed)",
                            edge_id
                        );
                    }
                    Err(_e) => {
                        // Hook failed - log but don't block anchor creation (graceful degradation)
                        #[cfg(feature = "observability")]
                        tracing::warn!(
                            "Pre-anchor hook failed for {} {} at timestamp {}: {} (anchor will still be created)",
                            "edge",
                            edge_id,
                            timestamp,
                            _e
                        );
                    }
                }
            }
        }

        if let Some(prev_id) = prev_version_id
            && let Some(prev) = self.edge_versions.get_mut(&prev_id)
        {
            prev.next_version = Some(version_id);
        }

        // Check if this is an anchor before storing (for observer notification)
        let is_anchor = version.is_anchor();

        self.edge_versions.insert(version_id, version);
        self.edge_version_heads.insert(edge_id, version_id);

        // Increment cached version count (for O(1) capacity checks)
        *self.edge_version_counts.entry(edge_id).or_insert(0) += 1;

        // Notify observers - emit appropriate events
        let timestamp = temporal.transaction_time().start();

        // Emit version created event (for all versions)
        let version_event = StorageEvent::EdgeVersionCreated {
            version_id,
            edge_id,
            timestamp,
            is_anchor,
        };
        notify_observers(&self.observers, &version_event);

        // Emit anchor created event (only for anchors)
        if is_anchor {
            let anchor_event = StorageEvent::EdgeAnchorCreated {
                version_id,
                edge_id,
                timestamp,
            };
            notify_observers(&self.observers, &anchor_event);
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
        if depth >= MAX_RECONSTRUCTION_DEPTH {
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

        // Check cache first (fast path for concurrent reads)
        if let Some(cached) = self.node_property_cache.get(&version_id) {
            return Ok(cached.as_ref().clone());
        }

        // Cache miss - reconstruct properties
        let version = self
            .node_versions
            .get(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        let properties = match &version.data {
            VersionData::Anchor { properties, .. } => properties.clone(),
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
        if depth >= MAX_RECONSTRUCTION_DEPTH {
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

        // Check cache first (fast path for concurrent reads)
        if let Some(cached) = self.edge_property_cache.get(&version_id) {
            return Ok(cached.as_ref().clone());
        }

        // Cache miss - reconstruct properties
        let version = self
            .edge_versions
            .get(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        let properties = match &version.data {
            VersionData::Anchor { properties, .. } => properties.clone(),
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

    /// Count how many versions exist since the last anchor (for a node).
    fn count_versions_since_anchor_node(&self, version_id: VersionId) -> usize {
        let mut count = 0;
        let mut current_id = version_id;

        loop {
            if let Some(version) = self.node_versions.get(&current_id) {
                if version.is_anchor() {
                    return count;
                }
                count += 1;

                // Move to previous version
                if let Some(prev_id) = version.prev_version {
                    current_id = prev_id;
                } else {
                    // Reached the beginning without finding an anchor
                    return count;
                }
            } else {
                return count;
            }
        }
    }

    /// Count how many versions exist since the last anchor (for an edge).
    fn count_versions_since_anchor_edge(&self, version_id: VersionId) -> usize {
        let mut count = 0;
        let mut current_id = version_id;

        loop {
            if let Some(version) = self.edge_versions.get(&current_id) {
                if version.is_anchor() {
                    return count;
                }
                count += 1;

                if let Some(prev_id) = version.prev_version {
                    current_id = prev_id;
                } else {
                    return count;
                }
            } else {
                return count;
            }
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
            node_cache_entries: self.node_property_cache.len(),
            edge_cache_entries: self.edge_property_cache.len(),
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
}

impl Default for HistoricalStorage {
    fn default() -> Self {
        Self::new()
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
    /// Number of cached node property reconstructions
    pub node_cache_entries: usize,
    /// Number of cached edge property reconstructions
    pub edge_cache_entries: usize,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::TimeRange;
    use crate::storage::{StorageEvent, StorageObserver};

    #[test]
    fn test_create_first_version() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(100).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let temporal = BiTemporalInterval::current(1000);
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
            let temporal = BiTemporalInterval::current(1000 + (i as i64) * 100);
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
                BiTemporalInterval::current(1000),
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
                BiTemporalInterval::current(2000),
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
        assert_eq!(props.get("age").and_then(|v| v.as_int()), Some(31));
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
                BiTemporalInterval::new(TimeRange::new(0, 1000), TimeRange::new(0, Timestamp::MAX)),
                label,
                PropertyMapBuilder::new().insert("age", 30i64).build(),
            )
            .unwrap();

        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(1000, 2000),
                    TimeRange::new(0, Timestamp::MAX),
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
                    TimeRange::new(2000, Timestamp::MAX),
                    TimeRange::new(0, Timestamp::MAX),
                ),
                label,
                PropertyMapBuilder::new().insert("age", 32i64).build(),
            )
            .unwrap();

        // Query at different times
        assert_eq!(
            storage.find_node_version_at_time(node_id, 500, 100),
            Some(v1)
        );
        assert_eq!(
            storage.find_node_version_at_time(node_id, 1500, 100),
            Some(v2)
        );
        assert_eq!(
            storage.find_node_version_at_time(node_id, 2500, 100),
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
                    BiTemporalInterval::current(1000 + (i as i64) * 100),
                    label,
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
        }

        // Try to add 4th version - should fail
        let result = storage.add_node_version(
            node_id,
            VersionId::new(3).unwrap(),
            BiTemporalInterval::current(1300),
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
                    BiTemporalInterval::current(1000 + (i as i64) * 100),
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
            BiTemporalInterval::current(1200),
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
                    BiTemporalInterval::current(1000 + (i as i64) * 100),
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
        let temporal = BiTemporalInterval::current(1000);

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
                BiTemporalInterval::current(1000),
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
                BiTemporalInterval::current(2000),
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
                BiTemporalInterval::current(1000),
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
                BiTemporalInterval::current(2000),
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
                BiTemporalInterval::current(1000),
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
                BiTemporalInterval::current(2000),
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
                    BiTemporalInterval::current(1000 + (i as i64) * 100),
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
        let temporal = BiTemporalInterval::current(1000);
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
                BiTemporalInterval::current(1000),
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
                BiTemporalInterval::current(2000),
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
                BiTemporalInterval::current(1000),
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
            (0, 500, vec![0.1f32, 0.0]),               // valid 0-500
            (500, 1000, vec![0.2f32, 0.0]),            // valid 500-1000
            (1000, Timestamp::MAX, vec![0.3f32, 0.0]), // valid 1000+
        ];

        for (i, (start, end, emb)) in embeddings.iter().enumerate() {
            storage
                .add_node_version(
                    node_id,
                    VersionId::new(i as u64).unwrap(),
                    BiTemporalInterval::new(
                        TimeRange::new(*start, *end),
                        TimeRange::new(0, Timestamp::MAX),
                    ),
                    label,
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", emb)
                        .build(),
                )
                .unwrap();
        }

        // Query at different times
        let v_at_250 = storage.find_node_version_at_time(node_id, 250, 0);
        let v_at_750 = storage.find_node_version_at_time(node_id, 750, 0);
        let v_at_1500 = storage.find_node_version_at_time(node_id, 1500, 0);

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
                BiTemporalInterval::current(1000),
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
                BiTemporalInterval::current(2000),
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
                BiTemporalInterval::current(1000),
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
                BiTemporalInterval::current(1000),
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
                BiTemporalInterval::current(2000),
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
        let temporal = BiTemporalInterval::current(1000);
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
                BiTemporalInterval::current(1000),
                label,
                PropertyMapBuilder::new().insert("value", 1i64).build(),
            )
            .unwrap();

        let v2 = VersionId::new(2).unwrap();
        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::current(2000),
                label,
                PropertyMapBuilder::new().insert("value", 2i64).build(),
            )
            .unwrap();

        let v3 = VersionId::new(3).unwrap();
        storage
            .add_node_version(
                node_id,
                v3,
                BiTemporalInterval::current(3000),
                label,
                PropertyMapBuilder::new().insert("value", 3i64).build(),
            )
            .unwrap();

        let v4 = VersionId::new(4).unwrap();
        storage
            .add_node_version(
                node_id,
                v4,
                BiTemporalInterval::current(4000),
                label,
                PropertyMapBuilder::new().insert("value", 4i64).build(),
            )
            .unwrap();

        // Reconstruct v4 (latest delta) - should populate entire chain
        let result = storage.reconstruct_node_properties(v4).unwrap();
        assert_eq!(result.get("value").and_then(|v| v.as_int()), Some(4));

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
        let temporal = BiTemporalInterval::current(1000);
        let props = PropertyMapBuilder::new().insert("since", 2020i64).build();

        storage
            .add_edge_version(edge_id, version_id, temporal, label, source, target, props)
            .unwrap();

        // First read - cache miss
        let result1 = storage.reconstruct_edge_properties(version_id).unwrap();
        assert_eq!(result1.get("since").and_then(|v| v.as_int()), Some(2020));

        // Check cache was populated
        let stats = storage.stats();
        assert_eq!(stats.edge_cache_entries, 1);

        // Second read - should hit cache
        let result2 = storage.reconstruct_edge_properties(version_id).unwrap();
        assert_eq!(result2.get("since").and_then(|v| v.as_int()), Some(2020));

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
        let temporal = BiTemporalInterval::current(1000);

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
                BiTemporalInterval::current(1000),
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
        let temporal = BiTemporalInterval::current(1000);
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
        let temporal = BiTemporalInterval::current(1000);
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
                assert_eq!(properties.get("since").and_then(|v| v.as_int()), Some(2021));
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
                    BiTemporalInterval::current(1000 + (i as i64) * 100),
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
                    BiTemporalInterval::current(1000 + (i as i64) * 100),
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
                BiTemporalInterval::current(1000),
                label,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        // Create edge anchor
        storage
            .add_edge_version(
                edge_id,
                VersionId::new(2).unwrap(),
                BiTemporalInterval::current(2000),
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
                BiTemporalInterval::current(timestamp),
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
                assert_eq!(*ts, timestamp);
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
                BiTemporalInterval::current(1000),
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
            BiTemporalInterval::current(1000),
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
                BiTemporalInterval::current(1000),
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
                BiTemporalInterval::current(1000),
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
                BiTemporalInterval::current(1000),
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
            BiTemporalInterval::current(1000),
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
                    BiTemporalInterval::current(1000 + (i as i64) * 100),
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
                BiTemporalInterval::current(1000),
                label,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        // Create edge version
        storage
            .add_edge_version(
                edge_id,
                VersionId::new(2).unwrap(),
                BiTemporalInterval::current(2000),
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
                BiTemporalInterval::current(0),
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
                    BiTemporalInterval::current(i as i64 * 1000),
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
                BiTemporalInterval::current(0),
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
                    BiTemporalInterval::current(i as i64 * 1000),
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
                BiTemporalInterval::current(0),
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
                    BiTemporalInterval::current(i as i64 * 1000),
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
                BiTemporalInterval::current(0),
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
                    BiTemporalInterval::current(i as i64 * 1000),
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
}
