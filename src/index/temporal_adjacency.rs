//! Temporal Adjacency Index for fast temporal graph traversal.
//!
//! This module provides an index that maps (node_id, time) -> edge_ids, enabling
//! efficient queries for edges that existed at a specific point in time. This is
//! critical for `SemanticPathfinder::find_path_at_time()` to find paths through
//! deleted edges.
//!
//! # Architecture
//!
//! - **Outgoing Index**: Maps source nodes to their outgoing edges
//! - **Incoming Index**: Maps target nodes to their incoming edges
//! - **Temporal Ordering**: Entries sorted by valid_from for binary search
//! - **DoS Protection**: Configurable max entries per node
//!
//! # Performance
//!
//! - Insert: O(log N) - binary search + shift for sorted insertion
//! - Query: O(log N + K) - binary search + scan K matching entries
//! - Memory: ~64 bytes per entry

use dashmap::DashMap;

use crate::core::{EdgeId, InternedString, NodeId, Timestamp};
use crate::utils::error::StorageError;

/// Configuration for temporal adjacency index.
#[derive(Debug, Clone)]
pub struct TemporalAdjacencyConfig {
    /// Maximum entries per node (DoS protection).
    pub max_entries_per_node: usize,
}

impl Default for TemporalAdjacencyConfig {
    fn default() -> Self {
        Self {
            max_entries_per_node: 1_000_000, // 1M edges per node
        }
    }
}

/// A single temporal adjacency entry.
///
/// Tracks an edge's temporal validity for quick lookup.
/// This struct is exposed for persistence but not part of the public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub struct TemporalAdjacencyEntry {
    /// Edge ID
    pub edge_id: EdgeId,
    /// Neighbor node (target for outgoing, source for incoming)
    pub neighbor: NodeId,
    /// Edge label
    pub label: InternedString,
    /// Valid time range start
    pub valid_from: Timestamp,
    /// Valid time range end
    pub valid_to: Timestamp,
    /// Transaction time range start
    pub tx_from: Timestamp,
    /// Transaction time range end
    pub tx_to: Timestamp,
}

impl TemporalAdjacencyEntry {
    /// Check if this entry is valid at the given time.
    #[inline]
    fn is_valid_at(&self, valid_time: Timestamp, tx_time: Timestamp) -> bool {
        valid_time >= self.valid_from
            && valid_time < self.valid_to
            && tx_time >= self.tx_from
            && tx_time < self.tx_to
    }
}

/// Temporal adjacency index for graph traversal at specific points in time.
pub struct TemporalAdjacencyIndex {
    /// Outgoing edges: source_node -> [entries]
    pub(crate) outgoing: DashMap<NodeId, Vec<TemporalAdjacencyEntry>>,
    /// Incoming edges: target_node -> [entries]
    pub(crate) incoming: DashMap<NodeId, Vec<TemporalAdjacencyEntry>>,
    /// Configuration
    config: TemporalAdjacencyConfig,
}

impl TemporalAdjacencyIndex {
    /// Create a new temporal adjacency index.
    pub fn new(config: TemporalAdjacencyConfig) -> Self {
        Self {
            outgoing: DashMap::new(),
            incoming: DashMap::new(),
            config,
        }
    }

    /// Close the valid time of an edge's most recent entry.
    ///
    /// This updates the valid_to timestamp of the most recent entry for the given edge.
    /// This is called when a new version of an edge is created, closing the previous version.
    pub fn close_edge_valid_time(
        &self,
        edge_id: EdgeId,
        source: NodeId,
        target: NodeId,
        valid_end: Timestamp,
    ) {
        // Update outgoing index
        if let Some(mut entries) = self.outgoing.get_mut(&source)
            // Find the most recent entry for this edge (should be last due to sorted insertion)
            && let Some(entry) = entries.iter_mut().rev().find(|e| e.edge_id == edge_id)
        {
            entry.valid_to = valid_end;
        }

        // Update incoming index
        if let Some(mut entries) = self.incoming.get_mut(&target)
            && let Some(entry) = entries.iter_mut().rev().find(|e| e.edge_id == edge_id)
        {
            entry.valid_to = valid_end;
        }
    }

    /// Close the transaction time of an edge's most recent entry.
    ///
    /// This updates the tx_to timestamp of the most recent entry for the given edge.
    /// This is called when an edge is deleted or superseded, closing its transaction time.
    pub fn close_edge_transaction_time(
        &self,
        edge_id: EdgeId,
        source: NodeId,
        target: NodeId,
        tx_end: Timestamp,
    ) {
        // Update outgoing index
        if let Some(mut entries) = self.outgoing.get_mut(&source)
            // Find the most recent entry for this edge (should be last due to sorted insertion)
            && let Some(entry) = entries.iter_mut().rev().find(|e| e.edge_id == edge_id)
        {
            entry.tx_to = tx_end;
        }

        // Update incoming index
        if let Some(mut entries) = self.incoming.get_mut(&target)
            && let Some(entry) = entries.iter_mut().rev().find(|e| e.edge_id == edge_id)
        {
            entry.tx_to = tx_end;
        }
    }

    /// Insert an edge into the index.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::CapacityExceeded` if max entries per node exceeded.
    ///
    /// **Atomicity**: Pre-checks capacity for both nodes before any mutations to
    /// prevent inconsistent state if one node exceeds capacity.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_edge(
        &self,
        edge_id: EdgeId,
        source: NodeId,
        target: NodeId,
        label: InternedString,
        valid_from: Timestamp,
        valid_to: Timestamp,
        tx_from: Timestamp,
        tx_to: Timestamp,
    ) -> Result<(), StorageError> {
        // Pre-check capacity for both nodes BEFORE any mutations
        // This prevents inconsistent state where outgoing is inserted but incoming fails
        let outgoing_entries = self.outgoing.entry(source).or_default();
        if outgoing_entries.len() >= self.config.max_entries_per_node {
            return Err(StorageError::CapacityExceeded {
                resource: format!("temporal adjacency entries for node {}", source),
                current: outgoing_entries.len(),
                limit: self.config.max_entries_per_node,
            });
        }
        drop(outgoing_entries);

        let incoming_entries = self.incoming.entry(target).or_default();
        if incoming_entries.len() >= self.config.max_entries_per_node {
            return Err(StorageError::CapacityExceeded {
                resource: format!("temporal adjacency entries for node {}", target),
                current: incoming_entries.len(),
                limit: self.config.max_entries_per_node,
            });
        }
        drop(incoming_entries);

        // Now safe to insert into both indexes
        let entry = TemporalAdjacencyEntry {
            edge_id,
            neighbor: target,
            label,
            valid_from,
            valid_to,
            tx_from,
            tx_to,
        };

        // Insert into outgoing index
        let mut outgoing_entries = self.outgoing.entry(source).or_default();
        let pos = outgoing_entries
            .binary_search_by_key(&valid_from, |e| e.valid_from)
            .unwrap_or_else(|pos| pos);
        outgoing_entries.insert(pos, entry);
        drop(outgoing_entries);

        // Insert into incoming index
        let entry_incoming = TemporalAdjacencyEntry {
            edge_id,
            neighbor: source, // For incoming, neighbor is the source
            label,
            valid_from,
            valid_to,
            tx_from,
            tx_to,
        };

        let mut incoming_entries = self.incoming.entry(target).or_default();
        let pos = incoming_entries
            .binary_search_by_key(&valid_from, |e| e.valid_from)
            .unwrap_or_else(|pos| pos);
        incoming_entries.insert(pos, entry_incoming);

        Ok(())
    }

    /// Get outgoing edges from a node at a specific time.
    ///
    /// Returns a deduplicated list of edges that were valid at the given time.
    /// If multiple versions of the same edge match, the edge is returned once.
    ///
    /// **Performance**: O(log N + K) where N = total entries, K = matching entries.
    /// Uses binary search to find the first potentially valid entry, then scans
    /// forward only through candidates that could overlap the query time.
    pub fn get_outgoing_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        self.outgoing
            .get(&node_id)
            .map(|entries| {
                let mut seen = std::collections::HashSet::new();

                // Binary search to find first entry where valid_from <= valid_time
                // Entries are sorted by valid_from, so we scan from this point forward
                let start_idx = entries.partition_point(|e| e.valid_from <= valid_time);

                // Scan backward from start_idx to find all entries that could be valid at valid_time
                // An entry at index i could be valid if valid_from <= valid_time < valid_to
                entries[..start_idx]
                    .iter()
                    .rev()
                    .take_while(|e| e.valid_to > valid_time) // Stop when valid_to <= valid_time
                    .filter(|e| e.is_valid_at(valid_time, tx_time))
                    .filter_map(|e| {
                        if seen.insert(e.edge_id) {
                            Some(e.edge_id)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get incoming edges to a node at a specific time.
    ///
    /// Returns a deduplicated list of edges that were valid at the given time.
    /// If multiple versions of the same edge match, the edge is returned once.
    ///
    /// **Performance**: O(log N + K) where N = total entries, K = matching entries.
    /// Uses binary search to find the first potentially valid entry, then scans
    /// forward only through candidates that could overlap the query time.
    pub fn get_incoming_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        self.incoming
            .get(&node_id)
            .map(|entries| {
                let mut seen = std::collections::HashSet::new();

                // Binary search to find first entry where valid_from <= valid_time
                let start_idx = entries.partition_point(|e| e.valid_from <= valid_time);

                // Scan backward from start_idx to find all entries that could be valid at valid_time
                entries[..start_idx]
                    .iter()
                    .rev()
                    .take_while(|e| e.valid_to > valid_time)
                    .filter(|e| e.is_valid_at(valid_time, tx_time))
                    .filter_map(|e| {
                        if seen.insert(e.edge_id) {
                            Some(e.edge_id)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get outgoing edges with a specific label from a node at a specific time.
    ///
    /// Returns a deduplicated list of edges that were valid at the given time.
    /// If multiple versions of the same edge match, the edge is returned once.
    ///
    /// **Performance**: O(log N + K) where N = total entries, K = matching entries.
    /// Uses binary search to find the first potentially valid entry, then scans
    /// forward only through candidates that could overlap the query time.
    pub fn get_outgoing_with_label_at_time(
        &self,
        node_id: NodeId,
        label: InternedString,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        self.outgoing
            .get(&node_id)
            .map(|entries| {
                let mut seen = std::collections::HashSet::new();

                // Binary search to find first entry where valid_from <= valid_time
                let start_idx = entries.partition_point(|e| e.valid_from <= valid_time);

                // Scan backward from start_idx to find all entries that could be valid at valid_time
                entries[..start_idx]
                    .iter()
                    .rev()
                    .take_while(|e| e.valid_to > valid_time)
                    .filter(|e| e.label == label && e.is_valid_at(valid_time, tx_time))
                    .filter_map(|e| {
                        if seen.insert(e.edge_id) {
                            Some(e.edge_id)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::TIMESTAMP_MAX;
    use crate::core::temporal::time;

    #[test]
    fn test_entry_is_valid_at() {
        let t0 = time::now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t1 = time::now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t2 = time::now();

        let entry = TemporalAdjacencyEntry {
            edge_id: EdgeId::new(1).unwrap(),
            neighbor: NodeId::new(100).unwrap(),
            label: InternedString::from_raw(1),
            valid_from: t0,
            valid_to: t2,
            tx_from: t0,
            tx_to: TIMESTAMP_MAX,
        };

        // Valid at t1 (between t0 and t2)
        assert!(entry.is_valid_at(t1, t1));

        // Not valid at or after t2
        assert!(!entry.is_valid_at(t2, t1));
    }
}
