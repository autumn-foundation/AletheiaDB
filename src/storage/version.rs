//! Version management for temporal storage.
//!
//! This module implements the version chain structures that enable time-traveling
//! queries. Each node and edge can have multiple versions over time, linked together
//! in a chain ordered by transaction time.
//!
//! The anchor+delta compression strategy is used to minimize storage overhead:
//! - Anchors: Full snapshots of state (created periodically)
//! - Deltas: Only the changed properties since the previous version

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::temporal::BiTemporalInterval;
use std::collections::{HashMap, HashSet};

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
    pub changed: HashMap<String, PropertyValue>,
    /// Properties that were removed
    pub removed: HashSet<String>,
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
            match old.get(key) {
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
            if !new.contains_key(key) {
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
            builder = builder.remove(key);
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
        assert!(delta.removed.contains("city"));
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
            .insert("age".to_string(), PropertyValue::Int(31));
        delta
            .changed
            .insert("city".to_string(), PropertyValue::string("NYC"));

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
            VersionId::new(1),
            NodeId::new(10),
            temporal,
            crate::core::interning::GLOBAL_INTERNER.intern("Person"),
            props,
        );

        assert!(version.is_anchor());
        assert!(!version.is_delta());
        assert_eq!(version.node_id, NodeId::new(10));
    }

    #[test]
    fn test_edge_version_delta() {
        let old_props = PropertyMapBuilder::new().insert("weight", 1i64).build();

        let new_props = PropertyMapBuilder::new().insert("weight", 2i64).build();

        let temporal = BiTemporalInterval::current(2000);

        let version = EdgeVersion::new_delta(
            VersionId::new(2),
            EdgeId::new(20),
            temporal,
            crate::core::interning::GLOBAL_INTERNER.intern("KNOWS"),
            NodeId::new(1),
            NodeId::new(2),
            &old_props,
            &new_props,
            VersionId::new(1),
        );

        assert!(!version.is_anchor());
        assert!(version.is_delta());
        assert_eq!(version.prev_version, Some(VersionId::new(1)));
    }
}
