//! MVCC Snapshot Isolation for Checkpointing
//!
//! This module provides snapshot isolation for checkpoint operations, preventing
//! fuzzy checkpointing (mixed state from different LSNs) that can lead to data
//! corruption.
//!
//! # Problem Statement
//!
//! Without snapshot isolation, iterating over CurrentStorage/HistoricalStorage
//! during checkpointing sees concurrent modifications, resulting in:
//! - Mixed state from different transaction times
//! - Inconsistent checkpoint data
//! - Data corruption on WAL replay
//!
//! # Solution
//!
//! Arc-based Copy-on-Write (COW) snapshots:
//! - Capture consistent point-in-time view at checkpoint LSN
//! - Use Arc references to avoid full clones (memory efficient)
//! - Isolate iteration from concurrent writes
//! - Enable streaming persistence (no Vec allocation)
//!
//! # Usage
//!
//! ```ignore
//! use gallifreydb::storage::snapshot::StorageSnapshot;
//!
//! // Create snapshot at specific LSN
//! let snapshot = current.create_snapshot(lsn);
//!
//! // Iterate over snapshot (isolated from concurrent writes)
//! for node in snapshot.iter_nodes() {
//!     // Stream to disk without loading entire DB into memory
//!     persist_node(&node)?;
//! }
//! ```

use crate::core::graph::{Edge, Node};
use crate::storage::version::{EdgeVersion, NodeVersion};
use crate::storage::wal::LSN;
use std::sync::Arc;

/// Snapshot isolation trait for storage layers.
///
/// Provides a consistent point-in-time view of storage that is isolated
/// from concurrent modifications. Used primarily for checkpointing to
/// ensure data consistency.
///
/// # Design
///
/// Snapshots use Arc-based COW:
/// - Snapshot creation does ONE iteration over DashMap/structures
/// - Collects Arc<T> references (cheap, just pointer + refcount)
/// - Total memory: 8 bytes × num_entities (not full clones)
/// - Iteration over snapshot is isolated from concurrent writes
pub trait StorageSnapshot: Send + Sync {
    /// Node iterator type (streaming, no Vec allocation)
    type NodeIter: Iterator<Item = Node>;

    /// Edge iterator type (streaming, no Vec allocation)
    type EdgeIter: Iterator<Item = Edge>;

    /// Get the LSN at which this snapshot was taken
    fn lsn(&self) -> LSN;

    /// Get the number of nodes in this snapshot
    fn node_count(&self) -> usize;

    /// Get the number of edges in this snapshot
    fn edge_count(&self) -> usize;

    /// Iterate over nodes in snapshot (streaming, isolated)
    fn iter_nodes(&self) -> Self::NodeIter;

    /// Iterate over edges in snapshot (streaming, isolated)
    fn iter_edges(&self) -> Self::EdgeIter;
}

/// Snapshot of CurrentStorage at a specific LSN.
///
/// Uses Arc-based COW to provide snapshot isolation without
/// full data clones. Memory overhead is ~8 bytes per entity.
///
/// # Isolation Guarantee
///
/// Once created, the snapshot reflects the exact state at the
/// snapshot LSN. Concurrent writes to CurrentStorage do NOT
/// affect this snapshot's iteration.
#[derive(Debug, Clone)]
pub struct CurrentStorageSnapshot {
    /// LSN at which snapshot was taken
    lsn: LSN,
    /// Arc references to nodes (cheap, ~8 bytes each)
    nodes: Vec<Arc<Node>>,
    /// Arc references to edges (cheap, ~8 bytes each)
    edges: Vec<Arc<Edge>>,
}

impl CurrentStorageSnapshot {
    /// Create a new snapshot from collected node and edge references.
    ///
    /// # Arguments
    ///
    /// * `lsn` - LSN at which snapshot was taken
    /// * `nodes` - Arc references to nodes at snapshot time
    /// * `edges` - Arc references to edges at snapshot time
    pub fn new(lsn: LSN, nodes: Vec<Arc<Node>>, edges: Vec<Arc<Edge>>) -> Self {
        Self { lsn, nodes, edges }
    }

    /// Get nodes as a slice (for testing/debugging)
    #[cfg(test)]
    pub fn nodes(&self) -> &[Arc<Node>] {
        &self.nodes
    }

    /// Get edges as a slice (for testing/debugging)
    #[cfg(test)]
    pub fn edges(&self) -> &[Arc<Edge>] {
        &self.edges
    }
}

impl StorageSnapshot for CurrentStorageSnapshot {
    type NodeIter = CurrentNodeIterator;
    type EdgeIter = CurrentEdgeIterator;

    fn lsn(&self) -> LSN {
        self.lsn
    }

    fn node_count(&self) -> usize {
        self.nodes.len()
    }

    fn edge_count(&self) -> usize {
        self.edges.len()
    }

    fn iter_nodes(&self) -> Self::NodeIter {
        CurrentNodeIterator {
            nodes: self.nodes.clone(),
            index: 0,
        }
    }

    fn iter_edges(&self) -> Self::EdgeIter {
        CurrentEdgeIterator {
            edges: self.edges.clone(),
            index: 0,
        }
    }
}

/// Iterator over nodes in CurrentStorageSnapshot.
///
/// Clones nodes from Arc references during iteration (streaming).
/// No Vec<Node> allocation - memory efficient for large graphs.
pub struct CurrentNodeIterator {
    nodes: Vec<Arc<Node>>,
    index: usize,
}

impl Iterator for CurrentNodeIterator {
    type Item = Node;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.nodes.len() {
            let node = (*self.nodes[self.index]).clone();
            self.index += 1;
            Some(node)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.nodes.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for CurrentNodeIterator {
    fn len(&self) -> usize {
        self.nodes.len() - self.index
    }
}

/// Iterator over edges in CurrentStorageSnapshot.
///
/// Clones edges from Arc references during iteration (streaming).
/// No Vec<Edge> allocation - memory efficient for large graphs.
pub struct CurrentEdgeIterator {
    edges: Vec<Arc<Edge>>,
    index: usize,
}

impl Iterator for CurrentEdgeIterator {
    type Item = Edge;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.edges.len() {
            let edge = (*self.edges[self.index]).clone();
            self.index += 1;
            Some(edge)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.edges.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for CurrentEdgeIterator {
    fn len(&self) -> usize {
        self.edges.len() - self.index
    }
}

/// Snapshot of HistoricalStorage at a specific LSN.
///
/// Captures all node and edge versions visible at the snapshot LSN.
/// Uses Arc-based COW for memory efficiency.
#[derive(Debug, Clone)]
pub struct HistoricalStorageSnapshot {
    /// LSN at which snapshot was taken
    lsn: LSN,
    /// Arc references to node versions
    node_versions: Vec<Arc<NodeVersion>>,
    /// Arc references to edge versions
    edge_versions: Vec<Arc<EdgeVersion>>,
}

impl HistoricalStorageSnapshot {
    /// Create a new historical snapshot.
    pub fn new(
        lsn: LSN,
        node_versions: Vec<Arc<NodeVersion>>,
        edge_versions: Vec<Arc<EdgeVersion>>,
    ) -> Self {
        Self {
            lsn,
            node_versions,
            edge_versions,
        }
    }

    /// Get LSN at which snapshot was taken
    pub fn lsn(&self) -> LSN {
        self.lsn
    }

    /// Get number of node versions in snapshot
    pub fn node_version_count(&self) -> usize {
        self.node_versions.len()
    }

    /// Get number of edge versions in snapshot
    pub fn edge_version_count(&self) -> usize {
        self.edge_versions.len()
    }

    /// Iterate over node versions in snapshot (streaming)
    pub fn iter_node_versions(&self) -> HistoricalNodeVersionIterator {
        HistoricalNodeVersionIterator {
            versions: self.node_versions.clone(),
            index: 0,
        }
    }

    /// Iterate over edge versions in snapshot (streaming)
    pub fn iter_edge_versions(&self) -> HistoricalEdgeVersionIterator {
        HistoricalEdgeVersionIterator {
            versions: self.edge_versions.clone(),
            index: 0,
        }
    }
}

/// Iterator over node versions in HistoricalStorageSnapshot.
pub struct HistoricalNodeVersionIterator {
    versions: Vec<Arc<NodeVersion>>,
    index: usize,
}

impl Iterator for HistoricalNodeVersionIterator {
    type Item = Arc<NodeVersion>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.versions.len() {
            let version = self.versions[self.index].clone();
            self.index += 1;
            Some(version)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.versions.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for HistoricalNodeVersionIterator {
    fn len(&self) -> usize {
        self.versions.len() - self.index
    }
}

/// Iterator over edge versions in HistoricalStorageSnapshot.
pub struct HistoricalEdgeVersionIterator {
    versions: Vec<Arc<EdgeVersion>>,
    index: usize,
}

impl Iterator for HistoricalEdgeVersionIterator {
    type Item = Arc<EdgeVersion>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.versions.len() {
            let version = self.versions[self.index].clone();
            self.index += 1;
            Some(version)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.versions.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for HistoricalEdgeVersionIterator {
    fn len(&self) -> usize {
        self.versions.len() - self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::{NodeId, VersionId};
    use crate::core::interning::InternedString;
    use crate::core::property::PropertyMap;

    #[test]
    fn test_current_snapshot_creation() {
        let lsn = LSN(42);
        let nodes = vec![Arc::new(Node::new(
            NodeId::new(1).unwrap(),
            InternedString::from_raw(1),
            PropertyMap::new(),
            VersionId::new(1).unwrap(),
        ))];
        let edges = vec![];

        let snapshot = CurrentStorageSnapshot::new(lsn, nodes, edges);

        assert_eq!(snapshot.lsn(), LSN(42));
        assert_eq!(snapshot.node_count(), 1);
        assert_eq!(snapshot.edge_count(), 0);
    }

    #[test]
    fn test_current_snapshot_iteration() {
        let lsn = LSN(10);
        let nodes = vec![
            Arc::new(Node::new(
                NodeId::new(1).unwrap(),
                InternedString::from_raw(1),
                PropertyMap::new(),
                VersionId::new(1).unwrap(),
            )),
            Arc::new(Node::new(
                NodeId::new(2).unwrap(),
                InternedString::from_raw(1),
                PropertyMap::new(),
                VersionId::new(2).unwrap(),
            )),
        ];

        let snapshot = CurrentStorageSnapshot::new(lsn, nodes, vec![]);

        let collected: Vec<Node> = snapshot.iter_nodes().collect();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].id, NodeId::new(1).unwrap());
        assert_eq!(collected[1].id, NodeId::new(2).unwrap());
    }

    #[test]
    fn test_iterator_exact_size() {
        let lsn = LSN(10);
        let nodes = vec![
            Arc::new(Node::new(
                NodeId::new(1).unwrap(),
                InternedString::from_raw(1),
                PropertyMap::new(),
                VersionId::new(1).unwrap(),
            )),
            Arc::new(Node::new(
                NodeId::new(2).unwrap(),
                InternedString::from_raw(1),
                PropertyMap::new(),
                VersionId::new(2).unwrap(),
            )),
        ];

        let snapshot = CurrentStorageSnapshot::new(lsn, nodes, vec![]);
        let mut iter = snapshot.iter_nodes();

        assert_eq!(iter.len(), 2);
        iter.next();
        assert_eq!(iter.len(), 1);
        iter.next();
        assert_eq!(iter.len(), 0);
    }
}
