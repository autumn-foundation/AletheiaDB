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
                        result.insert(*key, PropertyValue::vector(&new_vec));
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
mod metadata_tests {
    use super::*;

    #[test]
    fn test_version_metadata_new() {
        let tx_id = TxId::new(100);
        let timestamp = Timestamp::from(5000);
        let metadata = VersionMetadata::new(tx_id, timestamp);

        assert_eq!(metadata.created_by_tx, tx_id);
        assert_eq!(metadata.commit_timestamp, Some(timestamp));
    }

    #[test]
    fn test_version_metadata_uncommitted() {
        let tx_id = TxId::new(200);
        let metadata = VersionMetadata::uncommitted(tx_id);

        assert_eq!(metadata.created_by_tx, tx_id);
        assert_eq!(metadata.commit_timestamp, None);
    }

    #[test]
    fn test_version_metadata_default() {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let exe = std::env::current_exe().expect("failed to locate current test binary");
        let mut child = Command::new(exe)
            .args([
                "--ignored",
                "--exact",
                "core::version::metadata_tests::test_version_metadata_default_subprocess_helper",
            ])
            .spawn()
            .expect("failed to spawn subprocess for default metadata test");

        // CI environments can be slow, so give it plenty of time (10s)
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(
                        status.success(),
                        "subprocess helper failed for VersionMetadata default semantics"
                    );
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("VersionMetadata::default/default_for_existing did not complete");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("failed while polling subprocess: {e}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn test_version_metadata_default_subprocess_helper() {
        let metadata = VersionMetadata::default();
        let default_expected = VersionMetadata::default_for_existing();

        assert_eq!(metadata.created_by_tx, default_expected.created_by_tx);
        assert_eq!(metadata.commit_timestamp, default_expected.commit_timestamp);
        assert_eq!(metadata.created_by_tx, TxId::new(0));
        assert!(metadata.commit_timestamp.is_some());
    }

    #[test]
    fn test_version_metadata_debug() {
        let tx_id = TxId::new(123);
        let timestamp = Timestamp::from(456);
        let metadata = VersionMetadata::new(tx_id, timestamp);
        let debug_str = format!("{:?}", metadata);

        assert!(debug_str.contains("VersionMetadata"));
        assert!(debug_str.contains("created_by_tx"));
        assert!(debug_str.contains("commit_timestamp"));
        assert!(debug_str.contains("123"));
    }

    #[test]
    fn test_version_metadata_clone_copy() {
        let tx_id = TxId::new(123);
        let timestamp = Timestamp::from(456);
        let metadata = VersionMetadata::new(tx_id, timestamp);

        let copy = metadata; // Copy
        assert_eq!(metadata, copy);

        #[allow(clippy::clone_on_copy)]
        let clone = metadata.clone(); // Clone
        assert_eq!(metadata, clone);
    }
}

#[cfg(test)]
mod storage_tests {
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
        for item in new_embedding_sparse.iter_mut().take(38) {
            // 10% of 384
            *item = 0.9f32;
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
        for item in new_embedding_full.iter_mut().take(346) {
            // 90% of 384
            *item = 0.9f32;
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

    // ========================================================================
    // Property Cloning Performance Tests (Issue #214)
    // ========================================================================

    #[test]
    fn test_property_key_clone_is_cheap() {
        // Verify PropertyKey cloning is O(1) - just copies the InternedString ID
        // This validates the optimization from Issue #202

        let key1 = GLOBAL_INTERNER.intern("test_property").unwrap();
        let key2 = key1; // Copy (not clone, but same semantics for Copy types)

        // Both should have the same underlying ID (they're the same InternedString)
        assert_eq!(key1, key2);

        // Verify cloning many keys is fast (all O(1) copies)
        let keys: Vec<_> = (0..1000)
            .map(|i| GLOBAL_INTERNER.intern(format!("key_{}", i)).unwrap())
            .collect();

        let cloned_keys: Vec<_> = keys.to_vec();

        // All keys should be equal to their clones
        for (original, cloned) in keys.iter().zip(cloned_keys.iter()) {
            assert_eq!(original, cloned);
        }
    }

    #[test]
    fn test_property_value_clone_is_arc_refcount_increment() {
        // Verify PropertyValue cloning is O(1) - Arc refcount increment, not deep copy
        // This validates that values use Arc internally (already implemented)

        // Test String value
        let string_val =
            PropertyValue::string("A reasonably long string that would be expensive to deep copy");
        let cloned_string = string_val.clone();

        // Both should be equal and point to the same Arc
        assert_eq!(string_val, cloned_string);
        if let (PropertyValue::String(arc1), PropertyValue::String(arc2)) =
            (&string_val, &cloned_string)
        {
            // Arcs should point to the same data (same address)
            assert!(std::ptr::eq(
                arc1.as_ref() as *const str,
                arc2.as_ref() as *const str
            ));
        } else {
            panic!("Expected String variants");
        }

        // Test Vector value (large embedding)
        let large_embedding = vec![0.1f32; 1536]; // OpenAI ada-002 size
        let vector_val = PropertyValue::vector(&large_embedding);
        let cloned_vector = vector_val.clone();

        assert_eq!(vector_val, cloned_vector);
        if let (PropertyValue::Vector(arc1), PropertyValue::Vector(arc2)) =
            (&vector_val, &cloned_vector)
        {
            // Arcs should point to the same data
            assert!(std::ptr::eq(
                arc1.as_ref() as *const [f32],
                arc2.as_ref() as *const [f32]
            ));
        } else {
            panic!("Expected Vector variants");
        }

        // Test Array value
        let array_val = PropertyValue::array(vec![PropertyValue::Int(42); 100]);
        let cloned_array = array_val.clone();

        assert_eq!(array_val, cloned_array);
        if let (PropertyValue::Array(arc1), PropertyValue::Array(arc2)) =
            (&array_val, &cloned_array)
        {
            // Arcs should point to the same data
            assert!(std::ptr::eq(
                arc1.as_ref() as *const Vec<PropertyValue>,
                arc2.as_ref() as *const Vec<PropertyValue>
            ));
        } else {
            panic!("Expected Array variants");
        }
    }

    #[test]
    fn test_property_delta_from_diff_clone_overhead() {
        // Verify PropertyDelta::from_diff has minimal clone overhead
        // All clones should be cheap (InternedString ID copy + Arc refcount increment)

        let old_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("city", "NYC")
            .insert("embedding", PropertyValue::vector(vec![0.1f32; 384]))
            .build();

        let new_props = PropertyMapBuilder::new()
            .insert("name", "Alice") // Unchanged
            .insert("age", 31i64) // Changed
            .insert("country", "USA") // Added
            .insert("embedding", PropertyValue::vector(vec![0.2f32; 384])) // Changed
            // city removed
            .build();

        let delta = PropertyDelta::from_diff(&old_props, &new_props);

        // Verify delta structure
        assert_eq!(delta.changed.len(), 2); // age, country (embedding uses vector_deltas)
        assert_eq!(delta.vector_deltas.len(), 1); // embedding
        assert_eq!(delta.removed.len(), 1); // city

        // Verify cloned values in delta point to same Arc as new_props
        let age_key = GLOBAL_INTERNER.intern("age").unwrap();
        if let Some(delta_age) = delta.changed.get(&age_key)
            && let Some(new_age) = new_props.get("age")
        {
            // Should be equal
            assert_eq!(delta_age, new_age);
            // For non-Arc types like Int, this is a value comparison
            // But for Arc types, they would share the same allocation
        }

        // Verify cloning large embeddings is cheap (uses Arc)
        let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();
        assert!(delta.vector_deltas.contains_key(&embedding_key));
    }

    #[test]
    fn test_property_delta_apply_clone_overhead() {
        // Verify PropertyDelta::apply has minimal clone overhead
        // All value clones should be Arc refcount increments

        let base = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("city", "NYC")
            .insert("embedding", PropertyValue::vector(vec![0.1f32; 384]))
            .build();

        let mut delta = PropertyDelta::new();
        let age_key = GLOBAL_INTERNER.intern("age").unwrap();
        let country_key = GLOBAL_INTERNER.intern("country").unwrap();

        delta.changed.insert(age_key, PropertyValue::Int(31));
        delta
            .changed
            .insert(country_key, PropertyValue::string("USA"));

        let result = delta.apply(&base);

        // Verify result has correct values
        assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("Alice"));
        assert_eq!(result.get("age").and_then(|v| v.as_int()), Some(31.into()));
        assert_eq!(result.get("country").and_then(|v| v.as_str()), Some("USA"));
        assert_eq!(result.get("city").and_then(|v| v.as_str()), Some("NYC"));

        // Verify unchanged values share the same Arc
        if let (Some(base_name), Some(result_name)) = (base.get("name"), result.get("name"))
            && let (PropertyValue::String(base_arc), PropertyValue::String(result_arc)) =
                (base_name, result_name)
        {
            // Should point to the same Arc allocation
            assert!(std::ptr::eq(
                base_arc.as_ref() as *const str,
                result_arc.as_ref() as *const str
            ));
        }

        if let (Some(base_embedding), Some(result_embedding)) =
            (base.get("embedding"), result.get("embedding"))
            && let (PropertyValue::Vector(base_arc), PropertyValue::Vector(result_arc)) =
                (base_embedding, result_embedding)
        {
            // Should point to the same Arc allocation (unchanged)
            assert!(std::ptr::eq(
                base_arc.as_ref() as *const [f32],
                result_arc.as_ref() as *const [f32]
            ));
        }
    }

    #[test]
    fn test_property_delta_apply_edge_cases() {
        // Test edge cases for apply method

        // Case 1: Empty base
        let empty_base = PropertyMapBuilder::new().build();
        let mut delta = PropertyDelta::new();
        delta.changed.insert(
            GLOBAL_INTERNER.intern("new").unwrap(),
            PropertyValue::Int(42),
        );

        let result = delta.apply(&empty_base);
        assert_eq!(result.get("new").and_then(|v| v.as_int()), Some(42.into()));

        // Case 2: Empty delta (no changes)
        let base = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let empty_delta = PropertyDelta::new();
        let result = empty_delta.apply(&base);

        // Should be identical to base
        assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("Alice"));
        assert_eq!(result.get("age").and_then(|v| v.as_int()), Some(30.into()));

        // Case 3: Delta with only removals (no additions/changes)
        let base = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("city", "NYC")
            .build();

        let mut removal_delta = PropertyDelta::new();
        removal_delta
            .removed
            .insert(GLOBAL_INTERNER.intern("city").unwrap());

        let result = removal_delta.apply(&base);

        assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("Alice"));
        assert_eq!(result.get("age").and_then(|v| v.as_int()), Some(30.into()));
        assert!(result.get("city").is_none());

        // Case 4: Large-scale scenario (verify performance doesn't degrade)
        let mut large_base_builder = PropertyMapBuilder::new();
        for i in 0..1000 {
            large_base_builder = large_base_builder.insert(&format!("prop_{}", i), i as i64);
        }
        let large_base = large_base_builder.build();

        let mut small_delta = PropertyDelta::new();
        // Only change 1% of properties (10 out of 1000)
        for i in 0..10 {
            let key = GLOBAL_INTERNER.intern(format!("prop_{}", i)).unwrap();
            small_delta
                .changed
                .insert(key, PropertyValue::Int((i + 1000) as i64));
        }

        let result = small_delta.apply(&large_base);

        // Verify changes applied
        assert_eq!(
            result.get("prop_0").and_then(|v| v.as_int()),
            Some(1000.into())
        );
        assert_eq!(
            result.get("prop_9").and_then(|v| v.as_int()),
            Some(1009.into())
        );

        // Verify unchanged properties preserved
        assert_eq!(
            result.get("prop_10").and_then(|v| v.as_int()),
            Some(10.into())
        );
        assert_eq!(
            result.get("prop_999").and_then(|v| v.as_int()),
            Some(999.into())
        );
    }

    #[test]
    fn test_sequential_delta_application_shares_arcs() {
        // Verify that sequential delta application (hot path for time-travel)
        // maintains Arc sharing for unchanged properties

        let base = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("counter", 0i64)
            .insert("embedding", PropertyValue::vector(vec![0.1f32; 384]))
            .build();

        // Create a chain of deltas, each incrementing the counter
        let mut deltas = Vec::new();
        for i in 1..=5 {
            let mut delta = PropertyDelta::new();
            let counter_key = GLOBAL_INTERNER.intern("counter").unwrap();
            delta.changed.insert(counter_key, PropertyValue::Int(i));
            deltas.push(delta);
        }

        // Apply all deltas in sequence (typical time-travel query pattern)
        let mut current = base.clone();
        for delta in &deltas {
            current = delta.apply(&current);
        }

        // Verify final result
        assert_eq!(
            current.get("counter").and_then(|v| v.as_int()),
            Some(5.into())
        );

        // Verify unchanged properties still share Arcs with base
        if let (Some(base_name), Some(final_name)) = (base.get("name"), current.get("name"))
            && let (PropertyValue::String(base_arc), PropertyValue::String(final_arc)) =
                (base_name, final_name)
        {
            // Should still point to the same Arc allocation
            assert!(std::ptr::eq(
                base_arc.as_ref() as *const str,
                final_arc.as_ref() as *const str
            ));
        }

        if let (Some(base_embedding), Some(final_embedding)) =
            (base.get("embedding"), current.get("embedding"))
            && let (PropertyValue::Vector(base_arc), PropertyValue::Vector(final_arc)) =
                (base_embedding, final_embedding)
        {
            // Should still point to the same Arc allocation
            assert!(std::ptr::eq(
                base_arc.as_ref() as *const [f32],
                final_arc.as_ref() as *const [f32]
            ));
        }
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::{MAX_VECTOR_DIMENSIONS, PropertyMapBuilder};
    use std::sync::Arc;

    #[test]
    fn test_materialize_vector_deltas_missing_base_property() {
        let key = GLOBAL_INTERNER.intern("embedding").unwrap();
        let mut delta = PropertyDelta::new();

        // Manually insert a sparse delta that requires a base property
        // We use a dummy sparse delta
        let sparse_delta = VectorDelta::Sparse {
            dimension: 10,
            changes: std::sync::Arc::new(vec![]),
        };
        delta.vector_deltas.insert(key, sparse_delta);

        // Empty base map (missing the key)
        let base = PropertyMapBuilder::new().build();

        let result = delta.materialize_vector_deltas(&base);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("base property not found"));
    }

    #[test]
    fn test_materialize_vector_deltas_wrong_base_type() {
        let key = GLOBAL_INTERNER.intern("embedding").unwrap();
        let mut delta = PropertyDelta::new();

        let sparse_delta = VectorDelta::Sparse {
            dimension: 10,
            changes: std::sync::Arc::new(vec![]),
        };
        delta.vector_deltas.insert(key, sparse_delta);

        // Base map has the key but it's an integer, not a vector
        let base = PropertyMapBuilder::new().insert("embedding", 42i64).build();

        let result = delta.materialize_vector_deltas(&base);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("base property is not a vector"));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "VectorDelta applied to vector of wrong dimension")]
    fn test_vector_delta_apply_dimension_mismatch_panic() {
        // Create a sparse delta expecting dimension 10
        let delta = VectorDelta::Sparse {
            dimension: 10,
            changes: std::sync::Arc::new(vec![]),
        };

        // Apply to a base vector of dimension 5 (mismatch)
        let base = vec![0.0f32; 5];
        let _ = delta.apply(&base);
    }

    #[test]
    fn test_vector_delta_from_diff_max_dimensions() {
        // Create vectors exceeding the maximum allowed dimension
        let len = MAX_VECTOR_DIMENSIONS + 1;
        let v1 = vec![0.0f32; len];
        let v2 = vec![1.0f32; len];

        // Should return None due to dimension limit
        let result = VectorDelta::from_diff(&v1, &v2);
        assert!(result.is_none());
    }

    #[test]
    fn test_vector_delta_from_diff_nan_change() {
        // 💣 Risk: (a - b).abs() > EPSILON returns false if one is NaN.
        // This means changes involving NaN are silently ignored!
        let old = vec![1.0f32];
        let new = vec![f32::NAN];
        let delta = VectorDelta::from_diff(&old, &new);
        assert!(delta.is_some(), "Change from 1.0 to NaN should be detected");

        let old = vec![f32::NAN];
        let new = vec![1.0f32];
        let delta = VectorDelta::from_diff(&old, &new);
        assert!(delta.is_some(), "Change from NaN to 1.0 should be detected");

        let old = vec![1.0f32];
        let new = vec![f32::INFINITY];
        let delta = VectorDelta::from_diff(&old, &new);
        assert!(
            delta.is_some(),
            "Change from 1.0 to Infinity should be detected"
        );
    }

    #[test]
    fn test_vector_delta_apply_manual_construction_oob() {
        // 💣 Risk: Manual construction or deserialization could create invalid indices.
        // Apply should not panic or corrupt memory.
        let dimension = 10;
        let changes = std::sync::Arc::new(vec![(100, 1.0f32)]); // Index 100 > dimension 10
        let delta = VectorDelta::Sparse { dimension, changes };

        let base = vec![0.0f32; 10];
        let result = delta.apply(&base);

        // Should return base unchanged or with ignored OOB updates
        assert_eq!(result.len(), 10);
        assert_eq!(result[0], 0.0);
    }

    #[test]
    fn test_property_delta_apply_full_vector_missing_base() {
        // 💣 Risk: Applying a delta containing a Full vector to a base that misses the key
        // should NOT fail silently. Since it's a full replacement, the base isn't needed!
        // Current implementation requires base property to exist even for Full delta.

        let base = PropertyMapBuilder::new().build(); // Empty base

        let mut delta = PropertyDelta::new();
        let key = GLOBAL_INTERNER.intern("embedding").unwrap();
        let new_vec = vec![1.0f32, 2.0, 3.0];
        let vec_delta = VectorDelta::Full(Arc::from(new_vec.clone()));

        delta.vector_deltas.insert(key, vec_delta);

        let result = delta.apply(&base);

        // Expectation: The vector should be present in the result
        assert!(
            result.get("embedding").is_some(),
            "Full vector delta should be applied even if base property is missing"
        );

        if let Some(val) = result.get("embedding") {
            assert_eq!(val.as_vector(), Some(new_vec.as_slice()));
        }
    }

    #[test]
    fn test_vector_delta_vs_property_value_equality() {
        // 🧪 Strategy: Document the divergence between VectorDelta (approximate) and PropertyValue (exact).
        // VectorDelta ignores changes < epsilon and handles NaN differently than PropertyValue.

        // Case 1: Small difference (less than epsilon)
        // Use 0.0 as base to ensure the small difference is representable in f32
        // (1.0 + 1e-8 might be rounded to 1.0 due to f32 precision)
        let v1 = vec![0.0f32];
        let v2 = vec![VECTOR_EPSILON / 2.0];

        // PropertyValue uses exact equality (bitwise for f32 via PartialEq)
        let pv1 = PropertyValue::vector(&v1);
        let pv2 = PropertyValue::vector(&v2);
        assert_ne!(pv1, pv2, "PropertyValue should use exact equality");

        // VectorDelta uses approximate equality
        let delta = VectorDelta::from_diff(&v1, &v2);
        assert!(
            delta.is_none(),
            "VectorDelta should ignore changes smaller than epsilon"
        );

        // Case 2: NaN handling
        // PropertyValue: NaN != NaN
        let nan_vec = vec![f32::NAN];
        let pv_nan1 = PropertyValue::vector(&nan_vec);
        let pv_nan2 = PropertyValue::vector(&nan_vec);
        assert_ne!(pv_nan1, pv_nan2, "PropertyValue should treat NaN != NaN");

        // VectorDelta: NaN == NaN (treated as no change)
        let delta_nan = VectorDelta::from_diff(&nan_vec, &nan_vec);
        assert!(
            delta_nan.is_none(),
            "VectorDelta should treat NaN as equal to NaN (no change)"
        );

        // This confirms that PropertyDelta (which uses VectorDelta) might report "no change"
        // even when PropertyValue equality says they are different. This is a design choice
        // for storage efficiency but important to document.
    }

    #[test]
    fn test_property_delta_apply_sparse_ignored_on_missing_base() {
        // 🧪 Strategy: Verify that a sparse delta is silently ignored if the base property is missing.
        // This is a known "best effort" behavior (fail open) rather than "fail closed".
        // Documenting it with a test ensures it doesn't change unexpectedly.

        let mut delta = PropertyDelta::new();
        let key = GLOBAL_INTERNER.intern("embedding").unwrap();

        // Sparse delta: change index 0 to 1.0
        let changes = Arc::new(vec![(0, 1.0f32)]);
        let vec_delta = VectorDelta::Sparse {
            dimension: 10,
            changes,
        };

        delta.vector_deltas.insert(key, vec_delta);

        // Base property map *without* "embedding"
        let base = PropertyMapBuilder::new().insert("name", "Alice").build();

        // Apply delta
        let result = delta.apply(&base);

        // Expectation: "embedding" is missing in result (delta ignored)
        assert!(
            result.get("embedding").is_none(),
            "Sparse delta should be silently ignored if base property is missing"
        );
    }

    #[test]
    fn test_property_delta_apply_sparse_ignored_on_wrong_type() {
        // 🧪 Strategy: Verify that a sparse delta is silently ignored if the base property has wrong type.

        let mut delta = PropertyDelta::new();
        let key = GLOBAL_INTERNER.intern("embedding").unwrap();

        let changes = Arc::new(vec![(0, 1.0f32)]);
        let vec_delta = VectorDelta::Sparse {
            dimension: 10,
            changes,
        };

        delta.vector_deltas.insert(key, vec_delta);

        // Base has "embedding" but it's an Int
        let base = PropertyMapBuilder::new().insert("embedding", 42i64).build();

        // Apply delta
        let result = delta.apply(&base);

        // Expectation: "embedding" remains 42i64 (delta ignored)
        assert_eq!(
            result.get("embedding").and_then(|v| v.as_int()),
            Some(42),
            "Sparse delta should be silently ignored if base property is wrong type"
        );
    }

    #[test]
    fn test_property_delta_silently_ignores_dimension_change() {
        // 🧪 Strategy: Verify that dimension changes in vectors are treated as full updates
        // even if sparse delta returns None (which it does for dimension mismatch).
        //
        // This was a bug found by Elenchus where dimension changes were silently ignored.

        // 1. Create base with 2D vector
        let old_vec = vec![1.0f32, 2.0];
        let old_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&old_vec))
            .build();

        // 2. Create new with 3D vector (dimension change)
        let new_vec = vec![1.0f32, 2.0, 3.0];
        let new_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&new_vec))
            .build();

        // 3. Create delta
        let delta = PropertyDelta::from_diff(&old_props, &new_props);

        // 4. Apply delta to old
        let applied = delta.apply(&old_props);

        // 5. Check if update was applied
        let applied_vec = applied.get("embedding").unwrap().as_vector().unwrap();

        assert_eq!(
            applied_vec.len(),
            3,
            "PropertyDelta should apply full update on dimension change"
        );
        assert_eq!(applied_vec, &new_vec[..]);
    }

    #[test]
    fn test_property_delta_handles_nan_no_change() {
        // 🎯 Target: PropertyDelta::from_diff with NaN values
        // 💣 Risk: NaN != NaN could cause spurious delta entries
        // 🧪 Strategy: Create PropertyMap with NaN, create delta to same map.
        // 🔬 Verification: Delta should be empty.

        let nan_val = PropertyValue::Float(f64::NAN);
        let props = PropertyMapBuilder::new().insert("val", nan_val).build();

        let delta = PropertyDelta::from_diff(&props, &props);
        assert!(delta.is_empty(), "NaN -> NaN should result in empty delta");
    }

    #[test]
    fn test_sentry_floats_epsilon_boundary() {
        // 🛡️ Sentry Test: Verify floats_approx_equal handles exact epsilon difference as equal.
        // This targets mutants that replace `<=` with `<` in (a-b).abs() <= VECTOR_EPSILON.

        let old = vec![0.0f32];
        let new = vec![VECTOR_EPSILON]; // Exact epsilon difference

        // Should be considered equal, so no delta
        let delta = VectorDelta::from_diff(&old, &new);
        assert!(
            delta.is_none(),
            "Difference of exactly EPSILON should be treated as equal"
        );
    }

    #[test]
    fn test_sentry_floats_infinite_equality() {
        // 🛡️ Sentry Test: Verify floats_approx_equal handles infinity equality correctly.
        // This targets mutants that remove the explicit `is_infinite` check.
        // (INF - INF) is NaN, and NaN <= EPSILON is false, so without check they would be unequal.

        let old = vec![f32::INFINITY];
        let new = vec![f32::INFINITY];

        let delta = VectorDelta::from_diff(&old, &new);
        assert!(delta.is_none(), "Infinity == Infinity should be no change");
    }

    #[test]
    fn test_materialize_vector_deltas_success() {
        // 🛡️ Sentry Test: Verify success path of materialize_vector_deltas.
        // This targets mutants that might empty the function body or loop, causing data loss.
        // If materialization fails silently, sparse deltas are not persisted!

        let mut delta = PropertyDelta::new();
        let key = GLOBAL_INTERNER.intern("embedding").unwrap();

        // Create a sparse delta: change index 0 to 1.0
        // Base vector size: 10
        let changes = std::sync::Arc::new(vec![(0, 1.0f32)]);
        let vec_delta = VectorDelta::Sparse {
            dimension: 10,
            changes,
        };

        delta.vector_deltas.insert(key, vec_delta);

        // Base property map with valid vector
        let base_vec = vec![0.0f32; 10];
        let base = PropertyMapBuilder::new()
            .insert_vector("embedding", &base_vec)
            .build();

        // Perform materialization
        let result = delta.materialize_vector_deltas(&base);
        assert!(result.is_ok(), "Materialization should succeed");

        // Verify side effects
        assert!(
            delta.vector_deltas.is_empty(),
            "vector_deltas should be empty after materialization"
        );
        assert!(
            delta.changed.contains_key(&key),
            "changed should contain the materialized vector"
        );

        // Verify the value in changed is correct (should be full vector)
        let materialized = delta.changed.get(&key).unwrap();
        if let PropertyValue::Vector(v) = materialized {
            assert_eq!(v.len(), 10);
            assert_eq!(v[0], 1.0f32); // Applied change
            assert_eq!(v[1], 0.0f32); // Unchanged
        } else {
            panic!("Materialized value should be a Vector");
        }
    }

    #[test]
    fn test_vector_delta_apply_empty_changes() {
        // 🛡️ Sentry Test: Applying sparse delta with no changes should return base vector unchanged.
        let delta = VectorDelta::Sparse {
            dimension: 5,
            changes: Arc::new(vec![]),
        };
        let base = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let result = delta.apply(&base);
        assert_eq!(result, base);
    }

    #[test]
    fn test_vector_delta_apply_out_of_bounds() {
        // 🛡️ Sentry Test: Verify out-of-bounds indices are ignored safely.
        let delta = VectorDelta::Sparse {
            dimension: 3,
            changes: Arc::new(vec![
                (0, 10.0), // Valid
                (5, 20.0), // Invalid index
            ]),
        };
        let base = vec![1.0, 2.0, 3.0];
        let result = delta.apply(&base);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0], 10.0); // Updated
        assert_eq!(result[1], 2.0); // Unchanged
        assert_eq!(result[2], 3.0); // Unchanged
    }
}

#[cfg(test)]
mod mutant_kill_tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::TIMESTAMP_MAX;
    use std::sync::Arc;

    fn make_node_anchor() -> NodeVersion {
        let props = PropertyMapBuilder::new().insert("name", "node").build();
        NodeVersion::new_anchor(
            VersionId::new(10).unwrap(),
            NodeId::new(11).unwrap(),
            BiTemporalInterval::current(1_000.into()),
            GLOBAL_INTERNER.intern("Node").unwrap(),
            props,
        )
    }

    fn make_edge_anchor() -> EdgeVersion {
        let props = PropertyMapBuilder::new().insert("weight", 1i64).build();
        EdgeVersion::new_anchor(
            VersionId::new(20).unwrap(),
            EdgeId::new(21).unwrap(),
            BiTemporalInterval::current(1_000.into()),
            GLOBAL_INTERNER.intern("EDGE").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            props,
        )
    }

    fn make_edge_delta() -> EdgeVersion {
        let old_props = PropertyMapBuilder::new().insert("weight", 1i64).build();
        let new_props = PropertyMapBuilder::new().insert("weight", 2i64).build();
        EdgeVersion::new_delta(
            VersionId::new(22).unwrap(),
            EdgeId::new(23).unwrap(),
            BiTemporalInterval::current(2_000.into()),
            GLOBAL_INTERNER.intern("EDGE").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            &old_props,
            &new_props,
            VersionId::new(20).unwrap(),
        )
    }

    #[test]
    fn test_vector_delta_from_diff_allows_exact_max_dimensions() {
        let len = MAX_VECTOR_DIMENSIONS;
        let old = vec![0.0f32; len];
        let mut new = old.clone();
        new[0] = 1.0;

        let delta = VectorDelta::from_diff(&old, &new);
        assert!(delta.is_some(), "Max dimension should be allowed");
    }

    #[test]
    fn test_vector_delta_from_diff_threshold_behavior_for_sparse_vs_full() {
        // Boundary: changes * 2 == dimension should be full storage.
        let old_full = vec![0.0f32; 4];
        let mut new_full = old_full.clone();
        new_full[0] = 1.0;
        new_full[1] = 2.0;
        let delta_full = VectorDelta::from_diff(&old_full, &new_full).unwrap();
        assert!(
            matches!(delta_full, VectorDelta::Full(_)),
            "Threshold boundary should choose full storage"
        );

        // One change in 3 dimensions should be sparse.
        let old_sparse = vec![0.0f32; 3];
        let mut new_sparse = old_sparse.clone();
        new_sparse[0] = 1.0;
        let delta_sparse = VectorDelta::from_diff(&old_sparse, &new_sparse).unwrap();
        assert!(
            matches!(delta_sparse, VectorDelta::Sparse { .. }),
            "Few changes should choose sparse storage"
        );
    }

    #[test]
    fn test_vector_delta_apply_ignores_index_equal_to_length() {
        let base = vec![0.0f32, 1.0, 2.0];
        let delta = VectorDelta::Sparse {
            dimension: 3,
            changes: Arc::new(vec![(3, 99.0)]),
        };

        let result = delta.apply(&base);
        assert_eq!(result, base);
    }

    #[test]
    fn test_vector_delta_sparse_estimated_heap_size_matches_formula() {
        let mut changes = Vec::with_capacity(4);
        changes.push((0u32, 1.0f32));
        changes.push((3u32, 2.0f32));
        let delta = VectorDelta::Sparse {
            dimension: 8,
            changes: Arc::new(changes),
        };

        let expected = match &delta {
            VectorDelta::Sparse { changes, .. } => {
                changes.capacity() * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>())
            }
            _ => unreachable!(),
        };
        assert_eq!(delta.estimated_heap_size(), expected);
    }

    #[test]
    fn test_vector_delta_partial_eq_semantics() {
        let sparse_a = VectorDelta::Sparse {
            dimension: 4,
            changes: Arc::new(vec![(1, 0.5), (3, 1.0)]),
        };
        let sparse_b = VectorDelta::Sparse {
            dimension: 4,
            changes: Arc::new(vec![(1, 0.5), (3, 1.0)]),
        };
        assert_eq!(sparse_a, sparse_b);

        let sparse_dim_mismatch = VectorDelta::Sparse {
            dimension: 5,
            changes: Arc::new(vec![(1, 0.5), (3, 1.0)]),
        };
        assert_ne!(sparse_a, sparse_dim_mismatch);

        let sparse_len_mismatch = VectorDelta::Sparse {
            dimension: 4,
            changes: Arc::new(vec![(1, 0.5)]),
        };
        assert_ne!(sparse_a, sparse_len_mismatch);

        let sparse_idx_mismatch = VectorDelta::Sparse {
            dimension: 4,
            changes: Arc::new(vec![(0, 0.5), (3, 1.0)]),
        };
        assert_ne!(sparse_a, sparse_idx_mismatch);

        let sparse_val_mismatch = VectorDelta::Sparse {
            dimension: 4,
            changes: Arc::new(vec![(1, 0.5 + 1e-3), (3, 1.0)]),
        };
        assert_ne!(sparse_a, sparse_val_mismatch);

        let full_a = VectorDelta::Full(Arc::from(vec![1.0f32, 2.0f32]));
        let full_b = VectorDelta::Full(Arc::from(vec![1.0f32, 2.0f32]));
        assert_eq!(full_a, full_b);

        let full_len_mismatch = VectorDelta::Full(Arc::from(vec![1.0f32]));
        assert_ne!(full_a, full_len_mismatch);

        let full_val_mismatch = VectorDelta::Full(Arc::from(vec![1.0f32, 2.5f32]));
        assert_ne!(full_a, full_val_mismatch);

        assert_ne!(sparse_a, full_a);
    }

    #[test]
    fn test_temporal_version_close_transaction_time_updates_tx_dimension() {
        let mut node = make_node_anchor();
        let end = Timestamp::from(2_000);

        node.close_transaction_time(end).unwrap();

        assert_eq!(node.temporal().transaction_time().end(), end);
        assert_eq!(node.temporal().valid_time().end(), TIMESTAMP_MAX);
    }

    #[test]
    fn test_property_delta_is_empty_only_when_all_collections_empty() {
        let key_a = GLOBAL_INTERNER.intern("a").unwrap();
        let key_v = GLOBAL_INTERNER.intern("v").unwrap();
        let key_r = GLOBAL_INTERNER.intern("r").unwrap();

        let mut changed_only = PropertyDelta::new();
        changed_only.changed.insert(key_a, PropertyValue::Int(1));
        assert!(!changed_only.is_empty());

        let mut vector_only = PropertyDelta::new();
        vector_only.vector_deltas.insert(
            key_v,
            VectorDelta::Sparse {
                dimension: 2,
                changes: Arc::new(vec![(0, 1.0)]),
            },
        );
        assert!(!vector_only.is_empty());

        let mut removed_only = PropertyDelta::new();
        removed_only.removed.insert(key_r);
        assert!(!removed_only.is_empty());

        assert!(PropertyDelta::new().is_empty());
    }

    #[test]
    fn test_property_delta_estimated_heap_size_matches_formula() {
        let mut delta = PropertyDelta::new();
        let key_name = GLOBAL_INTERNER.intern("name").unwrap();
        let key_vec = GLOBAL_INTERNER.intern("embedding").unwrap();
        let key_removed = GLOBAL_INTERNER.intern("old").unwrap();

        delta
            .changed
            .insert(key_name, PropertyValue::string("Alice"));
        delta.vector_deltas.insert(
            key_vec,
            VectorDelta::Sparse {
                dimension: 4,
                changes: Arc::new(vec![(1, 2.0)]),
            },
        );
        delta.removed.insert(key_removed);

        let expected_changed_overhead = delta.changed.capacity()
            * (std::mem::size_of::<PropertyKey>() + std::mem::size_of::<PropertyValue>() + 8);
        let expected_changed_values: usize = delta
            .changed
            .values()
            .map(PropertyValue::estimated_heap_size)
            .sum();

        let expected_vector_overhead = delta.vector_deltas.capacity()
            * (std::mem::size_of::<PropertyKey>() + std::mem::size_of::<VectorDelta>() + 8);
        let expected_vector_values: usize = delta
            .vector_deltas
            .values()
            .map(VectorDelta::estimated_heap_size)
            .sum();

        let expected_removed_overhead =
            delta.removed.capacity() * (std::mem::size_of::<PropertyKey>() + 8);

        let expected = expected_changed_overhead
            + expected_changed_values
            + expected_vector_overhead
            + expected_vector_values
            + expected_removed_overhead;

        assert_eq!(delta.estimated_heap_size(), expected);
    }

    #[test]
    fn test_node_and_edge_estimated_size_match_formula() {
        let node = make_node_anchor();
        assert_eq!(
            node.estimated_size(),
            std::mem::size_of::<NodeVersion>() + node.data.estimated_heap_size()
        );

        let edge = make_edge_anchor();
        assert_eq!(
            edge.estimated_size(),
            std::mem::size_of::<EdgeVersion>() + edge.data.estimated_heap_size()
        );
    }

    #[test]
    fn test_edge_anchor_reports_not_delta() {
        let edge = make_edge_anchor();
        assert!(!edge.is_delta());
    }

    #[test]
    fn test_entity_version_trait_round_trip_links_for_node_and_edge() {
        fn set_links<V: EntityVersion>(
            v: &mut V,
            prev: Option<VersionId>,
            next: Option<VersionId>,
        ) {
            v.set_prev_version(prev);
            v.set_next_version(next);
        }
        fn links<V: EntityVersion>(v: &V) -> (Option<VersionId>, Option<VersionId>) {
            (v.prev_version(), v.next_version())
        }

        let mut node = make_node_anchor();
        let node_prev = Some(VersionId::new(99).unwrap());
        let node_next = Some(VersionId::new(100).unwrap());
        set_links(&mut node, node_prev, node_next);
        assert_eq!(links(&node), (node_prev, node_next));
        set_links(&mut node, None, None);
        assert_eq!(links(&node), (None, None));

        let mut edge = make_edge_anchor();
        let edge_prev = Some(VersionId::new(199).unwrap());
        let edge_next = Some(VersionId::new(200).unwrap());
        set_links(&mut edge, edge_prev, edge_next);
        assert_eq!(links(&edge), (edge_prev, edge_next));
        set_links(&mut edge, None, None);
        assert_eq!(links(&edge), (None, None));
    }

    #[test]
    fn test_entity_version_trait_is_anchor_for_edge_variants() {
        fn trait_is_anchor<V: EntityVersion>(v: &V) -> bool {
            v.is_anchor()
        }

        let edge_anchor = make_edge_anchor();
        let edge_delta = make_edge_delta();

        assert!(trait_is_anchor(&edge_anchor));
        assert!(!trait_is_anchor(&edge_delta));
    }

    #[test]
    fn test_vector_delta_epsilon_boundary() {
        // 🛡️ Sentinel Test: Verify exact epsilon handling in VectorDelta.
        // This targets mutants that replace `<=` with `<` in floats_approx_equal.
        // (a - b).abs() <= VECTOR_EPSILON

        // A small offset to test boundaries around VECTOR_EPSILON.
        const SMALL_OFFSET: f32 = 1e-9;

        // Use 0.0 as base to ensure EPSILON is exactly representable and addition
        // doesn't lose precision (which happens around 1.0 due to f32::EPSILON > VECTOR_EPSILON).
        let v1 = vec![0.0f32];

        // Case 1: Difference exactly equal to EPSILON
        // 0.0 + 1e-7. abs(diff) = 1e-7.
        // Original (<=): Equal -> No Delta (None)
        // Mutant (<): Not Equal -> Delta (Some)
        let v2 = vec![VECTOR_EPSILON];
        assert!(
            VectorDelta::from_diff(&v1, &v2).is_none(),
            "Difference of exactly EPSILON should be considered equal (no delta)"
        );

        // Case 2: Difference slightly larger than EPSILON
        // Should be detected as different
        let v3 = vec![VECTOR_EPSILON + SMALL_OFFSET];
        assert!(
            VectorDelta::from_diff(&v1, &v3).is_some(),
            "Difference > EPSILON should be detected"
        );

        // Case 3: Difference slightly smaller than EPSILON
        // Should be considered equal
        let v4 = vec![VECTOR_EPSILON - SMALL_OFFSET];
        assert!(
            VectorDelta::from_diff(&v1, &v4).is_none(),
            "Difference < EPSILON should be considered equal"
        );
    }
}
