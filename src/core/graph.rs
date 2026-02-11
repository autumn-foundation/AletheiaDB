//! Core graph structures for nodes and edges.
//!
//! This module defines the fundamental graph elements that make up AletheiaDB's
//! current state. These structures are optimized for the "hot path" - fast access
//! to current data without temporal overhead.

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::PropertyMap;
use crate::core::version::VersionMetadata;

#[inline]
fn matches_label(label_id: InternedString, label: &str) -> bool {
    use crate::core::interning::GLOBAL_INTERNER;
    GLOBAL_INTERNER
        .resolve_with(label_id, |interned| interned == label)
        .unwrap_or(false)
}

/// A node in the current state of the graph.
///
/// This represents the current version of a node, optimized for fast access.
/// Historical versions are stored separately in the temporal storage layer.
#[derive(Clone, PartialEq)]
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
    /// Check if this node has a specific label using a string.
    ///
    /// This is a convenience method that accepts a `&str` instead of requiring
    /// the caller to pre-intern the string. It compares against the existing
    /// interned label and does NOT add the input string to the interner.
    ///
    /// # Performance Note
    ///
    /// For performance-critical code paths (e.g., tight loops checking many labels),
    /// prefer pre-interning the label once and using [`has_label`](Self::has_label)
    /// instead. This method has the overhead of a HashMap lookup on each call.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Convenient for one-off checks
    /// if node.has_label_str("Person") {
    ///     // ...
    /// }
    ///
    /// // For performance-critical loops, pre-intern:
    /// let person_label = GLOBAL_INTERNER.intern("Person")?;
    /// for node in many_nodes {
    ///     if node.has_label(person_label) {  // Faster!
    ///         // ...
    ///     }
    /// }
    /// ```
    #[inline]
    pub fn has_label_str(&self, label: &str) -> bool {
        matches_label(self.label, label)
    }
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label_str = crate::core::interning::GLOBAL_INTERNER
            .resolve_with(self.label, |s| s.to_string())
            .unwrap_or_else(|| format!("{:?}", self.label));

        f.debug_struct("Node")
            .field("id", &self.id)
            .field("label", &label_str)
            .field("properties", &self.properties)
            .field("current_version", &self.current_version)
            .field("metadata", &self.metadata)
            .finish()
    }
}

/// An edge in the current state of the graph.
///
/// Edges are directed relationships between nodes with properties.
/// This represents the current version, optimized for fast traversals.
#[derive(Clone, PartialEq)]
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
    /// Check if this edge has a specific label using a string.
    ///
    /// This is a convenience method that accepts a `&str` instead of requiring
    /// the caller to pre-intern the string. It compares against the existing
    /// interned label and does NOT add the input string to the interner.
    ///
    /// # Performance Note
    ///
    /// For performance-critical code paths (e.g., tight loops checking many labels),
    /// prefer pre-interning the label once and using [`has_label`](Self::has_label)
    /// instead. This method has the overhead of a HashMap lookup on each call.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Convenient for one-off checks
    /// if edge.has_label_str("KNOWS") {
    ///     // ...
    /// }
    ///
    /// // For performance-critical loops, pre-intern:
    /// let knows_label = GLOBAL_INTERNER.intern("KNOWS")?;
    /// for edge in many_edges {
    ///     if edge.has_label(knows_label) {  // Faster!
    ///         // ...
    ///     }
    /// }
    /// ```
    #[inline]
    pub fn has_label_str(&self, label: &str) -> bool {
        matches_label(self.label, label)
    }

    /// Check if this edge connects the given source and target nodes.
    #[inline]
    pub fn connects(&self, source: NodeId, target: NodeId) -> bool {
        self.source == source && self.target == target
    }
}

impl std::fmt::Debug for Edge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label_str = crate::core::interning::GLOBAL_INTERNER
            .resolve_with(self.label, |s| s.to_string())
            .unwrap_or_else(|| format!("{:?}", self.label));

        f.debug_struct("Edge")
            .field("id", &self.id)
            .field("label", &label_str)
            .field("source", &self.source)
            .field("target", &self.target)
            .field("properties", &self.properties)
            .field("current_version", &self.current_version)
            .field("metadata", &self.metadata)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_node_creation() {
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            props,
            VersionId::new(100).unwrap(),
        );

        assert_eq!(node.id, NodeId::new(1).unwrap());
        assert_eq!(node.label, label);
        assert_eq!(node.current_version, VersionId::new(100).unwrap());
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(30));
    }

    #[test]
    fn test_node_has_label() {
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let other_label = GLOBAL_INTERNER.intern("Company").unwrap();

        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        assert!(node.has_label(label));
        assert!(!node.has_label(other_label));
    }

    #[test]
    fn test_edge_creation() {
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let props = PropertyMapBuilder::new().insert("since", 2020i64).build();

        let edge = Edge::new(
            EdgeId::new(1).unwrap(),
            label,
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            props,
            VersionId::new(100).unwrap(),
        );

        assert_eq!(edge.id, EdgeId::new(1).unwrap());
        assert_eq!(edge.label, label);
        assert_eq!(edge.source, NodeId::new(1).unwrap());
        assert_eq!(edge.target, NodeId::new(2).unwrap());
        assert_eq!(edge.current_version, VersionId::new(100).unwrap());
        assert_eq!(
            edge.get_property("since").and_then(|v| v.as_int()),
            Some(2020)
        );
    }

    #[test]
    fn test_edge_connects() {
        let edge = Edge::new(
            EdgeId::new(1).unwrap(),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        assert!(edge.connects(NodeId::new(1).unwrap(), NodeId::new(2).unwrap()));
        assert!(!edge.connects(NodeId::new(2).unwrap(), NodeId::new(1).unwrap()));
        assert!(!edge.connects(NodeId::new(1).unwrap(), NodeId::new(3).unwrap()));
    }

    #[test]
    fn test_node_has_label_str() {
        // Create a node with a "Person" label
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        // Test 1: Should return true for matching label (already interned)
        assert!(
            node.has_label_str("Person"),
            "Should return true for matching label"
        );

        // Test 2: Should return false for non-matching label (but previously interned)
        GLOBAL_INTERNER.intern("Company").unwrap();
        assert!(
            !node.has_label_str("Company"),
            "Should return false for non-matching label"
        );

        // Test 3: Should return false for label that was never interned
        // This is the key behavior - we don't pollute the interner
        assert!(
            !node.has_label_str("NeverInterned"),
            "Should return false for label that was never interned"
        );

        // Test 4: Verify the interner was NOT polluted
        assert!(
            GLOBAL_INTERNER.get_id("NeverInterned").is_none(),
            "Interner should not contain 'NeverInterned' after has_label_str call"
        );
    }

    #[test]
    fn test_edge_has_label_str() {
        // Create an edge with a "KNOWS" label
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let edge = Edge::new(
            EdgeId::new(1).unwrap(),
            label,
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        // Test 1: Should return true for matching label (already interned)
        assert!(
            edge.has_label_str("KNOWS"),
            "Should return true for matching label"
        );

        // Test 2: Should return false for non-matching label (but previously interned)
        GLOBAL_INTERNER.intern("LIKES").unwrap();
        assert!(
            !edge.has_label_str("LIKES"),
            "Should return false for non-matching label"
        );

        // Test 3: Should return false for label that was never interned
        // This is the key behavior - we don't pollute the interner
        assert!(
            !edge.has_label_str("NeverInternedEdge"),
            "Should return false for label that was never interned"
        );

        // Test 4: Verify the interner was NOT polluted
        assert!(
            GLOBAL_INTERNER.get_id("NeverInternedEdge").is_none(),
            "Interner should not contain 'NeverInternedEdge' after has_label_str call"
        );
    }

    #[test]
    fn test_node_debug() {
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            PropertyMapBuilder::new().insert("name", "Alice").build(),
            VersionId::new(1).unwrap(),
        );

        let debug_str = format!("{:?}", node);
        assert!(
            debug_str.contains("Person"),
            "Debug output should contain resolved label"
        );
        assert!(
            debug_str.contains("Alice"),
            "Debug output should contain property value"
        );
    }

    #[test]
    fn test_node_debug_fallback() {
        // Create a Node with a raw InternedString that doesn't exist in the interner
        // InternedString(u32::MAX) is extremely unlikely to exist
        let raw_label = InternedString::from_raw(u32::MAX);
        let node = Node::new(
            NodeId::new(99).unwrap(),
            raw_label,
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        let debug_str = format!("{:?}", node);
        // Should fallback to InternedString(4294967295)
        assert!(
            debug_str.contains("InternedString(4294967295)"),
            "Debug output should fallback to raw ID for unknown label"
        );
    }

    #[test]
    fn test_edge_debug() {
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let edge = Edge::new(
            EdgeId::new(10).unwrap(),
            label,
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            PropertyMapBuilder::new().insert("since", 2024).build(),
            VersionId::new(1).unwrap(),
        );

        let debug_str = format!("{:?}", edge);
        assert!(
            debug_str.contains("KNOWS"),
            "Debug output should contain resolved label"
        );
        assert!(
            debug_str.contains("2024"),
            "Debug output should contain property value"
        );
    }

    #[test]
    fn test_edge_debug_fallback() {
        // Create an Edge with a raw InternedString that doesn't exist
        let raw_label = InternedString::from_raw(u32::MAX - 1);
        let edge = Edge::new(
            EdgeId::new(10).unwrap(),
            raw_label,
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        let debug_str = format!("{:?}", edge);
        assert!(
            debug_str.contains("InternedString(4294967294)"),
            "Debug output should fallback to raw ID for unknown label"
        );
    }
}
