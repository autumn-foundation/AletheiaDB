//! Strongly-typed ID types for graph elements.
//!
//! This module provides distinct types for different kinds of identifiers to prevent
//! mix-ups at compile time. For example, you cannot accidentally pass a `NodeId` where
//! an `EdgeId` is expected.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Unique identifier for a node in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u64);

impl NodeId {
    /// Create a new NodeId from a u64 value.
    #[inline]
    pub const fn new(id: u64) -> Self {
        NodeId(id)
    }

    /// Get the inner u64 value.
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Node({})", self.0)
    }
}

/// Unique identifier for an edge in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeId(u64);

impl EdgeId {
    /// Create a new EdgeId from a u64 value.
    #[inline]
    pub const fn new(id: u64) -> Self {
        EdgeId(id)
    }

    /// Get the inner u64 value.
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Edge({})", self.0)
    }
}

/// Unique identifier for a version of a node or edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VersionId(u64);

impl VersionId {
    /// Create a new VersionId from a u64 value.
    #[inline]
    pub const fn new(id: u64) -> Self {
        VersionId(id)
    }

    /// Get the inner u64 value.
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for VersionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Version({})", self.0)
    }
}

/// Represents either a node or an edge identifier.
///
/// Useful for operations that work with both nodes and edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityId {
    /// Node entity variant
    Node(NodeId),
    /// Edge entity variant
    Edge(EdgeId),
}

impl EntityId {
    /// Returns true if this is a node ID.
    #[inline]
    pub const fn is_node(&self) -> bool {
        matches!(self, EntityId::Node(_))
    }

    /// Returns true if this is an edge ID.
    #[inline]
    pub const fn is_edge(&self) -> bool {
        matches!(self, EntityId::Edge(_))
    }

    /// Returns the inner NodeId if this is a node, None otherwise.
    #[inline]
    pub const fn as_node(&self) -> Option<NodeId> {
        match self {
            EntityId::Node(id) => Some(*id),
            EntityId::Edge(_) => None,
        }
    }

    /// Returns the inner EdgeId if this is an edge, None otherwise.
    #[inline]
    pub const fn as_edge(&self) -> Option<EdgeId> {
        match self {
            EntityId::Node(_) => None,
            EntityId::Edge(id) => Some(*id),
        }
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntityId::Node(id) => write!(f, "{}", id),
            EntityId::Edge(id) => write!(f, "{}", id),
        }
    }
}

impl From<NodeId> for EntityId {
    fn from(id: NodeId) -> Self {
        EntityId::Node(id)
    }
}

impl From<EdgeId> for EntityId {
    fn from(id: EdgeId) -> Self {
        EntityId::Edge(id)
    }
}

/// Atomic ID generator for creating unique IDs.
///
/// This is thread-safe and can be used concurrently without external synchronization.
pub struct IdGenerator {
    next_id: AtomicU64,
}

impl IdGenerator {
    /// Create a new ID generator starting from 0.
    pub const fn new() -> Self {
        IdGenerator {
            next_id: AtomicU64::new(0),
        }
    }

    /// Create a new ID generator starting from a specific value.
    pub const fn with_start(start: u64) -> Self {
        IdGenerator {
            next_id: AtomicU64::new(start),
        }
    }

    /// Generate the next unique ID.
    ///
    /// This method is thread-safe and lock-free.
    #[inline]
    pub fn next(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Get the current value without incrementing.
    #[inline]
    pub fn current(&self) -> u64 {
        self.next_id.load(Ordering::Relaxed)
    }
}

impl Default for IdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_id_creation() {
        let id = NodeId::new(42);
        assert_eq!(id.as_u64(), 42);
    }

    #[test]
    fn test_edge_id_creation() {
        let id = EdgeId::new(100);
        assert_eq!(id.as_u64(), 100);
    }

    #[test]
    fn test_version_id_creation() {
        let id = VersionId::new(1000);
        assert_eq!(id.as_u64(), 1000);
    }

    #[test]
    fn test_entity_id_from_node() {
        let node_id = NodeId::new(1);
        let entity_id: EntityId = node_id.into();
        assert!(entity_id.is_node());
        assert!(!entity_id.is_edge());
        assert_eq!(entity_id.as_node(), Some(node_id));
    }

    #[test]
    fn test_entity_id_from_edge() {
        let edge_id = EdgeId::new(2);
        let entity_id: EntityId = edge_id.into();
        assert!(!entity_id.is_node());
        assert!(entity_id.is_edge());
        assert_eq!(entity_id.as_edge(), Some(edge_id));
    }

    #[test]
    fn test_id_generator() {
        let generator = IdGenerator::new();
        assert_eq!(generator.next(), 0);
        assert_eq!(generator.next(), 1);
        assert_eq!(generator.next(), 2);
        assert_eq!(generator.current(), 3);
    }

    #[test]
    fn test_id_generator_with_start() {
        let generator = IdGenerator::with_start(100);
        assert_eq!(generator.next(), 100);
        assert_eq!(generator.next(), 101);
    }

    #[test]
    fn test_id_display() {
        let node = NodeId::new(42);
        let edge = EdgeId::new(100);
        let version = VersionId::new(1000);

        assert_eq!(format!("{}", node), "Node(42)");
        assert_eq!(format!("{}", edge), "Edge(100)");
        assert_eq!(format!("{}", version), "Version(1000)");
    }

    #[test]
    fn test_ids_are_distinct_types() {
        // This test ensures that you cannot accidentally use one type where another is expected.
        // This is enforced by the type system, so we just verify we can create different types.
        let _node = NodeId::new(1);
        let _edge = EdgeId::new(1);
        let _version = VersionId::new(1);

        // The following would fail to compile (which is what we want):
        // fn takes_node_id(_id: NodeId) {}
        // takes_node_id(_edge); // Type error!
    }
}
