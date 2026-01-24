//! Version management for temporal storage.
//!
//! This module implements the version chain structures that enable time-traveling
//! queries. Each node and edge can have multiple versions over time, linked together
//! in a chain ordered by transaction time.
//!
//! The anchor+delta compression strategy is used to minimize storage overhead:
//! - Anchors: Full snapshots of state (created periodically)
//! - Deltas: Only the changed properties since the previous version

use crate::api::transaction::types::TxId;
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::{MAX_VECTOR_DIMENSIONS, PropertyKey, PropertyMap, PropertyValue};
use crate::core::temporal::{BiTemporalInterval, Timestamp};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Epsilon for floating-point comparisons in vector deltas.
///
/// Used to determine if two f32 values are effectively equal, accounting for
/// floating-point precision limitations. Value chosen to be robust for typical
/// embedding use cases while avoiding spurious deltas.
const VECTOR_EPSILON: f32 = 1e-7;

/// Sparse representation of vector changes.
///
/// Stores only the changed elements to minimize storage overhead when
/// a small percentage of vector elements are modified. This is particularly
/// effective for embeddings where individual element updates are rare.
///
/// # Storage Overhead
///
/// - **Sparse**: `num_changes * (4 bytes index + 4 bytes value)` = 8 bytes per change
/// - **Full**: `dimensions * 4 bytes`
///
/// For a 1536-dimensional vector with 1 element changed:
/// - Sparse: 8 bytes
/// - Full: 6144 bytes
/// - Savings: 768x
///
/// # Threshold Strategy
///
/// Uses sparse storage when `num_changes < dimensions * 0.5` (50% threshold).
/// For heavily modified vectors (>50% changed), falls back to full storage.
///
/// # Equality
///
/// Custom `PartialEq` implementation uses epsilon-based comparison for float values
/// to maintain consistency with `from_diff()`. Two VectorDelta instances are considered
/// equal if their structures match and all float values are within `VECTOR_EPSILON`.
#[derive(Debug, Clone)]
pub enum VectorDelta {
    /// Sparse storage: (index, new_value) pairs for changed elements
    Sparse {
        /// Original vector dimension
        dimension: usize,
        /// Changed indices and their new values
        changes: Arc<Vec<(u32, f32)>>,
    },
    /// Full storage: complete new vector (used when >50% of elements changed)
    Full(Arc<[f32]>),
}

impl VectorDelta {
    /// Compute a delta between two vectors.
    ///
    /// Returns `None` if vectors are identical or have different dimensions.
    /// Uses sparse storage when storing individual changes is more efficient
    /// than storing the full vector (changes.len() * 2 < dimension).
    ///
    /// # Errors
    ///
    /// Returns `None` if:
    /// - Vectors have different dimensions
    /// - Vectors are identical (no changes)
    /// - Vector dimension exceeds MAX_VECTOR_DIMENSIONS
    pub fn from_diff(old: &[f32], new: &[f32]) -> Option<Self> {
        if old.len() != new.len() {
            return None;
        }

        // Validate dimension doesn't exceed maximum to prevent DoS
        if old.len() > MAX_VECTOR_DIMENSIONS {
            return None;
        }

        // Collect changed indices with epsilon-based comparison
        let mut changes = Vec::new();
        for (idx, (old_val, new_val)) in old.iter().zip(new.iter()).enumerate() {
            // Use epsilon-based comparison to avoid spurious deltas from floating-point precision
            if (old_val - new_val).abs() > VECTOR_EPSILON {
                // Validate index fits in u32 (should always pass given MAX_VECTOR_DIMENSIONS check)
                let idx_u32 = u32::try_from(idx).ok()?;
                changes.push((idx_u32, *new_val));
            }
        }

        if changes.is_empty() {
            return None;
        }

        let dimension = old.len();

        // Use sparse storage if it's more efficient than full storage
        // Sparse cost: changes * 8 bytes (u32 + f32)
        // Full cost: dimension * 4 bytes (f32)
        // Sparse is better when: changes * 8 < dimension * 4
        // Simplifies to: changes * 2 < dimension
        if changes.len() * 2 < dimension {
            // Use sparse storage
            Some(VectorDelta::Sparse {
                dimension,
                changes: Arc::new(changes),
            })
        } else {
            // Use full storage (sparse wouldn't save space)
            Some(VectorDelta::Full(Arc::from(new)))
        }
    }

    /// Apply this delta to a base vector, producing the new vector.
    ///
    /// # Panics (Debug Only)
    ///
    /// In debug builds, panics if the base vector dimension doesn't match the
    /// expected dimension. In release builds, returns base unchanged on mismatch.
    pub fn apply(&self, base: &[f32]) -> Vec<f32> {
        match self {
            VectorDelta::Sparse { dimension, changes } => {
                if base.len() != *dimension {
                    // Dimension mismatch - this should never happen in correct usage
                    debug_assert!(
                        base.len() == *dimension,
                        "VectorDelta applied to vector of wrong dimension. Expected: {}, Got: {}",
                        *dimension,
                        base.len()
                    );
                    // In release builds, return base unchanged to avoid corruption
                    return base.to_vec();
                }

                let mut result = base.to_vec();
                for &(idx, value) in changes.iter() {
                    if (idx as usize) < result.len() {
                        result[idx as usize] = value;
                    }
                }
                result
            }
            VectorDelta::Full(new_vec) => new_vec.to_vec(),
        }
    }

    /// Estimate the heap memory usage of this delta in bytes.
    ///
    /// # Note on Arc Overhead
    ///
    /// This estimate includes only the data storage, not the Arc allocation overhead.
    /// Arc adds approximately ~24 bytes of overhead per allocation (reference count + weak count + metadata).
    ///
    /// **Actual Storage Overhead:**
    /// - **Sparse**: `Arc overhead (~24 bytes) + num_changes * 8 bytes (index + value)`
    /// - **Full**: `Arc overhead (~24 bytes) + dimensions * 4 bytes`
    ///
    /// For small vectors or few changes, the Arc overhead becomes significant relative to the data size.
    pub fn estimated_heap_size(&self) -> usize {
        match self {
            VectorDelta::Sparse { changes, .. } => {
                // Vec capacity * (u32 + f32) size
                changes.capacity() * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>())
            }
            VectorDelta::Full(vec) => vec.len() * std::mem::size_of::<f32>(),
        }
    }
}

/// Custom PartialEq implementation for VectorDelta using epsilon-based float comparison.
///
/// This maintains consistency with `from_diff()` which uses `VECTOR_EPSILON` to determine
/// if two float values are effectively equal.
impl PartialEq for VectorDelta {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                VectorDelta::Sparse {
                    dimension: dim1,
                    changes: changes1,
                },
                VectorDelta::Sparse {
                    dimension: dim2,
                    changes: changes2,
                },
            ) => {
                // Dimensions must match
                if dim1 != dim2 {
                    return false;
                }

                // Must have same number of changes
                if changes1.len() != changes2.len() {
                    return false;
                }

                // Compare each (index, value) pair with epsilon for floats
                for ((idx1, val1), (idx2, val2)) in changes1.iter().zip(changes2.iter()) {
                    if idx1 != idx2 || (val1 - val2).abs() > VECTOR_EPSILON {
                        return false;
                    }
                }

                true
            }
            (VectorDelta::Full(vec1), VectorDelta::Full(vec2)) => {
                // Must have same length
                if vec1.len() != vec2.len() {
                    return false;
                }

                // Compare each element with epsilon
                for (v1, v2) in vec1.iter().zip(vec2.iter()) {
                    if (v1 - v2).abs() > VECTOR_EPSILON {
                        return false;
                    }
                }

                true
            }
            _ => false, // Different variants are never equal
        }
    }
}

/// Trait for version types that have a bi-temporal interval.
///
/// This trait provides a common interface for accessing and modifying the
/// temporal interval of node and edge versions, reducing code duplication
/// in operations that need to modify temporal properties.
pub trait TemporalVersion {
    /// Get a reference to the version's bi-temporal interval.
    fn temporal(&self) -> &BiTemporalInterval;

    /// Get a mutable reference to the version's bi-temporal interval.
    fn temporal_mut(&mut self) -> &mut BiTemporalInterval;

    /// Close the transaction time of this version.
    ///
    /// This marks the version as no longer being the "current knowledge" after
    /// the specified timestamp. Used when a version is superseded or deleted.
    fn close_transaction_time(&mut self, end_timestamp: Timestamp) {
        let temporal = self.temporal_mut();
        *temporal = temporal.close_transaction_time(end_timestamp);
    }
}

/// Metadata about version creation for Snapshot Isolation.
///
/// This tracks which transaction created a version and when it was committed,
/// enabling proper visibility checking for Snapshot Isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionMetadata {
    /// Transaction that created this version
    pub created_by_tx: TxId,

    /// When this version was committed (None if uncommitted)
    pub commit_timestamp: Option<Timestamp>,
}

impl VersionMetadata {
    /// Create new version metadata for a committed version.
    pub fn new(created_by_tx: TxId, commit_timestamp: Timestamp) -> Self {
        VersionMetadata {
            created_by_tx,
            commit_timestamp: Some(commit_timestamp),
        }
    }

    /// Create metadata for an uncommitted version.
    pub fn uncommitted(created_by_tx: TxId) -> Self {
        VersionMetadata {
            created_by_tx,
            commit_timestamp: None,
        }
    }

    /// Create default metadata for existing data (migration helper).
    pub fn default_for_existing() -> Self {
        use crate::core::hlc::HybridTimestamp;
        VersionMetadata {
            created_by_tx: TxId::new(0),
            // Phase 2: Use HybridTimestamp instead of integer literal
            commit_timestamp: Some(HybridTimestamp::new_unchecked(0, 0)),
        }
    }
}

impl Default for VersionMetadata {
    fn default() -> Self {
        Self::default_for_existing()
    }
}

/// Configuration for anchor creation strategy.
#[derive(Debug, Clone)]
pub struct AnchorConfig {
    /// Create an anchor every N versions (default: 10)
    pub anchor_interval: u32,
    /// Maximum delta chain length before forcing an anchor
    pub max_delta_chain: u32,
}

impl Default for AnchorConfig {
    fn default() -> Self {
        AnchorConfig {
            anchor_interval: 10,
            max_delta_chain: 20,
        }
    }
}

/// Delta representing changes to properties.
///
/// This stores only the changes from the previous version, enabling
/// efficient storage of temporal data. For vector properties, uses
/// sparse delta compression when beneficial (Issue #215).
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyDelta {
    /// Properties that were added or modified (non-vector)
    pub changed: HashMap<PropertyKey, PropertyValue>,
    /// Vector properties with sparse delta optimization
    pub vector_deltas: HashMap<PropertyKey, VectorDelta>,
    /// Properties that were removed
    pub removed: HashSet<PropertyKey>,
}

impl PropertyDelta {
    /// Create a new empty delta.
    pub fn new() -> Self {
        PropertyDelta {
            changed: HashMap::new(),
            vector_deltas: HashMap::new(),
            removed: HashSet::new(),
        }
    }

    /// Create a delta by comparing two property maps.
    ///
    /// Returns the changes needed to transform `old` into `new`.
    /// Uses sparse delta compression for vector properties (Issue #215).
    pub fn from_diff(old: &PropertyMap, new: &PropertyMap) -> Self {
        let mut delta = PropertyDelta::new();

        // Find added and modified properties
        for (key, new_value) in new.iter() {
            match old.get_by_interned_key(key) {
                Some(old_value) if old_value == new_value => {
                    // Unchanged, skip
                }
                Some(old_value) => {
                    // Modified - check if both are vectors for sparse delta optimization
                    match (old_value.as_vector(), new_value.as_vector()) {
                        (Some(old_vec), Some(new_vec)) => {
                            // Both are vectors - use sparse delta if beneficial
                            if let Some(vec_delta) = VectorDelta::from_diff(old_vec, new_vec) {
                                delta.vector_deltas.insert(*key, vec_delta);
                            }
                            // If from_diff returns None, vectors are identical (already handled above)
                        }
                        _ => {
                            // Not both vectors, or value added/type changed - store full value
                            delta.changed.insert(*key, new_value.clone());
                        }
                    }
                }
                None => {
                    // Property added - store full value
                    delta.changed.insert(*key, new_value.clone());
                }
            }
        }

        // Find removed properties
        for key in old.keys() {
            if !new.contains_interned_key(key) {
                delta.removed.insert(*key);
            }
        }

        delta
    }

    /// Apply this delta to a property map, producing a new map.
    pub fn apply(&self, base: &PropertyMap) -> PropertyMap {
        // Clone the base map using the builder to get a mutable version
        let mut builder = base.clone().builder();

        // Apply regular changes
        for (key, value) in &self.changed {
            builder = builder.insert_by_key(*key, value.clone());
        }

        // Apply vector deltas
        for (key, vec_delta) in &self.vector_deltas {
            if let Some(base_value) = base.get_by_interned_key(key)
                && let Some(base_vec) = base_value.as_vector()
            {
                let new_vec = vec_delta.apply(base_vec);
                builder = builder.insert_by_key(*key, PropertyValue::vector(&new_vec));
            }
        }

        // Apply removals
        for key in &self.removed {
            builder = builder.remove_by_key(key);
        }

        builder.build()
    }

    /// Returns true if this delta has no changes.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.vector_deltas.is_empty() && self.removed.is_empty()
    }

    /// Materialize sparse vector deltas into full vectors for persistence.
    ///
    /// This converts all `VectorDelta::Sparse` entries into full `PropertyValue::Vector`
    /// instances by applying them to the base properties. This is required before
    /// persistence since sparse deltas cannot be persisted without their base vectors.
    ///
    /// # Arguments
    ///
    /// * `base` - The base properties (typically from the anchor version)
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` if all sparse deltas were successfully materialized.
    /// Returns `Err` if any sparse delta cannot be materialized (e.g., base property missing).
    ///
    /// # Side Effects
    ///
    /// Moves materialized vectors from `vector_deltas` to `changed`, converting them
    /// to full `PropertyValue::Vector` instances. After this operation, `vector_deltas`
    /// will only contain `VectorDelta::Full` entries (which are then moved to `changed`).
    pub fn materialize_vector_deltas(&mut self, base: &PropertyMap) -> Result<(), String> {
        // Materialize ALL vector deltas (both Sparse and Full) into regular changed properties
        // This is necessary for persistence since the persistence format doesn't support VectorDelta
        let keys: Vec<_> = self.vector_deltas.keys().copied().collect();

        for key in keys {
            if let Some(vec_delta) = self.vector_deltas.remove(&key) {
                match vec_delta {
                    VectorDelta::Full(vec) => {
                        // Full delta can be directly converted
                        self.changed.insert(key, PropertyValue::Vector(vec));
                    }
                    VectorDelta::Sparse { .. } => {
                        // Sparse delta requires base vector to materialize
                        let base_value = base.get_by_interned_key(&key).ok_or_else(|| {
                            format!(
                                "Cannot materialize sparse vector delta: base property not found for key {:?}",
                                key
                            )
                        })?;

                        let base_vec = base_value.as_vector().ok_or_else(|| {
                            format!(
                                "Cannot materialize sparse vector delta: base property is not a vector for key {:?}",
                                key
                            )
                        })?;

                        // Apply sparse delta to get full vector
                        let new_vec = vec_delta.apply(base_vec);
                        self.changed.insert(key, PropertyValue::vector(&new_vec));
                    }
                }
            }
        }

        Ok(())
    }

    /// Estimate the heap memory usage of this delta in bytes.
    ///
    /// This provides a rough estimate including:
    /// - HashMap/HashSet internal storage
    /// - PropertyKey interned strings (counted as pointer size since shared)
    /// - PropertyValue heap allocations (strings, vectors, etc.)
    /// - VectorDelta heap allocations (sparse or full storage)
    pub fn estimated_heap_size(&self) -> usize {
        let mut size = 0;

        // HashMap overhead for changed properties
        size += self.changed.capacity()
            * (std::mem::size_of::<PropertyKey>() + std::mem::size_of::<PropertyValue>() + 8);

        // PropertyValue heap sizes
        for value in self.changed.values() {
            size += value.estimated_heap_size();
        }

        // HashMap overhead for vector deltas
        size += self.vector_deltas.capacity()
            * (std::mem::size_of::<PropertyKey>() + std::mem::size_of::<VectorDelta>() + 8);

        // VectorDelta heap sizes
        for vec_delta in self.vector_deltas.values() {
            size += vec_delta.estimated_heap_size();
        }

        // HashSet overhead for removed
        size += self.removed.capacity() * (std::mem::size_of::<PropertyKey>() + 8);

        size
    }
}

impl Default for PropertyDelta {
    fn default() -> Self {
        Self::new()
    }
}

/// Version data - either a full snapshot (anchor) or a delta.
#[derive(Debug, Clone, PartialEq)]
pub enum VersionData {
    /// Full snapshot of properties (anchor point)
    Anchor {
        /// The complete property map
        properties: PropertyMap,
        /// Stable ID of corresponding temporal vector snapshot (if any)
        ///
        /// Links this graph anchor to a TemporalVectorIndex snapshot for provenance tracking.
        /// `None` means no vector snapshot exists for this anchor (normal if no vectors present
        /// or temporal vector indexing is disabled).
        vector_snapshot_id: Option<usize>,
    },
    /// Delta from previous version
    Delta {
        /// The property changes
        delta: PropertyDelta,
    },
}

impl VersionData {
    /// Create an anchor version with the given properties.
    pub fn anchor(properties: PropertyMap) -> Self {
        VersionData::Anchor {
            properties,
            vector_snapshot_id: None,
        }
    }

    /// Create a delta version from two property maps.
    pub fn delta_from_diff(old: &PropertyMap, new: &PropertyMap) -> Self {
        VersionData::Delta {
            delta: PropertyDelta::from_diff(old, new),
        }
    }

    /// Returns true if this is an anchor.
    pub fn is_anchor(&self) -> bool {
        matches!(self, VersionData::Anchor { .. })
    }

    /// Returns true if this is a delta.
    pub fn is_delta(&self) -> bool {
        matches!(self, VersionData::Delta { .. })
    }

    /// Set the vector snapshot ID for an anchor.
    ///
    /// This links the anchor to a temporal vector index snapshot for provenance tracking.
    /// Only works on Anchor variants; silently does nothing for Delta variants.
    ///
    /// # Arguments
    /// - `id`: Stable snapshot ID returned by TemporalVectorIndex
    pub fn set_vector_snapshot_id(&mut self, id: usize) {
        if let VersionData::Anchor {
            vector_snapshot_id, ..
        } = self
        {
            *vector_snapshot_id = Some(id);
        }
    }

    /// Get the vector snapshot ID from an anchor.
    ///
    /// Returns the linked snapshot ID if this is an Anchor with a snapshot, or `None` otherwise.
    /// Delta variants always return `None`.
    ///
    /// # Returns
    /// - `Some(id)`: Anchor has a linked vector snapshot
    /// - `None`: No snapshot linked (Delta, or Anchor without vector snapshot)
    pub fn get_vector_snapshot_id(&self) -> Option<usize> {
        match self {
            VersionData::Anchor {
                vector_snapshot_id, ..
            } => *vector_snapshot_id,
            _ => None,
        }
    }

    /// Estimate the heap memory usage of this version data in bytes.
    ///
    /// This provides a rough estimate of heap allocations for memory accounting.
    pub fn estimated_heap_size(&self) -> usize {
        match self {
            VersionData::Anchor { properties, .. } => properties.estimated_heap_size(),
            VersionData::Delta { delta } => delta.estimated_heap_size(),
        }
    }
}

/// A version of a node at a specific point in time.
#[derive(Debug, Clone)]
pub struct NodeVersion {
    /// Unique version identifier
    pub id: VersionId,
    /// ID of the node this version belongs to
    pub node_id: NodeId,
    /// Temporal interval when this version was valid
    pub temporal: BiTemporalInterval,
    /// Label of the node (may change over time)
    pub label: InternedString,
    /// Version data (anchor or delta)
    pub data: VersionData,
    /// Link to the next version in the chain (None if this is the latest)
    pub next_version: Option<VersionId>,
    /// Link to the previous version (for reverse traversal)
    pub prev_version: Option<VersionId>,
}

impl NodeVersion {
    /// Create a new anchor version (full snapshot).
    pub fn new_anchor(
        id: VersionId,
        node_id: NodeId,
        temporal: BiTemporalInterval,
        label: InternedString,
        properties: PropertyMap,
    ) -> Self {
        NodeVersion {
            id,
            node_id,
            temporal,
            label,
            data: VersionData::anchor(properties),
            next_version: None,
            prev_version: None,
        }
    }

    /// Create a new delta version (incremental change).
    pub fn new_delta(
        id: VersionId,
        node_id: NodeId,
        temporal: BiTemporalInterval,
        label: InternedString,
        old_properties: &PropertyMap,
        new_properties: &PropertyMap,
        prev_version: VersionId,
    ) -> Self {
        NodeVersion {
            id,
            node_id,
            temporal,
            label,
            data: VersionData::delta_from_diff(old_properties, new_properties),
            next_version: None,
            prev_version: Some(prev_version),
        }
    }

    /// Returns true if this is an anchor version.
    #[inline]
    pub fn is_anchor(&self) -> bool {
        self.data.is_anchor()
    }

    /// Returns true if this is a delta version.
    #[inline]
    pub fn is_delta(&self) -> bool {
        self.data.is_delta()
    }

    /// Estimate the total memory usage of this version in bytes.
    ///
    /// This includes both stack size and estimated heap allocations, useful
    /// for memory accounting in tiered storage migration decisions.
    pub fn estimated_size(&self) -> usize {
        std::mem::size_of::<Self>() + self.data.estimated_heap_size()
    }
}

impl TemporalVersion for NodeVersion {
    fn temporal(&self) -> &BiTemporalInterval {
        &self.temporal
    }

    fn temporal_mut(&mut self) -> &mut BiTemporalInterval {
        &mut self.temporal
    }
}

/// A version of an edge at a specific point in time.
#[derive(Debug, Clone)]
pub struct EdgeVersion {
    /// Unique version identifier
    pub id: VersionId,
    /// ID of the edge this version belongs to
    pub edge_id: EdgeId,
    /// Temporal interval when this version was valid
    pub temporal: BiTemporalInterval,
    /// Label of the edge (may change over time)
    pub label: InternedString,
    /// Source node ID
    pub source: NodeId,
    /// Target node ID
    pub target: NodeId,
    /// Version data (anchor or delta)
    pub data: VersionData,
    /// Link to the next version in the chain
    pub next_version: Option<VersionId>,
    /// Link to the previous version
    pub prev_version: Option<VersionId>,
}

impl EdgeVersion {
    /// Create a new anchor version (full snapshot).
    pub fn new_anchor(
        id: VersionId,
        edge_id: EdgeId,
        temporal: BiTemporalInterval,
        label: InternedString,
        source: NodeId,
        target: NodeId,
        properties: PropertyMap,
    ) -> Self {
        EdgeVersion {
            id,
            edge_id,
            temporal,
            label,
            source,
            target,
            data: VersionData::anchor(properties),
            next_version: None,
            prev_version: None,
        }
    }

    /// Create a new delta version (incremental change).
    #[allow(clippy::too_many_arguments)]
    pub fn new_delta(
        id: VersionId,
        edge_id: EdgeId,
        temporal: BiTemporalInterval,
        label: InternedString,
        source: NodeId,
        target: NodeId,
        old_properties: &PropertyMap,
        new_properties: &PropertyMap,
        prev_version: VersionId,
    ) -> Self {
        EdgeVersion {
            id,
            edge_id,
            temporal,
            label,
            source,
            target,
            data: VersionData::delta_from_diff(old_properties, new_properties),
            next_version: None,
            prev_version: Some(prev_version),
        }
    }

    /// Returns true if this is an anchor version.
    #[inline]
    pub fn is_anchor(&self) -> bool {
        self.data.is_anchor()
    }

    /// Returns true if this is a delta version.
    #[inline]
    pub fn is_delta(&self) -> bool {
        self.data.is_delta()
    }

    /// Estimate the total memory usage of this version in bytes.
    ///
    /// This includes both stack size and estimated heap allocations, useful
    /// for memory accounting in tiered storage migration decisions.
    pub fn estimated_size(&self) -> usize {
        std::mem::size_of::<Self>() + self.data.estimated_heap_size()
    }
}

impl TemporalVersion for EdgeVersion {
    fn temporal(&self) -> &BiTemporalInterval {
        &self.temporal
    }

    fn temporal_mut(&mut self) -> &mut BiTemporalInterval {
        &mut self.temporal
    }
}

/// Trait for version types that support common version chain operations.
///
/// This trait extends [`TemporalVersion`] with methods needed for version chain
/// management, enabling generic implementations of version creation logic
/// and reducing code duplication between node and edge version handling.
///
/// # Trait Bounds
///
/// This trait requires `TemporalVersion`, which provides access to temporal
/// intervals via `temporal()` and `temporal_mut()`. This allows generic code
/// to both read and modify temporal properties.
///
/// # Example
///
/// ```ignore
/// use crate::storage::version::EntityVersion;
///
/// /// Count versions in a chain until reaching an anchor.
/// fn count_deltas_to_anchor<V: EntityVersion>(
///     start_version: &V,
///     get_version: impl Fn(VersionId) -> Option<&V>,
/// ) -> usize {
///     let mut count = 0;
///     let mut current_id = start_version.prev_version();
///
///     while let Some(vid) = current_id {
///         if let Some(version) = get_version(vid) {
///             if version.is_anchor() {
///                 break;
///             }
///             count += 1;
///             current_id = version.prev_version();
///         } else {
///             break;
///         }
///     }
///     count
/// }
/// ```
pub trait EntityVersion: TemporalVersion {
    /// Get the version's unique identifier.
    fn version_id(&self) -> VersionId;

    /// Check if this version is an anchor (full snapshot).
    fn is_anchor(&self) -> bool;

    /// Get the link to the previous version in the chain.
    fn prev_version(&self) -> Option<VersionId>;

    /// Set the link to the previous version in the chain.
    fn set_prev_version(&mut self, version_id: Option<VersionId>);

    /// Get the link to the next version in the chain.
    fn next_version(&self) -> Option<VersionId>;

    /// Set the link to the next version in the chain.
    fn set_next_version(&mut self, version_id: Option<VersionId>);

    /// Get a mutable reference to the version data.
    fn data_mut(&mut self) -> &mut VersionData;
}

impl EntityVersion for NodeVersion {
    fn version_id(&self) -> VersionId {
        self.id
    }

    fn is_anchor(&self) -> bool {
        self.data.is_anchor()
    }

    fn prev_version(&self) -> Option<VersionId> {
        self.prev_version
    }

    fn set_prev_version(&mut self, version_id: Option<VersionId>) {
        self.prev_version = version_id;
    }

    fn next_version(&self) -> Option<VersionId> {
        self.next_version
    }

    fn set_next_version(&mut self, version_id: Option<VersionId>) {
        self.next_version = version_id;
    }

    fn data_mut(&mut self) -> &mut VersionData {
        &mut self.data
    }
}

impl EntityVersion for EdgeVersion {
    fn version_id(&self) -> VersionId {
        self.id
    }

    fn is_anchor(&self) -> bool {
        self.data.is_anchor()
    }

    fn prev_version(&self) -> Option<VersionId> {
        self.prev_version
    }

    fn set_prev_version(&mut self, version_id: Option<VersionId>) {
        self.prev_version = version_id;
    }

    fn next_version(&self) -> Option<VersionId> {
        self.next_version
    }

    fn set_next_version(&mut self, version_id: Option<VersionId>) {
        self.next_version = version_id;
    }

    fn data_mut(&mut self) -> &mut VersionData {
        &mut self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_property_delta_diff() {
        let old = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("city", "NYC")
            .build();

        let new = PropertyMapBuilder::new()
            .insert("name", "Alice") // Unchanged
            .insert("age", 31i64) // Modified
            .insert("country", "USA") // Added
            // city removed
            .build();

        let delta = PropertyDelta::from_diff(&old, &new);

        assert_eq!(delta.changed.len(), 2); // age modified, country added
        assert_eq!(delta.removed.len(), 1); // city removed
        assert!(
            delta
                .removed
                .contains(&GLOBAL_INTERNER.intern("city").unwrap())
        );
    }

    #[test]
    fn test_property_delta_apply() {
        let base = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let mut delta = PropertyDelta::new();
        delta.changed.insert(
            GLOBAL_INTERNER.intern("age").unwrap(),
            PropertyValue::Int(31),
        );
        delta.changed.insert(
            GLOBAL_INTERNER.intern("city").unwrap(),
            PropertyValue::string("NYC"),
        );

        let result = delta.apply(&base);

        assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("Alice"));
        assert_eq!(result.get("age").and_then(|v| v.as_int()), Some(31.into()));
        assert_eq!(result.get("city").and_then(|v| v.as_str()), Some("NYC"));
    }

    #[test]
    fn test_empty_delta() {
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();

        let delta = PropertyDelta::from_diff(&props, &props);
        assert!(delta.is_empty());
    }

    #[test]
    fn test_node_version_anchor() {
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();

        let temporal = BiTemporalInterval::current(1000.into());

        let version = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(10).unwrap(),
            temporal,
            crate::core::interning::GLOBAL_INTERNER
                .intern("Person")
                .unwrap(),
            props,
        );

        assert!(version.is_anchor());
        assert!(!version.is_delta());
        assert_eq!(version.node_id, NodeId::new(10).unwrap());
    }

    #[test]
    fn test_edge_version_delta() {
        let old_props = PropertyMapBuilder::new().insert("weight", 1i64).build();

        let new_props = PropertyMapBuilder::new().insert("weight", 2i64).build();

        let temporal = BiTemporalInterval::current(2000.into());

        let version = EdgeVersion::new_delta(
            VersionId::new(2).unwrap(),
            EdgeId::new(20).unwrap(),
            temporal,
            crate::core::interning::GLOBAL_INTERNER
                .intern("KNOWS")
                .unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            &old_props,
            &new_props,
            VersionId::new(1).unwrap(),
        );

        assert!(!version.is_anchor());
        assert!(version.is_delta());
        assert_eq!(version.prev_version, Some(VersionId::new(1).unwrap()));
    }

    // ========================================================================
    // Estimated Size Tests
    // ========================================================================

    #[test]
    fn test_property_delta_estimated_heap_size_empty() {
        let delta = PropertyDelta::new();
        let size = delta.estimated_heap_size();
        // Empty delta should have zero heap overhead
        assert_eq!(size, 0, "Empty delta heap size should be zero");
    }

    #[test]
    fn test_property_delta_estimated_heap_size_with_changes() {
        let mut delta = PropertyDelta::new();
        delta.changed.insert(
            GLOBAL_INTERNER.intern("name").unwrap(),
            PropertyValue::string("Alice"), // 5 bytes
        );
        delta.changed.insert(
            GLOBAL_INTERNER.intern("description").unwrap(),
            PropertyValue::string("A longer description"), // 20 bytes
        );
        delta
            .removed
            .insert(GLOBAL_INTERNER.intern("old_field").unwrap());

        let size = delta.estimated_heap_size();
        // Should include at least string lengths (5 + 20 = 25 bytes)
        assert!(
            size >= 25,
            "Delta with strings should include string heap size"
        );
    }

    #[test]
    fn test_version_data_estimated_heap_size_anchor() {
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let data = VersionData::anchor(props);
        let size = data.estimated_heap_size();
        // Anchor should include property map heap size
        assert!(size >= 5, "Anchor heap size should include string 'Alice'");
    }

    #[test]
    fn test_version_data_estimated_heap_size_delta() {
        let old_props = PropertyMapBuilder::new().insert("name", "Alice").build();
        let new_props = PropertyMapBuilder::new().insert("name", "Bob").build();

        let data = VersionData::delta_from_diff(&old_props, &new_props);
        let size = data.estimated_heap_size();
        // Delta should include the changed property heap size
        assert!(size >= 3, "Delta heap size should include string 'Bob'");
    }

    #[test]
    fn test_node_version_estimated_size() {
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("embedding", PropertyValue::vector(vec![0.1f32; 384]))
            .build();

        let temporal = BiTemporalInterval::current(1000.into());
        let version = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(10).unwrap(),
            temporal,
            GLOBAL_INTERNER.intern("Person").unwrap(),
            props,
        );

        let size = version.estimated_size();
        // Should include stack size + heap size (at least vector: 384 * 4 = 1536 bytes)
        assert!(
            size >= std::mem::size_of::<NodeVersion>() + 384 * 4,
            "Node version estimated size should include vector heap"
        );
    }

    #[test]
    fn test_edge_version_estimated_size() {
        let props = PropertyMapBuilder::new()
            .insert("weight", 1.5f64)
            .insert("label", "connection")
            .build();

        let temporal = BiTemporalInterval::current(1000.into());
        let version = EdgeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            EdgeId::new(20).unwrap(),
            temporal,
            GLOBAL_INTERNER.intern("CONNECTS").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            props,
        );

        let size = version.estimated_size();
        // Should include at least stack size + string "connection" (10 bytes)
        assert!(
            size >= std::mem::size_of::<EdgeVersion>() + 10,
            "Edge version estimated size should include string heap"
        );
    }

    #[test]
    fn test_node_version_estimated_size_delta() {
        let old_props = PropertyMapBuilder::new().insert("count", 1i64).build();
        let new_props = PropertyMapBuilder::new().insert("count", 2i64).build();

        let temporal = BiTemporalInterval::current(2000.into());
        let version = NodeVersion::new_delta(
            VersionId::new(2).unwrap(),
            NodeId::new(10).unwrap(),
            temporal,
            GLOBAL_INTERNER.intern("Counter").unwrap(),
            &old_props,
            &new_props,
            VersionId::new(1).unwrap(),
        );

        let size = version.estimated_size();
        // Delta version should have smaller heap size than anchor with full data
        assert!(
            size >= std::mem::size_of::<NodeVersion>(),
            "Delta version size should include at least stack size"
        );
    }

    // ========================================================================
    // Vector Delta Optimization Tests (Issue #215)
    // ========================================================================

    #[test]
    fn test_vector_delta_sparse_optimization_single_element() {
        // Verify optimization: sparse delta for single element change
        let old_embedding = vec![0.1f32; 1536]; // OpenAI ada-002 size
        let mut new_embedding = old_embedding.clone();
        new_embedding[500] = 0.2f32; // Change only one element

        let old_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&old_embedding))
            .build();

        let new_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&new_embedding))
            .build();

        let delta = PropertyDelta::from_diff(&old_props, &new_props);

        // Optimized behavior: vector stored in vector_deltas, not in changed
        assert_eq!(
            delta.changed.len(),
            0,
            "Vector should not be in changed (uses sparse delta)"
        );
        assert_eq!(
            delta.vector_deltas.len(),
            1,
            "Vector should be in vector_deltas"
        );

        let delta_size = delta.estimated_heap_size();
        let full_vector_size = 1536 * std::mem::size_of::<f32>();

        // Sparse storage should be much smaller than full vector
        assert!(
            delta_size < full_vector_size / 10,
            "Sparse delta ({} bytes) should be much smaller than full vector ({} bytes)",
            delta_size,
            full_vector_size
        );

        println!(
            "OPTIMIZATION SUCCESS: Storing {} bytes (vs {} full) for 1-element change in 1536-dim vector ({}x savings)",
            delta_size,
            full_vector_size,
            full_vector_size / delta_size.max(1)
        );
    }

    #[test]
    fn test_vector_delta_sparse_optimization_multiple_elements() {
        // Verify optimization: sparse delta for multiple elements changed
        let old_embedding = vec![0.1f32; 384];
        let mut new_embedding = old_embedding.clone();

        // Change 5 elements (1.3% of vector)
        new_embedding[10] = 0.5f32;
        new_embedding[50] = 0.6f32;
        new_embedding[100] = 0.7f32;
        new_embedding[200] = 0.8f32;
        new_embedding[300] = 0.9f32;

        let old_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&old_embedding))
            .build();

        let new_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&new_embedding))
            .build();

        let delta = PropertyDelta::from_diff(&old_props, &new_props);

        // Optimized behavior: uses sparse delta
        assert_eq!(delta.vector_deltas.len(), 1, "Should have vector delta");
        assert_eq!(delta.changed.len(), 0, "Should not store in changed");

        let delta_size = delta.estimated_heap_size();
        let full_vector_size = 384 * std::mem::size_of::<f32>();

        // Sparse storage should be much smaller
        assert!(
            delta_size < full_vector_size / 4,
            "Sparse delta ({} bytes) should be much smaller than full vector ({} bytes)",
            delta_size,
            full_vector_size
        );

        let optimal_sparse_size = 5 * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>());
        println!(
            "OPTIMIZATION SUCCESS: {} bytes (vs {} full, {} raw sparse data) - {}x savings over full",
            delta_size,
            full_vector_size,
            optimal_sparse_size,
            full_vector_size / delta_size.max(1)
        );
    }

    #[test]
    fn test_vector_delta_no_change() {
        // Test edge case: vector unchanged should result in empty delta
        let embedding = vec![0.1f32; 384];

        let old_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&embedding))
            .build();

        let new_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&embedding))
            .build();

        let delta = PropertyDelta::from_diff(&old_props, &new_props);

        assert!(
            delta.is_empty(),
            "Delta should be empty when vector is unchanged"
        );
    }

    #[test]
    fn test_vector_delta_complete_replacement() {
        // Test case: entire vector changed (common case for regenerated embeddings)
        let old_embedding = vec![0.1f32; 384];
        let new_embedding = vec![0.9f32; 384]; // Completely different

        let old_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&old_embedding))
            .build();

        let new_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&new_embedding))
            .build();

        let delta = PropertyDelta::from_diff(&old_props, &new_props);

        // For complete replacement, full storage is optimal (no benefit from sparse)
        let delta_size = delta.estimated_heap_size();
        let full_vector_size = 384 * std::mem::size_of::<f32>();

        assert!(
            delta_size >= full_vector_size,
            "Full vector storage is expected for complete replacement"
        );
    }

    #[test]
    fn test_mixed_properties_with_vector_delta_optimization() {
        // Test case: multiple properties changed, including a vector with sparse optimization
        let old_embedding = vec![0.1f32; 384];
        let mut new_embedding = old_embedding.clone();
        new_embedding[0] = 0.2f32; // Change one element

        let old_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("embedding", PropertyValue::vector(&old_embedding))
            .build();

        let new_props = PropertyMapBuilder::new()
            .insert("name", "Alice") // Unchanged
            .insert("age", 31i64) // Changed
            .insert("embedding", PropertyValue::vector(&new_embedding)) // One element changed
            .build();

        let delta = PropertyDelta::from_diff(&old_props, &new_props);

        // Should have age in changed, embedding in vector_deltas
        assert_eq!(delta.changed.len(), 1, "Should have age changed");
        assert_eq!(
            delta.vector_deltas.len(),
            1,
            "Should have embedding in vector_deltas"
        );

        let delta_size = delta.estimated_heap_size();
        let full_vector_size = 384 * std::mem::size_of::<f32>();

        // Even with mixed properties, vector delta should save space
        assert!(
            delta_size < full_vector_size / 2,
            "Mixed delta with sparse vector should be smaller than full vector"
        );

        println!(
            "OPTIMIZATION: Mixed delta stores {} bytes (sparse vector + age property)",
            delta_size
        );
    }

    // ========================================================================
    // Sparse Vector Delta Tests (Desired Behavior - TDD)
    // ========================================================================

    #[test]
    fn test_sparse_vector_delta_single_element() {
        // Desired behavior: sparse storage for single element change
        let old_embedding = vec![0.1f32; 1536];
        let mut new_embedding = old_embedding.clone();
        new_embedding[500] = 0.2f32;

        let old_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&old_embedding))
            .build();

        let new_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&new_embedding))
            .build();

        let delta = PropertyDelta::from_diff(&old_props, &new_props);

        // Sparse storage: index (4 bytes) + value (4 bytes) + HashMap overhead
        let sparse_data_size = std::mem::size_of::<u32>() + std::mem::size_of::<f32>();
        let delta_size = delta.estimated_heap_size();
        let full_vector_size = 1536 * std::mem::size_of::<f32>();

        // Delta should be MUCH smaller than full vector (1536 * 4 = 6144 bytes)
        // Even with HashMap overhead, sparse should be < 5% of full vector size
        assert!(
            delta_size < full_vector_size / 20,
            "Sparse delta ({} bytes) should be much smaller than full vector ({} bytes). Raw data: {} bytes",
            delta_size,
            full_vector_size,
            sparse_data_size
        );

        // Verify it can be applied correctly
        let result = delta.apply(&old_props);
        assert_eq!(
            result.get("embedding").and_then(|v| v.as_vector()),
            new_props.get("embedding").and_then(|v| v.as_vector()),
            "Applied delta should produce correct result"
        );
    }

    #[test]
    fn test_sparse_vector_delta_few_elements() {
        // Desired behavior: sparse storage for small percentage of changes
        let old_embedding = vec![0.1f32; 384];
        let mut new_embedding = old_embedding.clone();

        // Change 10 elements (~2.6% of vector)
        let changed_indices = vec![10, 50, 100, 150, 200, 250, 300, 350, 375, 383];
        for &idx in &changed_indices {
            new_embedding[idx] = 0.9f32;
        }

        let old_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&old_embedding))
            .build();

        let new_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&new_embedding))
            .build();

        let delta = PropertyDelta::from_diff(&old_props, &new_props);

        // Sparse storage: 10 * (4 bytes index + 4 bytes value) = 80 bytes + HashMap overhead
        let sparse_data_size = 10 * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>());
        let delta_size = delta.estimated_heap_size();
        let full_vector_size = 384 * std::mem::size_of::<f32>();

        // Should be much smaller than full vector (384 * 4 = 1536 bytes)
        // Even with HashMap overhead, should be < 25% of full vector size
        assert!(
            delta_size < full_vector_size / 4,
            "Sparse delta ({} bytes) should be much smaller than full vector ({} bytes). Raw data: {} bytes",
            delta_size,
            full_vector_size,
            sparse_data_size
        );

        // Verify correctness
        let result = delta.apply(&old_props);
        assert_eq!(
            result.get("embedding").and_then(|v| v.as_vector()),
            new_props.get("embedding").and_then(|v| v.as_vector())
        );
    }

    #[test]
    fn test_sparse_vector_delta_threshold_behavior() {
        // Desired behavior: use sparse storage for few changes, full storage for many changes
        // This tests the threshold logic (e.g., if >50% changed, use full storage)

        // Case 1: 10% changed -> should use sparse
        let old_embedding = vec![0.1f32; 384];
        let mut new_embedding_sparse = old_embedding.clone();
        for i in 0..38 {
            // 10% of 384
            new_embedding_sparse[i] = 0.9f32;
        }

        let old_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&old_embedding))
            .build();

        let sparse_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&new_embedding_sparse))
            .build();

        let sparse_delta = PropertyDelta::from_diff(&old_props, &sparse_props);
        let sparse_size = sparse_delta.estimated_heap_size();

        // Case 2: 90% changed -> should use full storage
        let mut new_embedding_full = old_embedding.clone();
        for i in 0..346 {
            // 90% of 384
            new_embedding_full[i] = 0.9f32;
        }

        let full_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&new_embedding_full))
            .build();

        let full_delta = PropertyDelta::from_diff(&old_props, &full_props);
        let full_size = full_delta.estimated_heap_size();

        // Sparse delta should be smaller than full delta
        assert!(
            sparse_size < full_size / 2,
            "Sparse delta ({} bytes) should be significantly smaller than full delta ({} bytes)",
            sparse_size,
            full_size
        );
    }

    #[test]
    fn test_sparse_vector_delta_edge_cases() {
        // Test edge cases for sparse vector optimization

        // Case 1: First element changed
        let old_embedding = vec![0.1f32; 384];
        let mut new_embedding = old_embedding.clone();
        new_embedding[0] = 0.9f32;

        let old_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&old_embedding))
            .build();

        let new_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&new_embedding))
            .build();

        let delta = PropertyDelta::from_diff(&old_props, &new_props);
        let result = delta.apply(&old_props);
        assert_eq!(
            result.get("embedding").and_then(|v| v.as_vector()),
            new_props.get("embedding").and_then(|v| v.as_vector()),
            "First element change should work correctly"
        );

        // Case 2: Last element changed
        let mut new_embedding_last = old_embedding.clone();
        new_embedding_last[383] = 0.9f32;

        let new_props_last = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&new_embedding_last))
            .build();

        let delta_last = PropertyDelta::from_diff(&old_props, &new_props_last);
        let result_last = delta_last.apply(&old_props);
        assert_eq!(
            result_last.get("embedding").and_then(|v| v.as_vector()),
            new_props_last.get("embedding").and_then(|v| v.as_vector()),
            "Last element change should work correctly"
        );
    }
}
