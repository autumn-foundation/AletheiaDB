//! Write buffering for uncommitted transaction changes

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::PropertyMap;
use crate::core::temporal::BiTemporalInterval;
use crate::utils::error::{Result, StorageError};
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

/// Default maximum number of operations per transaction (DoS protection)
pub const DEFAULT_MAX_OPERATIONS: usize = 10_000;

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

    /// Maximum number of operations allowed (DoS protection)
    max_operations: usize,

    /// Track whether any vector properties were written in this transaction.
    /// This flag is used to optimize the commit path by only triggering
    /// temporal vector index updates when vector data was actually modified.
    has_vector_operations: bool,
}

impl WriteBuffer {
    /// Create a new empty write buffer with default capacity limit
    pub fn new() -> Self {
        Self::with_max_operations(DEFAULT_MAX_OPERATIONS)
    }

    /// Create a write buffer with a custom maximum operations limit
    pub fn with_max_operations(max_operations: usize) -> Self {
        WriteBuffer {
            operations: Vec::new(),
            modified_nodes: HashMap::new(),
            modified_edges: HashMap::new(),
            max_operations,
            has_vector_operations: false,
        }
    }

    /// Create a write buffer with pre-allocated capacity
    ///
    /// Sets max_operations to the requested capacity to avoid confusing behavior
    /// where the buffer is pre-allocated but still enforces the default limit.
    pub fn with_capacity(capacity: usize) -> Self {
        WriteBuffer {
            operations: Vec::with_capacity(capacity),
            modified_nodes: HashMap::with_capacity(capacity / 2),
            modified_edges: HashMap::with_capacity(capacity / 2),
            max_operations: capacity,
            has_vector_operations: false,
        }
    }

    /// Add a write operation to the buffer
    ///
    /// Returns an error if the maximum number of operations is exceeded (DoS protection).
    pub fn add(&mut self, write: BufferedWrite) -> Result<()> {
        // Check capacity limit (DoS protection)
        if self.operations.len() >= self.max_operations {
            return Err(StorageError::CapacityExceeded {
                resource: "transaction operations".to_string(),
                current: self.operations.len(),
                limit: self.max_operations,
            }
            .into());
        }

        let index = self.operations.len();

        // Track which entities are modified for conflict detection
        // and check for vector properties
        match &write {
            BufferedWrite::CreateNode {
                node_id,
                properties,
                ..
            }
            | BufferedWrite::UpdateNode {
                node_id,
                properties,
                ..
            } => {
                self.modified_nodes.insert(*node_id, index);
                // Check if this operation contains vector properties
                self.has_vector_operations =
                    self.has_vector_operations || properties.contains_vector();
            }
            BufferedWrite::DeleteNode { node_id } => {
                self.modified_nodes.insert(*node_id, index);
            }
            BufferedWrite::CreateEdge {
                edge_id,
                properties,
                ..
            }
            | BufferedWrite::UpdateEdge {
                edge_id,
                properties,
                ..
            } => {
                self.modified_edges.insert(*edge_id, index);
                // Check if this operation contains vector properties
                self.has_vector_operations =
                    self.has_vector_operations || properties.contains_vector();
            }
            BufferedWrite::DeleteEdge { edge_id } => {
                self.modified_edges.insert(*edge_id, index);
            }
        }

        self.operations.push(write);
        Ok(())
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

    /// Get the buffered write for a node, if any
    pub fn get_node_write(&self, node_id: NodeId) -> Option<&BufferedWrite> {
        self.modified_nodes
            .get(&node_id)
            .map(|&index| &self.operations[index])
    }

    /// Get the buffered write for an edge, if any
    pub fn get_edge_write(&self, edge_id: EdgeId) -> Option<&BufferedWrite> {
        self.modified_edges
            .get(&edge_id)
            .map(|&index| &self.operations[index])
    }

    /// Clear all buffered operations
    pub fn clear(&mut self) {
        self.operations.clear();
        self.modified_nodes.clear();
        self.modified_edges.clear();
        self.has_vector_operations = false;
    }

    /// Get the number of buffered operations
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Check if this transaction contains any vector property operations.
    ///
    /// This is used to optimize the commit path by only triggering temporal
    /// vector index updates when vector data was actually modified.
    pub fn has_vector_operations(&self) -> bool {
        self.has_vector_operations
    }

    /// Mark that this transaction contains vector property operations.
    ///
    /// This is typically called when deleting entities that contain vector
    /// properties, ensuring the temporal vector index is notified even though
    /// the delete operation itself doesn't include property data in the buffer.
    pub fn mark_has_vector_operations(&mut self) {
        self.has_vector_operations = true;
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
        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("Person")
            .unwrap();
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        buffer
            .add(BufferedWrite::CreateNode {
                node_id,
                version_id,
                label,
                properties,
                temporal,
            })
            .unwrap();

        assert_eq!(buffer.len(), 1);
        assert!(!buffer.is_empty());
        assert!(buffer.has_modified_node(node_id));
        assert!(!buffer.has_modified_node(NodeId::new(2).unwrap()));
    }

    #[test]
    fn test_write_buffer_add_edge() {
        let mut buffer = WriteBuffer::new();
        let edge_id = EdgeId::new(1).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("KNOWS")
            .unwrap();
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        buffer
            .add(BufferedWrite::CreateEdge {
                edge_id,
                version_id,
                source,
                target,
                label,
                properties,
                temporal,
            })
            .unwrap();

        assert_eq!(buffer.len(), 1);
        assert!(buffer.has_modified_edge(edge_id));
        assert!(!buffer.has_modified_edge(EdgeId::new(2).unwrap()));
    }

    #[test]
    fn test_write_buffer_multiple_operations() {
        let mut buffer = WriteBuffer::new();
        let node_id = NodeId::new(1).unwrap();
        let edge_id = EdgeId::new(1).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("Test")
            .unwrap();
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        // Add node
        buffer
            .add(BufferedWrite::CreateNode {
                node_id,
                version_id,
                label,
                properties: properties.clone(),
                temporal,
            })
            .unwrap();

        // Add edge
        buffer
            .add(BufferedWrite::CreateEdge {
                edge_id,
                version_id,
                source: node_id,
                target: NodeId::new(2).unwrap(),
                label,
                properties,
                temporal,
            })
            .unwrap();

        assert_eq!(buffer.len(), 2);
        assert!(buffer.has_modified_node(node_id));
        assert!(buffer.has_modified_edge(edge_id));
    }

    #[test]
    fn test_write_buffer_clear() {
        let mut buffer = WriteBuffer::new();
        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("Test")
            .unwrap();
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        buffer
            .add(BufferedWrite::CreateNode {
                node_id,
                version_id,
                label,
                properties,
                temporal,
            })
            .unwrap();

        assert_eq!(buffer.len(), 1);

        buffer.clear();

        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert!(!buffer.has_modified_node(node_id));
    }

    #[test]
    fn test_write_buffer_update_tracking() {
        let mut buffer = WriteBuffer::new();
        let node_id = NodeId::new(1).unwrap();
        let version_id_1 = VersionId::new(1).unwrap();
        let version_id_2 = VersionId::new(2).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("Test")
            .unwrap();
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        // Create node
        buffer
            .add(BufferedWrite::CreateNode {
                node_id,
                version_id: version_id_1,
                label,
                properties: properties.clone(),
                temporal,
            })
            .unwrap();

        // Update same node
        buffer
            .add(BufferedWrite::UpdateNode {
                node_id,
                version_id: version_id_2,
                label,
                properties,
                temporal,
            })
            .unwrap();

        // Should have 2 operations, but node appears once in modified_nodes
        assert_eq!(buffer.len(), 2);
        assert!(buffer.has_modified_node(node_id));

        // The most recent operation index should be stored
        assert_eq!(buffer.modified_nodes.get(&node_id), Some(&1));
    }

    #[test]
    fn test_capacity_exceeded() {
        // Create buffer with small capacity
        let mut buffer = WriteBuffer::with_max_operations(2);
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("Test")
            .unwrap();
        let properties = PropertyMap::new();
        let temporal = BiTemporalInterval::current(time::now());

        // Add first operation - should succeed
        buffer
            .add(BufferedWrite::CreateNode {
                node_id: NodeId::new(1).unwrap(),
                version_id: VersionId::new(1).unwrap(),
                label,
                properties: properties.clone(),
                temporal,
            })
            .unwrap();

        // Add second operation - should succeed
        buffer
            .add(BufferedWrite::CreateNode {
                node_id: NodeId::new(2).unwrap(),
                version_id: VersionId::new(2).unwrap(),
                label,
                properties: properties.clone(),
                temporal,
            })
            .unwrap();

        // Add third operation - should fail (exceeds capacity)
        let result = buffer.add(BufferedWrite::CreateNode {
            node_id: NodeId::new(3).unwrap(),
            version_id: VersionId::new(3).unwrap(),
            label,
            properties,
            temporal,
        });

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::utils::error::Error::Storage(StorageError::CapacityExceeded {
                resource,
                current,
                limit,
            }) => {
                assert_eq!(resource, "transaction operations");
                assert_eq!(current, 2);
                assert_eq!(limit, 2);
            }
            _ => panic!("Expected CapacityExceeded error"),
        }
    }

    #[test]
    fn test_write_buffer_with_capacity() {
        let buffer = WriteBuffer::with_capacity(10);
        assert_eq!(buffer.operations.capacity(), 10);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_vector_operations_tracking_nodes() {
        use crate::core::property::PropertyValue;

        let mut buffer = WriteBuffer::new();
        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("Document")
            .unwrap();
        let temporal = BiTemporalInterval::current(time::now());

        // Initially, no vector operations
        assert!(!buffer.has_vector_operations());

        // Create node with vector property
        let mut props = PropertyMap::new();
        props = props
            .builder()
            .insert("embedding", PropertyValue::vector([0.1, 0.2, 0.3]))
            .build();

        buffer
            .add(BufferedWrite::CreateNode {
                node_id,
                version_id,
                label,
                properties: props,
                temporal,
            })
            .unwrap();

        // Should now track vector operations
        assert!(buffer.has_vector_operations());
    }

    #[test]
    fn test_vector_operations_tracking_no_vectors() {
        let mut buffer = WriteBuffer::new();
        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("Person")
            .unwrap();
        let temporal = BiTemporalInterval::current(time::now());

        // Create node with only scalar properties (no vectors)
        let props = PropertyMap::new()
            .builder()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("active", true)
            .build();

        buffer
            .add(BufferedWrite::CreateNode {
                node_id,
                version_id,
                label,
                properties: props,
                temporal,
            })
            .unwrap();

        // Should NOT track vector operations
        assert!(!buffer.has_vector_operations());
    }

    #[test]
    fn test_vector_operations_clear() {
        use crate::core::property::PropertyValue;

        let mut buffer = WriteBuffer::new();
        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("Document")
            .unwrap();
        let temporal = BiTemporalInterval::current(time::now());

        // Add operation with vector
        let props = PropertyMap::new()
            .builder()
            .insert("embedding", PropertyValue::vector([0.1, 0.2, 0.3]))
            .build();

        buffer
            .add(BufferedWrite::CreateNode {
                node_id,
                version_id,
                label,
                properties: props,
                temporal,
            })
            .unwrap();

        assert!(buffer.has_vector_operations());

        // Clear should reset the flag
        buffer.clear();
        assert!(!buffer.has_vector_operations());
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_vector_operations_tracking_edges() {
        use crate::core::property::PropertyValue;

        let mut buffer = WriteBuffer::new();
        let edge_id = EdgeId::new(1).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("SIMILAR_TO")
            .unwrap();
        let temporal = BiTemporalInterval::current(time::now());

        // Initially, no vector operations
        assert!(!buffer.has_vector_operations());

        // Create edge with vector property
        let props = PropertyMap::new()
            .builder()
            .insert("similarity", PropertyValue::vector([0.95, 0.85, 0.90]))
            .build();

        buffer
            .add(BufferedWrite::CreateEdge {
                edge_id,
                version_id,
                source,
                target,
                label,
                properties: props,
                temporal,
            })
            .unwrap();

        // Should now track vector operations
        assert!(buffer.has_vector_operations());
    }

    #[test]
    fn test_vector_operations_update_node() {
        use crate::core::property::PropertyValue;

        let mut buffer = WriteBuffer::new();
        let node_id = NodeId::new(1).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("Document")
            .unwrap();
        let temporal = BiTemporalInterval::current(time::now());

        // Update node with vector property
        let props = PropertyMap::new()
            .builder()
            .insert("embedding", PropertyValue::vector([0.1, 0.2, 0.3]))
            .build();

        buffer
            .add(BufferedWrite::UpdateNode {
                node_id,
                version_id,
                label,
                properties: props,
                temporal,
            })
            .unwrap();

        // Should track vector operations
        assert!(buffer.has_vector_operations());
    }

    #[test]
    fn test_vector_operations_update_edge() {
        use crate::core::property::PropertyValue;

        let mut buffer = WriteBuffer::new();
        let edge_id = EdgeId::new(1).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("SIMILAR_TO")
            .unwrap();
        let temporal = BiTemporalInterval::current(time::now());

        // Update edge with vector property
        let props = PropertyMap::new()
            .builder()
            .insert("similarity", PropertyValue::vector([0.95]))
            .build();

        buffer
            .add(BufferedWrite::UpdateEdge {
                edge_id,
                version_id,
                source,
                target,
                label,
                properties: props,
                temporal,
            })
            .unwrap();

        // Should track vector operations
        assert!(buffer.has_vector_operations());
    }

    #[test]
    fn test_vector_operations_mark_manually() {
        let mut buffer = WriteBuffer::new();

        assert!(!buffer.has_vector_operations());

        // Manually mark as having vector operations
        buffer.mark_has_vector_operations();

        assert!(buffer.has_vector_operations());
    }

    #[test]
    fn test_vector_operations_mixed_operations() {
        use crate::core::property::PropertyValue;

        let mut buffer = WriteBuffer::new();
        let temporal = BiTemporalInterval::current(time::now());
        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("Test")
            .unwrap();

        // Add several operations without vectors
        for i in 0..5 {
            buffer
                .add(BufferedWrite::CreateNode {
                    node_id: NodeId::new(i).unwrap(),
                    version_id: VersionId::new(i).unwrap(),
                    label,
                    properties: PropertyMap::new().builder().insert("id", i as i64).build(),
                    temporal,
                })
                .unwrap();
        }

        assert!(!buffer.has_vector_operations());

        // Add one operation with a vector
        let props = PropertyMap::new()
            .builder()
            .insert("embedding", PropertyValue::vector([0.1, 0.2]))
            .build();

        buffer
            .add(BufferedWrite::CreateNode {
                node_id: NodeId::new(100).unwrap(),
                version_id: VersionId::new(100).unwrap(),
                label,
                properties: props,
                temporal,
            })
            .unwrap();

        // Should now track vector operations
        assert!(buffer.has_vector_operations());

        // Add more non-vector operations - flag should remain true
        for i in 6..10 {
            buffer
                .add(BufferedWrite::CreateNode {
                    node_id: NodeId::new(i).unwrap(),
                    version_id: VersionId::new(i).unwrap(),
                    label,
                    properties: PropertyMap::new().builder().insert("id", i as i64).build(),
                    temporal,
                })
                .unwrap();
        }

        assert!(buffer.has_vector_operations());
    }
}
