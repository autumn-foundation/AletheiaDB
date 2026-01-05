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
use crate::core::interning::{InternedString, GLOBAL_INTERNER};
use crate::core::property::{PropertyKey, PropertyMap, PropertyValue};
use crate::core::temporal::{BiTemporalInterval, Timestamp};
use std::collections::{HashMap, HashSet};

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
        VersionMetadata {
            created_by_tx: TxId::new(0),
            commit_timestamp: Some(0),
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
/// efficient storage of temporal data.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyDelta {
    /// Properties that were added or modified
    pub changed: HashMap<PropertyKey, PropertyValue>,
    /// Properties that were removed
    pub removed: HashSet<PropertyKey>,
}

impl PropertyDelta {
    /// Create a new empty delta.
    pub fn new() -> Self {
        PropertyDelta {
            changed: HashMap::new(),
            removed: HashSet::new(),
        }
    }

    /// Create a delta by comparing two property maps.
    ///
    /// Returns the changes needed to transform `old` into `new`.
    pub fn from_diff(old: &PropertyMap, new: &PropertyMap) -> Self {
        let mut delta = PropertyDelta::new();

        // Find added and modified properties
        for (key, new_value) in new.iter() {
            match old.get_by_interned_key(key) {
                Some(old_value) if old_value == new_value => {
                    // Unchanged, skip
                }
                _ => {
                    // Added or modified
                    delta.changed.insert(key.clone(), new_value.clone());
                }
            }
        }

        // Find removed properties
        for key in old.keys() {
            if !new.contains_interned_key(key) {
                delta.removed.insert(key.clone());
            }
        }

        delta
    }

    /// Apply this delta to a property map, producing a new map.
    pub fn apply(&self, base: &PropertyMap) -> PropertyMap {
        // Clone the base map using the builder to get a mutable version
        let mut builder = base.clone().builder();

        // Apply changes
        for (key, value) in &self.changed {
            builder = builder.insert(key.clone(), value.clone());
        }

        // Apply removals
        for key in &self.removed {
            builder = builder.remove_by_key(key);
        }

        builder.build()
    }

    /// Returns true if this delta has no changes.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
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
        VersionData::Anchor { properties }
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
}

impl TemporalVersion for EdgeVersion {
    fn temporal(&self) -> &BiTemporalInterval {
        &self.temporal
    }

    fn temporal_mut(&mut self) -> &mut BiTemporalInterval {
        &mut self.temporal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(delta.removed.contains(&GLOBAL_INTERNER.intern("city")));
    }

    #[test]
    fn test_property_delta_apply() {
        let base = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let mut delta = PropertyDelta::new();
        delta
            .changed
            .insert(GLOBAL_INTERNER.intern("age"), PropertyValue::Int(31));
        delta
            .changed
            .insert(GLOBAL_INTERNER.intern("city"), PropertyValue::string("NYC"));

        let result = delta.apply(&base);

        assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("Alice"));
        assert_eq!(result.get("age").and_then(|v| v.as_int()), Some(31));
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

        let temporal = BiTemporalInterval::current(1000);

        let version = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(10).unwrap(),
            temporal,
            crate::core::interning::GLOBAL_INTERNER.intern("Person"),
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

        let temporal = BiTemporalInterval::current(2000);

        let version = EdgeVersion::new_delta(
            VersionId::new(2).unwrap(),
            EdgeId::new(20).unwrap(),
            temporal,
            crate::core::interning::GLOBAL_INTERNER.intern("KNOWS"),
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
}
