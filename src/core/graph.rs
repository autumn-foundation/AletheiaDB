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

/// Hot-path header for a node, containing only fields needed for label filtering.
///
/// Storing these 16 bytes separately from the full [`Node`] struct (which carries
/// a large `PropertyMap` Arc) lets label-scan queries iterate 16 bytes per entry
/// instead of 100-500+ bytes, dramatically reducing cache pressure.
///
/// `NodeHeader` is kept in sync with the full node map: every `insert_node` creates
/// a corresponding header and every `remove_node` removes it.
///
/// # Examples
/// ```
/// use aletheiadb::core::id::NodeId;
/// use aletheiadb::core::interning::InternedString;
/// use aletheiadb::core::graph::NodeHeader;
///
/// let id = NodeId::new(42).unwrap();
/// let label = InternedString::new(1);
/// let header = NodeHeader { id, label };
/// assert_eq!(header.id, id);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeHeader {
    /// Unique identifier for this node (mirrors `Node::id`).
    pub id: NodeId,
    /// Label/type of the node (mirrors `Node::label`).
    pub label: InternedString,
}

impl From<&Node> for NodeHeader {
    #[inline]
    fn from(node: &Node) -> Self {
        NodeHeader {
            id: node.id,
            label: node.label,
        }
    }
}

/// A node in the current state of the graph.
///
/// This represents the current version of a node, optimized for fast access.
/// Historical versions are stored separately in the temporal storage layer.
///
/// # Examples
/// ```
/// use aletheiadb::core::id::{NodeId, VersionId, TxId};
/// use aletheiadb::core::interning::InternedString;
/// use aletheiadb::core::graph::Node;
/// use aletheiadb::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
/// use aletheiadb::core::version::VersionMetadata;
/// use aletheiadb::core::temporal::Timestamp;
///
/// let properties = PropertyMapBuilder::new()
///     .insert("name", PropertyValue::String("Alice".into()))
///     .build();
///
/// let node = Node {
///     id: NodeId::new(42).unwrap(),
///     label: InternedString::new(1),
///     properties,
///     current_version: VersionId::new(1).unwrap(),
///     metadata: VersionMetadata {
///         tx_id: TxId::new(10),
///         valid_time: Timestamp::MIN,
///     },
/// };
/// assert_eq!(node.properties.get("name").unwrap(), &PropertyValue::String("Alice".into()));
/// ```
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
    /// ```rust,no_run
    /// # use aletheiadb::core::{Node, NodeId, VersionId, PropertyMap};
    /// # use aletheiadb::core::interning::GLOBAL_INTERNER;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let label = GLOBAL_INTERNER.intern("Person")?;
    /// # let node = Node::new(NodeId::new(1)?, label, PropertyMap::new(), VersionId::new(1)?);
    /// // Convenient for one-off checks
    /// if node.has_label_str("Person") {
    ///     // ...
    /// }
    ///
    /// // For performance-critical loops, pre-intern:
    /// let person_label = GLOBAL_INTERNER.intern("Person")?;
    /// // for node in many_nodes {
    ///     if node.has_label(person_label) {  // Faster!
    ///         // ...
    ///     }
    /// // }
    /// # Ok(())
    /// # }
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
///
/// # Examples
/// ```
/// use aletheiadb::core::id::{EdgeId, NodeId, VersionId, TxId};
/// use aletheiadb::core::interning::InternedString;
/// use aletheiadb::core::graph::Edge;
/// use aletheiadb::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
/// use aletheiadb::core::version::VersionMetadata;
/// use aletheiadb::core::temporal::Timestamp;
///
/// let properties = PropertyMapBuilder::new()
///     .insert("weight", PropertyValue::Float(1.5))
///     .build();
///
/// let edge = Edge {
///     id: EdgeId::new(100).unwrap(),
///     label: InternedString::new(2),
///     source: NodeId::new(42).unwrap(),
///     target: NodeId::new(43).unwrap(),
///     properties,
///     current_version: VersionId::new(1).unwrap(),
///     metadata: VersionMetadata {
///         tx_id: TxId::new(11),
///         valid_time: Timestamp::MIN,
///     },
/// };
/// assert_eq!(edge.source, NodeId::new(42).unwrap());
/// ```
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
    /// ```rust,no_run
    /// # use aletheiadb::core::{Edge, EdgeId, NodeId, VersionId, PropertyMap};
    /// # use aletheiadb::core::interning::GLOBAL_INTERNER;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let label = GLOBAL_INTERNER.intern("KNOWS")?;
    /// # let edge = Edge::new(EdgeId::new(1)?, label, NodeId::new(1)?, NodeId::new(2)?, PropertyMap::new(), VersionId::new(1)?);
    /// // Convenient for one-off checks
    /// if edge.has_label_str("KNOWS") {
    ///     // ...
    /// }
    ///
    /// // For performance-critical loops, pre-intern:
    /// let knows_label = GLOBAL_INTERNER.intern("KNOWS")?;
    /// // for edge in many_edges {
    ///     if edge.has_label(knows_label) {  // Faster!
    ///         // ...
    ///     }
    /// // }
    /// # Ok(())
    /// # }
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
        assert_eq!(node.metadata, VersionMetadata::default());
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
        assert_eq!(edge.metadata, VersionMetadata::default());
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

    #[test]
    fn test_node_with_metadata() {
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let props = PropertyMapBuilder::new().build();
        let metadata = VersionMetadata::new(
            crate::core::id::TxId::new(123),
            crate::core::temporal::Timestamp::from(456),
        );

        let node = Node::with_metadata(
            NodeId::new(1).unwrap(),
            label,
            props,
            VersionId::new(1).unwrap(),
            metadata,
        );

        assert_eq!(node.metadata, metadata);
        assert_ne!(node.metadata, VersionMetadata::default());
    }

    #[test]
    fn test_edge_with_metadata() {
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let props = PropertyMapBuilder::new().build();
        let metadata = VersionMetadata::new(
            crate::core::id::TxId::new(789),
            crate::core::temporal::Timestamp::from(999),
        );

        let edge = Edge::with_metadata(
            EdgeId::new(1).unwrap(),
            label,
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            props,
            VersionId::new(1).unwrap(),
            metadata,
        );

        assert_eq!(edge.metadata, metadata);
        assert_ne!(edge.metadata, VersionMetadata::default());
    }

    #[test]
    fn test_edge_connects_source_mismatch() {
        let edge = Edge::new(
            EdgeId::new(1).unwrap(),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        // Correct: source 1, target 2
        assert!(edge.connects(NodeId::new(1).unwrap(), NodeId::new(2).unwrap()));

        // Mismatch source, match target
        assert!(
            !edge.connects(NodeId::new(3).unwrap(), NodeId::new(2).unwrap()),
            "Should return false when source mismatches even if target matches"
        );
    }

    #[test]
    fn test_edge_connects_exhaustive() {
        // 🛡️ Sentry Test: Kill mutants replacing `==` with `!=` or returning `true`/`false` or mutating `&&` to `||` in `connects`.
        let edge = Edge::new(
            EdgeId::new(1).unwrap(),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(10).unwrap(),
            NodeId::new(20).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        // Match both
        assert!(
            edge.connects(NodeId::new(10).unwrap(), NodeId::new(20).unwrap()),
            "Should return true for exact match"
        );

        // Mismatch both
        assert!(
            !edge.connects(NodeId::new(11).unwrap(), NodeId::new(21).unwrap()),
            "Should return false for complete mismatch"
        );

        // Match source, mismatch target
        assert!(
            !edge.connects(NodeId::new(10).unwrap(), NodeId::new(21).unwrap()),
            "Should return false when target mismatches"
        );

        // Mismatch source, match target
        assert!(
            !edge.connects(NodeId::new(11).unwrap(), NodeId::new(20).unwrap()),
            "Should return false when source mismatches"
        );
    }
}

/// Tests for NodeHeader hot/cold path split (Issue #340).
///
/// NodeHeader contains only the hot-path fields (id + label) needed for
/// filtering, allowing label scans to touch 16 bytes per node instead of
/// the full Node struct (100-500+ bytes with PropertyMap).
#[cfg(test)]
mod node_header_tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_node_header_has_id_and_label() {
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let header = NodeHeader {
            id: NodeId::new(42).unwrap(),
            label,
        };
        assert_eq!(header.id, NodeId::new(42).unwrap());
        assert_eq!(header.label, label);
    }

    #[test]
    fn test_node_header_from_node() {
        let label = GLOBAL_INTERNER.intern("Company").unwrap();
        let node = Node::new(
            NodeId::new(7).unwrap(),
            label,
            PropertyMapBuilder::new().insert("name", "Acme").build(),
            VersionId::new(1).unwrap(),
        );
        let header = NodeHeader::from(&node);
        assert_eq!(header.id, node.id);
        assert_eq!(header.label, node.label);
    }

    #[test]
    fn test_node_header_is_copy() {
        let label = GLOBAL_INTERNER.intern("Event").unwrap();
        let h1 = NodeHeader {
            id: NodeId::new(1).unwrap(),
            label,
        };
        let h2 = h1; // Copy, not move
        assert_eq!(h1.id, h2.id);
    }

    #[test]
    fn test_node_header_size_is_small() {
        // NodeHeader must be smaller than a full Node. Node has PropertyMap (Arc),
        // VersionId, and VersionMetadata on top of id + label.
        assert!(
            std::mem::size_of::<NodeHeader>() < std::mem::size_of::<Node>(),
            "NodeHeader ({} bytes) should be smaller than Node ({} bytes)",
            std::mem::size_of::<NodeHeader>(),
            std::mem::size_of::<Node>(),
        );
        // NodeHeader is exactly NodeId(u64) + InternedString(u32) + padding = 16 bytes
        assert!(
            std::mem::size_of::<NodeHeader>() <= 16,
            "NodeHeader should fit in at most 16 bytes, got {}",
            std::mem::size_of::<NodeHeader>()
        );
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;
    use crate::core::interning::{GLOBAL_INTERNER, InternedString};
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_matches_label_robustness() {
        // Create a Node with a raw InternedString that doesn't exist.
        // This exercises the `unwrap_or(false)` path in `matches_label`.
        let raw_label = InternedString::from_raw(u32::MAX - 5);
        let node = Node::new(
            NodeId::new(1).unwrap(),
            raw_label,
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        // Should return false, not panic or error.
        assert!(
            !node.has_label_str("AnyString"),
            "has_label_str should return false for invalid label ID"
        );
    }

    #[test]
    fn test_matches_label_mismatch() {
        // 🛡️ Sentry Test: Verify `matches_label` correctly returns false when checking a valid
        // label against a different string. This kills the mutant replacing `matches_label`
        // return value with `true`.
        let label = GLOBAL_INTERNER.intern("User").unwrap();
        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        assert!(
            !node.has_label_str("Admin"),
            "matches_label should return false when checking against a different label"
        );
    }

    #[test]
    fn test_node_with_metadata() {
        // 🛡️ Sentry Test: Verify Node::with_metadata correctly stores metadata.
        // This targets mutants where the metadata argument is ignored and replaced with default.
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let props = PropertyMapBuilder::new().build();
        let tx_id = crate::core::id::TxId::new(123);
        let timestamp = crate::core::temporal::Timestamp::from(456);
        let metadata = crate::core::version::VersionMetadata::new(tx_id, timestamp);

        let node = Node::with_metadata(
            NodeId::new(1).unwrap(),
            label,
            props,
            VersionId::new(10).unwrap(),
            metadata,
        );

        assert_eq!(
            node.metadata, metadata,
            "Node::with_metadata should store the provided metadata"
        );
        assert_eq!(node.metadata.created_by_tx, tx_id);
        assert_eq!(node.metadata.commit_timestamp, Some(timestamp));
    }

    #[test]
    fn test_edge_with_metadata() {
        // 🛡️ Sentry Test: Verify Edge::with_metadata correctly stores metadata.
        // This targets mutants where the metadata argument is ignored and replaced with default.
        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let props = PropertyMapBuilder::new().build();
        let tx_id = crate::core::id::TxId::new(789);
        let timestamp = crate::core::temporal::Timestamp::from(1000);
        let metadata = crate::core::version::VersionMetadata::new(tx_id, timestamp);

        let edge = Edge::with_metadata(
            EdgeId::new(1).unwrap(),
            label,
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            props,
            VersionId::new(1).unwrap(),
            metadata,
        );

        assert_eq!(
            edge.metadata, metadata,
            "Edge::with_metadata should store the provided metadata"
        );
        assert_eq!(edge.metadata.created_by_tx, tx_id);
        assert_eq!(edge.metadata.commit_timestamp, Some(timestamp));
    }

    #[test]
    fn test_get_property_behavior() {
        // 🛡️ Sentry Test: Kill mutants returning `None` or `Some(Default::default())` from `get_property`.
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let mut props = PropertyMapBuilder::new();
        props = props.insert("age", 42);
        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            props.build(),
            VersionId::new(1).unwrap(),
        );

        // Positive check: Must return the exact property value (not a default like 0 or None).
        let age_prop = node
            .get_property("age")
            .expect("Should find 'age' property");
        assert_eq!(
            age_prop.as_int(),
            Some(42),
            "get_property must return the exact stored value"
        );

        // Negative check: Must return None for missing properties.
        assert!(
            node.get_property("missing").is_none(),
            "get_property must return None for missing key"
        );

        let mut edge_props = PropertyMapBuilder::new();
        edge_props = edge_props.insert("weight", 42.5);
        let edge = Edge::new(
            EdgeId::new(1).unwrap(),
            label,
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            edge_props.build(),
            VersionId::new(1).unwrap(),
        );

        let weight_prop = edge
            .get_property("weight")
            .expect("Should find 'weight' property");
        assert_eq!(
            weight_prop.as_float(),
            Some(42.5),
            "Edge::get_property must return the exact stored value"
        );
        assert!(
            edge.get_property("missing").is_none(),
            "Edge::get_property must return None for missing key"
        );
    }

    #[test]
    fn test_has_label_behavior() {
        // 🛡️ Sentry Test: Kill mutants replacing `==` with `!=` or returning `true`/`false` in `has_label`.
        let label_a = GLOBAL_INTERNER.intern("TypeA").unwrap();
        let label_b = GLOBAL_INTERNER.intern("TypeB").unwrap();

        let node = Node::new(
            NodeId::new(1).unwrap(),
            label_a,
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        assert!(
            node.has_label(label_a),
            "has_label must return true for exact match"
        );
        assert!(
            !node.has_label(label_b),
            "has_label must return false for mismatch"
        );

        let edge = Edge::new(
            EdgeId::new(1).unwrap(),
            label_a,
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        assert!(
            edge.has_label(label_a),
            "Edge::has_label must return true for exact match"
        );
        assert!(
            !edge.has_label(label_b),
            "Edge::has_label must return false for mismatch"
        );
    }

    #[test]
    fn test_has_label_str_behavior() {
        // 🛡️ Sentry Test: Kill mutants returning `true`/`false` in `has_label_str`.
        let label = GLOBAL_INTERNER.intern("TargetLabel").unwrap();

        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        assert!(
            node.has_label_str("TargetLabel"),
            "has_label_str must return true for exact match"
        );
        assert!(
            !node.has_label_str("OtherLabel"),
            "has_label_str must return false for mismatch"
        );

        let edge = Edge::new(
            EdgeId::new(1).unwrap(),
            label,
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        assert!(
            edge.has_label_str("TargetLabel"),
            "Edge::has_label_str must return true for exact match"
        );
        assert!(
            !edge.has_label_str("OtherLabel"),
            "Edge::has_label_str must return false for mismatch"
        );
    }

    #[test]
    fn test_matches_label_behavior() {
        // 🛡️ Sentry Test: Kill mutants replacing `==` with `!=` or returning `true`/`false` in `matches_label`.
        let label_id = GLOBAL_INTERNER.intern("MatchMe").unwrap();

        // Direct test of the matches_label function
        assert!(
            super::matches_label(label_id, "MatchMe"),
            "matches_label must return true for match"
        );
        assert!(
            !super::matches_label(label_id, "DoNotMatch"),
            "matches_label must return false for mismatch"
        );
    }

    #[test]
    fn test_debug_format_not_empty() {
        // 🛡️ Sentry Test: Kill mutants replacing `fmt` body with `Ok(Default::default())`.
        let label = GLOBAL_INTERNER.intern("DebugLabel").unwrap();
        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        let node_debug = format!("{:?}", node);
        assert!(
            !node_debug.is_empty(),
            "Node debug format must not be empty"
        );
        // Ok(Default::default()) results in an empty string from format!("{:?}") or similar empty state
        // We explicitly assert length > 0
        assert!(
            node_debug.len() > 10,
            "Node debug format must contain structured data"
        );

        let edge = Edge::new(
            EdgeId::new(1).unwrap(),
            label,
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );

        let edge_debug = format!("{:?}", edge);
        assert!(
            !edge_debug.is_empty(),
            "Edge debug format must not be empty"
        );
        assert!(
            edge_debug.len() > 10,
            "Edge debug format must contain structured data"
        );
    }
}
