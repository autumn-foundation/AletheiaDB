//! Write buffering for uncommitted transaction changes

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::PropertyMap;
use crate::core::temporal::BiTemporalInterval;
use std::collections::HashMap;

/// Buffered write operation
///
/// Represents an uncommitted write operation that will be applied
/// atomically when the transaction commits.
#[derive(Debug, Clone)]
pub enum BufferedWrite {
    /// Create a new node
    CreateNode {
        /// Node ID
        node_id: NodeId,
        /// Version ID for this node
        version_id: VersionId,
        /// Node label
        label: InternedString,
        /// Node properties
        properties: PropertyMap,
        /// Bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Create a new edge
    CreateEdge {
        /// Edge ID
        edge_id: EdgeId,
        /// Version ID for this edge
        version_id: VersionId,
        /// Source node ID
        source: NodeId,
        /// Target node ID
        target: NodeId,
        /// Edge label
        label: InternedString,
        /// Edge properties
        properties: PropertyMap,
        /// Bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Update an existing node (creates new version)
    UpdateNode {
        /// Node ID being updated
        node_id: NodeId,
        /// New version ID
        version_id: VersionId,
        /// Node label (preserved from existing)
        label: InternedString,
        /// New properties
        properties: PropertyMap,
        /// Bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Update an existing edge (creates new version)
    UpdateEdge {
        /// Edge ID being updated
        edge_id: EdgeId,
        /// New version ID
        version_id: VersionId,
        /// Source node (preserved from existing)
        source: NodeId,
        /// Target node (preserved from existing)
        target: NodeId,
        /// Edge label (preserved from existing)
        label: InternedString,
        /// New properties
        properties: PropertyMap,
        /// Bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Delete a node
    DeleteNode {
        /// Node ID to delete
        node_id: NodeId,
    },
    /// Delete an edge
    DeleteEdge {
        /// Edge ID to delete
        edge_id: EdgeId,
    },
}

/// Write buffer for collecting uncommitted changes
///
/// Buffers all write operations in a transaction until commit time,
/// enabling atomicity and validation before applying changes.
pub struct WriteBuffer {
    /// Buffered operations in order
    operations: Vec<BufferedWrite>,

    /// Quick lookup: which nodes have been written to
    /// Maps NodeId → index in operations vector
    modified_nodes: HashMap<NodeId, usize>,

    /// Quick lookup: which edges have been written to
    /// Maps EdgeId → index in operations vector
    modified_edges: HashMap<EdgeId, usize>,
}

impl WriteBuffer {
    /// Create a new empty write buffer
    pub fn new() -> Self {
        WriteBuffer {
            operations: Vec::new(),
            modified_nodes: HashMap::new(),
            modified_edges: HashMap::new(),
        }
    }

    /// Create a write buffer with pre-allocated capacity
    pub fn with_capacity(capacity: usize) -> Self {
        WriteBuffer {
            operations: Vec::with_capacity(capacity),
            modified_nodes: HashMap::with_capacity(capacity / 2),
            modified_edges: HashMap::with_capacity(capacity / 2),
        }
    }

    /// Add a write operation to the buffer
    pub fn add(&mut self, write: BufferedWrite) {
        let index = self.operations.len();

        // Track which entities are modified for conflict detection
        match &write {
            BufferedWrite::CreateNode { node_id, .. }
            | BufferedWrite::UpdateNode { node_id, .. }
            | BufferedWrite::DeleteNode { node_id } => {
                self.modified_nodes.insert(*node_id, index);
            }
            BufferedWrite::CreateEdge { edge_id, .. }
            | BufferedWrite::UpdateEdge { edge_id, .. }
            | BufferedWrite::DeleteEdge { edge_id } => {
                self.modified_edges.insert(*edge_id, index);
            }
        }

        self.operations.push(write);
    }

    /// Get all operations in order
    pub fn operations(&self) -> &[BufferedWrite] {
        &self.operations
    }

    /// Check if a node has been modified in this buffer
    pub fn has_modified_node(&self, node_id: NodeId) -> bool {
        self.modified_nodes.contains_key(&node_id)
    }

    /// Check if an edge has been modified in this buffer
    pub fn has_modified_edge(&self, edge_id: EdgeId) -> bool {
        self.modified_edges.contains_key(&edge_id)
    }

    /// Clear all buffered operations
    pub fn clear(&mut self) {
        self.operations.clear();
        self.modified_nodes.clear();
        self.modified_edges.clear();
    }

    /// Get the number of buffered operations
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }
}

impl Default for WriteBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::temporal::time;

    #[test]
    fn test_write_buffer_creation() {
        let buffer = WriteBuffer::new();
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_write_buffer_add_node() {
        let mut buffer = WriteBuffer::new();
        let node_id = NodeId::new(1);
        let version_id = VersionId::new(1);
        let label = crate::core::interning::GLOBAL_INTERNER.intern("Person");
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        buffer.add(BufferedWrite::CreateNode {
            node_id,
            version_id,
            label,
            properties,
            temporal,
        });

        assert_eq!(buffer.len(), 1);
        assert!(!buffer.is_empty());
        assert!(buffer.has_modified_node(node_id));
        assert!(!buffer.has_modified_node(NodeId::new(2)));
    }

    #[test]
    fn test_write_buffer_add_edge() {
        let mut buffer = WriteBuffer::new();
        let edge_id = EdgeId::new(1);
        let version_id = VersionId::new(1);
        let source = NodeId::new(1);
        let target = NodeId::new(2);
        let label = crate::core::interning::GLOBAL_INTERNER.intern("KNOWS");
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        buffer.add(BufferedWrite::CreateEdge {
            edge_id,
            version_id,
            source,
            target,
            label,
            properties,
            temporal,
        });

        assert_eq!(buffer.len(), 1);
        assert!(buffer.has_modified_edge(edge_id));
        assert!(!buffer.has_modified_edge(EdgeId::new(2)));
    }

    #[test]
    fn test_write_buffer_multiple_operations() {
        let mut buffer = WriteBuffer::new();
        let node_id = NodeId::new(1);
        let edge_id = EdgeId::new(1);
        let version_id = VersionId::new(1);
        let label = crate::core::interning::GLOBAL_INTERNER.intern("Test");
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        // Add node
        buffer.add(BufferedWrite::CreateNode {
            node_id,
            version_id,
            label,
            properties: properties.clone(),
            temporal,
        });

        // Add edge
        buffer.add(BufferedWrite::CreateEdge {
            edge_id,
            version_id,
            source: node_id,
            target: NodeId::new(2),
            label,
            properties,
            temporal,
        });

        assert_eq!(buffer.len(), 2);
        assert!(buffer.has_modified_node(node_id));
        assert!(buffer.has_modified_edge(edge_id));
    }

    #[test]
    fn test_write_buffer_clear() {
        let mut buffer = WriteBuffer::new();
        let node_id = NodeId::new(1);
        let version_id = VersionId::new(1);
        let label = crate::core::interning::GLOBAL_INTERNER.intern("Test");
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        buffer.add(BufferedWrite::CreateNode {
            node_id,
            version_id,
            label,
            properties,
            temporal,
        });

        assert_eq!(buffer.len(), 1);

        buffer.clear();

        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert!(!buffer.has_modified_node(node_id));
    }

    #[test]
    fn test_write_buffer_update_tracking() {
        let mut buffer = WriteBuffer::new();
        let node_id = NodeId::new(1);
        let version_id_1 = VersionId::new(1);
        let version_id_2 = VersionId::new(2);
        let label = crate::core::interning::GLOBAL_INTERNER.intern("Test");
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        // Create node
        buffer.add(BufferedWrite::CreateNode {
            node_id,
            version_id: version_id_1,
            label,
            properties: properties.clone(),
            temporal,
        });

        // Update same node
        buffer.add(BufferedWrite::UpdateNode {
            node_id,
            version_id: version_id_2,
            label,
            properties,
            temporal,
        });

        // Should have 2 operations, but node appears once in modified_nodes
        assert_eq!(buffer.len(), 2);
        assert!(buffer.has_modified_node(node_id));

        // The most recent operation index should be stored
        assert_eq!(buffer.modified_nodes.get(&node_id), Some(&1));
    }

    #[test]
    fn test_write_buffer_with_capacity() {
        let buffer = WriteBuffer::with_capacity(10);
        assert_eq!(buffer.operations.capacity(), 10);
        assert!(buffer.is_empty());
    }
}
