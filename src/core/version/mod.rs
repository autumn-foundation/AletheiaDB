//! Version management and metadata.
//!
//! This module implements the version chain structures that enable time-traveling
//! queries. Each node and edge can have multiple versions over time, linked together
//! in a chain ordered by transaction time.
//!
//! It includes:
//! - `VersionMetadata`: Metadata about version creation (Snapshot Isolation).
//! - `NodeVersion` / `EdgeVersion`: The version structures.
//! - `VersionData`: Payload (Anchor or Delta).

use crate::core::error::Result;
use crate::core::hasher::IdentityHasher;
use crate::core::id::{EdgeId, NodeId, TxId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::{MAX_VECTOR_DIMENSIONS, PropertyKey, PropertyMap, PropertyValue};
use crate::core::provenance::Provenance;
use crate::core::temporal::{BiTemporalInterval, Timestamp};
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasherDefault;
use std::sync::Arc;

/// Fast HashMap using IdentityHasher for interned keys.
pub type FastHashMap<K, V> = HashMap<K, V, BuildHasherDefault<IdentityHasher>>;

/// Fast HashSet using IdentityHasher for interned keys.
pub type FastHashSet<T> = HashSet<T, BuildHasherDefault<IdentityHasher>>;

/// Metadata about version creation for Snapshot Isolation.
///
/// This tracks which transaction created a version and when it was committed,
/// enabling proper visibility checking for Snapshot Isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionMetadata {
    /// Transaction that created this version
    ///
    /// Note: For historical versions reconstructed from storage (not currently in memory),
    /// this may be `TxId(0)` if the creating transaction ID was not preserved in the
    /// historical storage format.
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

/// Epsilon for floating-point comparisons in vector deltas.
///
/// Used to determine if two f32 values are effectively equal, accounting for
/// floating-point precision limitations. Value chosen to be robust for typical
/// embedding use cases while avoiding spurious deltas.
const VECTOR_EPSILON: f32 = 1e-7;

/// Helper for approximate float equality that handles NaN and Infinity correctly.
///
/// Ensures that:
/// - NaN == NaN (treated as equal for change detection)
/// - NaN != Finite
/// - Inf == Inf (same sign)
/// - Finite values compared with epsilon
fn floats_approx_equal(a: f32, b: f32) -> bool {
    if a.is_nan() {
        return b.is_nan();
    }
    if b.is_nan() {
        return false;
    }
    if a.is_infinite() || b.is_infinite() {
        return a == b;
    }
    (a - b).abs() <= VECTOR_EPSILON
}

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
            if !floats_approx_equal(*old_val, *new_val) {
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
                    if idx1 != idx2 || !floats_approx_equal(*val1, *val2) {
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
                    if !floats_approx_equal(*v1, *v2) {
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
    fn close_transaction_time(&mut self, end_timestamp: Timestamp) -> Result<()> {
        let temporal = self.temporal_mut();
        *temporal = temporal.close_transaction_time(end_timestamp)?;
        Ok(())
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
///
/// # Examples
///
/// ```rust
/// use aletheiadb::core::property::PropertyMapBuilder;
/// use aletheiadb::core::version::PropertyDelta;
///
/// let old_props = PropertyMapBuilder::new()
///     .insert("name", "Alice")
///     .insert("age", 30i64)
///     .build();
///
/// let new_props = PropertyMapBuilder::new()
///     .insert("name", "Alice")
///     .insert("age", 31i64) // age changed
///     .build();
///
/// let delta = PropertyDelta::from_diff(&old_props, &new_props);
/// assert!(!delta.is_empty());
///
/// let result = delta.apply(&old_props);
/// assert_eq!(result.get("age").and_then(|v| v.as_int()), Some(31));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyDelta {
    /// Properties that were added or modified (non-vector)
    pub changed: FastHashMap<PropertyKey, PropertyValue>,
    /// Vector properties with sparse delta optimization
    pub vector_deltas: FastHashMap<PropertyKey, VectorDelta>,
    /// Properties that were removed
    pub removed: FastHashSet<PropertyKey>,
}

impl PropertyDelta {
    /// Create a new empty delta.
    pub fn new() -> Self {
        PropertyDelta {
            changed: FastHashMap::with_hasher(BuildHasherDefault::default()),
            vector_deltas: FastHashMap::with_hasher(BuildHasherDefault::default()),
            removed: FastHashSet::with_hasher(BuildHasherDefault::default()),
        }
    }

    /// Create a delta by comparing two property maps.
    ///
    /// Returns the changes needed to transform `old` into `new`.
    /// Uses sparse delta compression for vector properties (Issue #215).
    ///
    /// # Performance (Issue #214)
    ///
    /// This is optimized for the hot path (creating delta versions with anchor_interval=10).
    /// The implementation:
    ///
    /// 1. Uses default HashMap capacity, growing as needed during iteration
    /// 2. Uses Arc::clone() for property values (O(1) refcount increment)
    /// 3. Uses sparse vector deltas when beneficial (Issue #215)
    /// 4. PropertyKey is InternedString (O(1) copy) - Issue #202
    pub fn from_diff(old: &PropertyMap, new: &PropertyMap) -> Self {
        // Fast path: if the maps are identical (Arc pointer equality), the delta is empty.
        // This is a O(1) check that avoids iterating over the map content.
        if old == new {
            return PropertyDelta::new();
        }

        // Start with default capacity - in from_diff, we don't know upfront how many
        // properties will change, so pre-allocation could waste memory. HashMap will
        // grow as needed during iteration.
        let mut delta = PropertyDelta::new();

        // Find added and modified properties
        for (key, new_value) in new.iter() {
            match old.get_by_interned_key(key) {
                Some(old_value) => {
                    // Optimization: check semantic equality first to skip unchanged
                    // This handles NaN equality and Arc pointer equality internally
                    if old_value.semantically_equal(new_value) {
                        continue;
                    }

                    // Modified - check if both are vectors for sparse delta optimization
                    match (old_value.as_vector(), new_value.as_vector()) {
                        (Some(old_vec), Some(new_vec)) => {
                            // Both are vectors - use sparse delta if beneficial
                            if let Some(vec_delta) = VectorDelta::from_diff(old_vec, new_vec) {
                                delta.vector_deltas.insert(*key, vec_delta);
                            } else if old_vec.len() != new_vec.len() {
                                // If dimensions differ, treat as full replacement
                                delta.changed.insert(*key, new_value.clone());
                            }
                            // Else: Dimensions match but no changes > epsilon. Treated as no change.
                        }
                        _ => {
                            // Not both vectors, or value added/type changed - store full value
                            // Arc clone - O(1) refcount increment, shares underlying data
                            delta.changed.insert(*key, new_value.clone());
                        }
                    }
                }
                None => {
                    // Property added - store full value
                    // Arc clone - O(1) refcount increment
                    delta.changed.insert(*key, new_value.clone());
                }
            }
        }

        // Find removed properties
        for key in old.keys() {
            if !new.contains_interned_key(key) {
                // PropertyKey copy - O(1) InternedString ID copy
                delta.removed.insert(*key);
            }
        }

        delta
    }

    /// Apply this delta to a property map, producing a new map.
    ///
    /// # Performance (Issue #214)
    ///
    /// This is optimized for the hot path (time-travel queries with anchor_interval=10
    /// apply up to 9 deltas sequentially). The implementation:
    ///
    /// 1. Pre-calculates capacity to avoid HashMap resizing
    /// 2. Directly constructs the result HashMap without builder overhead
    /// 3. Uses Arc::clone() for unchanged properties (O(1) refcount increment)
    /// 4. Only clones modified properties (also O(1) due to Arc)
    ///
    /// This avoids the wasteful pattern of `base.clone().builder()` which would:
    /// - Clone the Arc (cheap)
    /// - Try to unwrap it (fails due to refcount > 1)
    /// - Fall back to cloning the entire HashMap structure
    ///
    /// # Failure Modes (Fail-Open)
    ///
    /// When applying a sparse vector delta (`VectorDelta::Sparse`):
    /// - If the base property exists and is a vector of matching dimension, the delta is applied.
    /// - If the base property is **missing** or has the **wrong type**, the sparse delta is **silently ignored**.
    ///
    /// This "fail-open" behavior is intentional for query/view construction to provide
    /// best-effort results even if the base state is inconsistent (e.g., during development
    /// or partial data recovery). It prevents the entire view from failing due to a single
    /// corrupted property history.
    pub fn apply(&self, base: &PropertyMap) -> PropertyMap {
        // Fast path: if delta is empty, return the base map (Arc clone).
        // This is a O(1) operation that avoids allocation and copying.
        if self.is_empty() {
            return base.clone();
        }

        // Calculate capacity for the new map to avoid reallocation
        // Properties from base (minus removed) plus potentially new properties from changes
        let estimated_capacity = base
            .len()
            .saturating_sub(self.removed.len())
            .max(self.changed.len() + self.vector_deltas.len());

        // Use standard HashMap for construction, PropertyMap::from_iter will handle internal structure
        // PropertyMap uses IdentityHasher internally too
        let mut result: FastHashMap<PropertyKey, PropertyValue> =
            FastHashMap::with_capacity_and_hasher(
                estimated_capacity,
                BuildHasherDefault::<IdentityHasher>::default(),
            );

        // Copy all base properties except removed ones (single lookup per property)
        // This is optimal when changes << base (typical case: ~1-10% change rate)
        for (key, value) in base.iter() {
            if !self.removed.contains(key) {
                // Arc clone - O(1) refcount increment, shares underlying data
                result.insert(*key, value.clone());
            }
        }

        // Apply regular changes (overwrites existing entries for modified properties)
        for (key, value) in &self.changed {
            // Arc clone - O(1) refcount increment
            result.insert(*key, value.clone());
        }

        // Apply vector deltas (overwrites existing entries)
        for (key, vec_delta) in &self.vector_deltas {
            match vec_delta {
                // Full replacement does not depend on base type/presence.
                VectorDelta::Full(vec) => {
                    result.insert(*key, PropertyValue::Vector(vec.clone()));
                }
                // Sparse delta requires vector base value.
                VectorDelta::Sparse { .. } => {
                    if let Some(base_value) = base.get_by_interned_key(key)
                        && let Some(base_vec) = base_value.as_vector()
                    {
                        let new_vec = vec_delta.apply(base_vec);
                        result.insert(*key, new_vec.into());
                    }
                }
            }
        }

        // Convert HashMap to PropertyMap using FromIterator
        result.into_iter().collect()
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
    /// # Failure Modes (Fail-Closed)
    ///
    /// Unlike [`apply`](Self::apply), this method is **fail-closed**. If a base property
    /// is missing or invalid for a sparse delta, it returns an error. This strictness
    /// is required for persistence to ensure data integrity - we cannot persist a
    /// sparse delta that cannot be fully resolved, as this would lead to permanent data loss.
    ///
    /// # Side Effects
    ///
    /// Moves materialized vectors from `vector_deltas` to `changed`, converting them
    /// to full `PropertyValue::Vector` instances. After this operation, `vector_deltas`
    /// will only contain `VectorDelta::Full` entries (which are then moved to `changed`).
    pub fn materialize_vector_deltas(
        &mut self,
        base: &PropertyMap,
    ) -> std::result::Result<(), String> {
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
                        self.changed.insert(key, new_vec.into());
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
///
/// Represents the payload of a node or edge version. If the node or edge has not changed
/// sufficiently to warrant a full snapshot, it may store a [`VersionData::Delta`] representing
/// changes from the prior version. Otherwise, it stores a [`VersionData::Anchor`].
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
///
/// # Embedded Commit Timestamp (Issue #238)
///
/// Follows the HyPer/TiDB architectural pattern: the commit timestamp is
/// embedded directly in the version struct so visibility checks can be
/// performed with a single comparison (`version.commit_timestamp < snapshot_ts`)
/// without acquiring the `TxVisibilityManager::committed` map lock.
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
    /// Commit timestamp embedded directly in the version (HyPer/TiDB pattern, Issue #238).
    ///
    /// Equals `temporal.transaction_time().start()`. Stored explicitly so that
    /// visibility checks bypass the `TxVisibilityManager::committed` map.
    pub commit_timestamp: Timestamp,
    /// Link to the next version in the chain (None if this is the latest)
    pub next_version: Option<VersionId>,
    /// Link to the previous version (for reverse traversal)
    pub prev_version: Option<VersionId>,
    /// Write-time attributive provenance (source, confidence, note, correlation_id).
    ///
    /// `None` unless the write that created this version supplied a
    /// [`Provenance`] bundle (Issue #3224). Distinct from `commit_timestamp`
    /// (which records *when* this version was written): provenance records
    /// *who/what* wrote it and *how confident* the writer was.
    pub provenance: Option<Arc<Provenance>>,
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
        let commit_timestamp = temporal.transaction_time().start();
        NodeVersion {
            id,
            node_id,
            temporal,
            label,
            data: VersionData::anchor(properties),
            commit_timestamp,
            next_version: None,
            prev_version: None,
            provenance: None,
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
        let commit_timestamp = temporal.transaction_time().start();
        NodeVersion {
            id,
            node_id,
            temporal,
            label,
            data: VersionData::delta_from_diff(old_properties, new_properties),
            commit_timestamp,
            next_version: None,
            prev_version: Some(prev_version),
            provenance: None,
        }
    }

    /// Attach a provenance bundle to this version, replacing any previous value.
    #[must_use]
    pub fn with_provenance(mut self, provenance: Option<Arc<Provenance>>) -> Self {
        self.provenance = provenance;
        self
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
///
/// # Embedded Commit Timestamp (Issue #238)
///
/// Follows the HyPer/TiDB architectural pattern: the commit timestamp is
/// embedded directly in the version struct so visibility checks can be
/// performed with a single comparison without a committed-map lookup.
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
    /// Commit timestamp embedded directly in the version (HyPer/TiDB pattern, Issue #238).
    ///
    /// Equals `temporal.transaction_time().start()`. Stored explicitly so that
    /// visibility checks bypass the `TxVisibilityManager::committed` map.
    pub commit_timestamp: Timestamp,
    /// Link to the next version in the chain
    pub next_version: Option<VersionId>,
    /// Link to the previous version
    pub prev_version: Option<VersionId>,
    /// Write-time attributive provenance (source, confidence, note, correlation_id).
    ///
    /// `None` unless the write that created this version supplied a
    /// [`Provenance`] bundle (Issue #3224).
    pub provenance: Option<Arc<Provenance>>,
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
        let commit_timestamp = temporal.transaction_time().start();
        EdgeVersion {
            id,
            edge_id,
            temporal,
            label,
            source,
            target,
            data: VersionData::anchor(properties),
            commit_timestamp,
            next_version: None,
            prev_version: None,
            provenance: None,
        }
    }

    /// Attach a provenance bundle to this version, replacing any previous value.
    #[must_use]
    pub fn with_provenance(mut self, provenance: Option<Arc<Provenance>>) -> Self {
        self.provenance = provenance;
        self
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
        let commit_timestamp = temporal.transaction_time().start();
        EdgeVersion {
            id,
            edge_id,
            temporal,
            label,
            source,
            target,
            data: VersionData::delta_from_diff(old_properties, new_properties),
            commit_timestamp,
            next_version: None,
            prev_version: Some(prev_version),
            provenance: None,
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
/// use crate::core::version::EntityVersion;
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
mod embedded_commit_timestamp_tests;
#[cfg(test)]
mod metadata_tests;
#[cfg(test)]
mod mutant_kill_tests;
#[cfg(test)]
mod sentry_tests;
#[cfg(test)]
mod storage_tests;
