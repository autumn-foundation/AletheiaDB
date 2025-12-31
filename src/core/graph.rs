//! Core graph structures for nodes and edges.
//!
//! This module defines the fundamental graph elements that make up GallifreyDB's
//! current state. These structures are optimized for the "hot path" - fast access
//! to current data without temporal overhead.

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::PropertyMap;
use crate::storage::VersionMetadata;

/// A node in the current state of the graph.
///
/// This represents the current version of a node, optimized for fast access.
/// Historical versions are stored separately in the temporal storage layer.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Unique identifier for this node.
    pub id: NodeId,
    /// Label/type of the node (interned for memory efficiency).
    pub label: InternedString,
    /// Current properties of the node (Arc-based for sharing).
    pub properties: PropertyMap,
    /// ID of the current version in the historical storage.
    pub current_version: VersionId,
    /// Transaction metadata for Snapshot Isolation.
    pub metadata: VersionMetadata,
}

impl Node {
    /// Create a new node with the given ID, label, and properties.
    pub fn new(
        id: NodeId,
        label: InternedString,
        properties: PropertyMap,
        current_version: VersionId,
    ) -> Self {
        Node {
            id,
            label,
            properties,
            current_version,
            metadata: VersionMetadata::default(),
        }
    }

    /// Create a new node with explicit metadata (for transactions).
    pub fn with_metadata(
        id: NodeId,
        label: InternedString,
        properties: PropertyMap,
        current_version: VersionId,
        metadata: VersionMetadata,
    ) -> Self {
        Node {
            id,
            label,
            properties,
            current_version,
            metadata,
        }
    }

    /// Get a property value by key.
    #[inline]
    pub fn get_property(&self, key: &str) -> Option<&crate::core::property::PropertyValue> {
        self.properties.get(key)
    }

    /// Check if this node has a specific label.
    #[inline]
    pub fn has_label(&self, label: InternedString) -> bool {
        self.label == label
    }
}

/// An edge in the current state of the graph.
///
/// Edges are directed relationships between nodes with properties.
/// This represents the current version, optimized for fast traversals.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    /// Unique identifier for this edge.
    pub id: EdgeId,
    /// Label/type of the edge (interned for memory efficiency).
    pub label: InternedString,
    /// Source node ID.
    pub source: NodeId,
    /// Target node ID.
    pub target: NodeId,
    /// Current properties of the edge (Arc-based for sharing).
    pub properties: PropertyMap,
    /// ID of the current version in the historical storage.
    pub current_version: VersionId,
    /// Transaction metadata for Snapshot Isolation.
    pub metadata: VersionMetadata,
}

impl Edge {
    /// Create a new edge with the given parameters.
    pub fn new(
        id: EdgeId,
        label: InternedString,
        source: NodeId,
        target: NodeId,
        properties: PropertyMap,
        current_version: VersionId,
    ) -> Self {
        Edge {
            id,
            label,
            source,
            target,
            properties,
            current_version,
            metadata: VersionMetadata::default(),
        }
    }

    /// Create a new edge with explicit metadata (for transactions).
    pub fn with_metadata(
        id: EdgeId,
        label: InternedString,
        source: NodeId,
        target: NodeId,
        properties: PropertyMap,
        current_version: VersionId,
        metadata: VersionMetadata,
    ) -> Self {
        Edge {
            id,
            label,
            source,
            target,
            properties,
            current_version,
            metadata,
        }
    }

    /// Get a property value by key.
    #[inline]
    pub fn get_property(&self, key: &str) -> Option<&crate::core::property::PropertyValue> {
        self.properties.get(key)
    }

    /// Check if this edge has a specific label.
    #[inline]
    pub fn has_label(&self, label: InternedString) -> bool {
        self.label == label
    }

    /// Check if this edge connects the given source and target nodes.
    #[inline]
    pub fn connects(&self, source: NodeId, target: NodeId) -> bool {
        self.source == source && self.target == target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_node_creation() {
        let label = GLOBAL_INTERNER.intern("Person");
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node = Node::new(NodeId::new(1), label, props, VersionId::new(100));

        assert_eq!(node.id, NodeId::new(1));
        assert_eq!(node.label, label);
        assert_eq!(node.current_version, VersionId::new(100));
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(30));
    }

    #[test]
    fn test_node_has_label() {
        let label = GLOBAL_INTERNER.intern("Person");
        let other_label = GLOBAL_INTERNER.intern("Company");

        let node = Node::new(
            NodeId::new(1),
            label,
            PropertyMapBuilder::new().build(),
            VersionId::new(1),
        );

        assert!(node.has_label(label));
        assert!(!node.has_label(other_label));
    }

    #[test]
    fn test_edge_creation() {
        let label = GLOBAL_INTERNER.intern("KNOWS");
        let props = PropertyMapBuilder::new().insert("since", 2020i64).build();

        let edge = Edge::new(
            EdgeId::new(1),
            label,
            NodeId::new(1),
            NodeId::new(2),
            props,
            VersionId::new(100),
        );

        assert_eq!(edge.id, EdgeId::new(1));
        assert_eq!(edge.label, label);
        assert_eq!(edge.source, NodeId::new(1));
        assert_eq!(edge.target, NodeId::new(2));
        assert_eq!(edge.current_version, VersionId::new(100));
        assert_eq!(
            edge.get_property("since").and_then(|v| v.as_int()),
            Some(2020)
        );
    }

    #[test]
    fn test_edge_connects() {
        let edge = Edge::new(
            EdgeId::new(1),
            GLOBAL_INTERNER.intern("KNOWS"),
            NodeId::new(1),
            NodeId::new(2),
            PropertyMapBuilder::new().build(),
            VersionId::new(1),
        );

        assert!(edge.connects(NodeId::new(1), NodeId::new(2)));
        assert!(!edge.connects(NodeId::new(2), NodeId::new(1)));
        assert!(!edge.connects(NodeId::new(1), NodeId::new(3)));
    }
}
