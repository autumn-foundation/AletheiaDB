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
use crate::core::error::StorageError;

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
    /// **Atomicity**: Holds locks for both nodes throughout the operation to prevent
    /// TOCTOU race conditions. Acquires locks in consistent order (by node ID) to
    /// prevent deadlock when two threads insert edges in opposite directions.
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
        // Handle self-loops separately to avoid borrow checker issues
        if source == target {
            // For self-loops, acquire BOTH outgoing and incoming locks atomically
            let mut outgoing_lock = self.outgoing.entry(source).or_default();
            let mut incoming_lock = self.incoming.entry(source).or_default();

            // Check capacity for BOTH indexes while holding both locks
            if outgoing_lock.len() >= self.config.max_entries_per_node {
                return Err(StorageError::CapacityExceeded {
                    resource: format!("temporal adjacency outgoing entries for node {}", source),
                    current: outgoing_lock.len(),
                    limit: self.config.max_entries_per_node,
                });
            }

            if incoming_lock.len() >= self.config.max_entries_per_node {
                return Err(StorageError::CapacityExceeded {
                    resource: format!("temporal adjacency incoming entries for node {}", source),
                    current: incoming_lock.len(),
                    limit: self.config.max_entries_per_node,
                });
            }

            // Insert into outgoing while holding lock
            let entry_out = TemporalAdjacencyEntry {
                edge_id,
                neighbor: target,
                label,
                valid_from,
                valid_to,
                tx_from,
                tx_to,
            };
            let pos = outgoing_lock
                .binary_search_by_key(&valid_from, |e| e.valid_from)
                .unwrap_or_else(|pos| pos);
            outgoing_lock.insert(pos, entry_out);

            // Insert into incoming while holding lock
            let entry_in = TemporalAdjacencyEntry {
                edge_id,
                neighbor: source,
                label,
                valid_from,
                valid_to,
                tx_from,
                tx_to,
            };
            let pos = incoming_lock
                .binary_search_by_key(&valid_from, |e| e.valid_from)
                .unwrap_or_else(|pos| pos);
            incoming_lock.insert(pos, entry_in);

            return Ok(());
        }

        // Normal case: source != target
        // Acquire locks in consistent order (by node ID) to prevent deadlock
        if source.as_u64() < target.as_u64() {
            // Acquire source (outgoing) first, then target (incoming)
            let mut outgoing_lock = self.outgoing.entry(source).or_default();
            let mut incoming_lock = self.incoming.entry(target).or_default();

            // Check capacity while holding both locks
            if outgoing_lock.len() >= self.config.max_entries_per_node {
                return Err(StorageError::CapacityExceeded {
                    resource: format!("temporal adjacency entries for node {}", source),
                    current: outgoing_lock.len(),
                    limit: self.config.max_entries_per_node,
                });
            }

            if incoming_lock.len() >= self.config.max_entries_per_node {
                return Err(StorageError::CapacityExceeded {
                    resource: format!("temporal adjacency entries for node {}", target),
                    current: incoming_lock.len(),
                    limit: self.config.max_entries_per_node,
                });
            }

            // Insert while holding both locks
            let entry_out = TemporalAdjacencyEntry {
                edge_id,
                neighbor: target,
                label,
                valid_from,
                valid_to,
                tx_from,
                tx_to,
            };
            let pos = outgoing_lock
                .binary_search_by_key(&valid_from, |e| e.valid_from)
                .unwrap_or_else(|pos| pos);
            outgoing_lock.insert(pos, entry_out);

            let entry_in = TemporalAdjacencyEntry {
                edge_id,
                neighbor: source,
                label,
                valid_from,
                valid_to,
                tx_from,
                tx_to,
            };
            let pos = incoming_lock
                .binary_search_by_key(&valid_from, |e| e.valid_from)
                .unwrap_or_else(|pos| pos);
            incoming_lock.insert(pos, entry_in);
        } else {
            // Acquire target (incoming) first, then source (outgoing) - reversed order
            let mut incoming_lock = self.incoming.entry(target).or_default();
            let mut outgoing_lock = self.outgoing.entry(source).or_default();

            // Check capacity while holding both locks
            if outgoing_lock.len() >= self.config.max_entries_per_node {
                return Err(StorageError::CapacityExceeded {
                    resource: format!("temporal adjacency entries for node {}", source),
                    current: outgoing_lock.len(),
                    limit: self.config.max_entries_per_node,
                });
            }

            if incoming_lock.len() >= self.config.max_entries_per_node {
                return Err(StorageError::CapacityExceeded {
                    resource: format!("temporal adjacency entries for node {}", target),
                    current: incoming_lock.len(),
                    limit: self.config.max_entries_per_node,
                });
            }

            // Insert while holding both locks
            let entry_out = TemporalAdjacencyEntry {
                edge_id,
                neighbor: target,
                label,
                valid_from,
                valid_to,
                tx_from,
                tx_to,
            };
            let pos = outgoing_lock
                .binary_search_by_key(&valid_from, |e| e.valid_from)
                .unwrap_or_else(|pos| pos);
            outgoing_lock.insert(pos, entry_out);

            let entry_in = TemporalAdjacencyEntry {
                edge_id,
                neighbor: source,
                label,
                valid_from,
                valid_to,
                tx_from,
                tx_to,
            };
            let pos = incoming_lock
                .binary_search_by_key(&valid_from, |e| e.valid_from)
                .unwrap_or_else(|pos| pos);
            incoming_lock.insert(pos, entry_in);
        }

        Ok(())
    }

    /// Get outgoing edges from a node at a specific time.
    ///
    /// Returns a deduplicated list of edges that were valid at the given time.
    /// If multiple versions of the same edge match, the edge is returned once.
    ///
    /// **Deduplication**: Necessary because multiple versions of an edge (with
    /// different valid_from times) can be valid at the same query point. For
    /// example, an edge created at t0, deleted at t1, and recreated at t2 will
    /// have multiple entries, but only one should be returned per query.
    ///
    /// **Performance**: O(log N + K) where N = total entries, K = matching entries.
    /// Uses binary search to find the first potentially valid entry, then scans
    /// backward through candidates that could overlap the query time.
    ///
    /// **Future Optimization**: Consider using SmallVec or inline array for
    /// common case of <10 edges to avoid HashSet allocation overhead.
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

    fn ts(t: i64) -> Timestamp {
        time::from_secs(t)
    }

    #[test]
    fn test_entry_is_valid_at() {
        let t0 = ts(10);
        let t1 = ts(20);
        let t2 = ts(30);

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

    #[test]
    fn test_insert_and_retrieve_outgoing() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(10),
                ts(20), // valid range [10, 20)
                ts(5),
                TIMESTAMP_MAX, // tx range [5, MAX)
            )
            .unwrap();

        // Valid query
        let result = index.get_outgoing_at_time(source, ts(15), ts(10));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], edge);

        // Invalid query (time before valid_from)
        let result = index.get_outgoing_at_time(source, ts(5), ts(10));
        assert!(result.is_empty());

        // Invalid query (time after valid_to)
        let result = index.get_outgoing_at_time(source, ts(25), ts(10));
        assert!(result.is_empty());
    }

    #[test]
    fn test_insert_and_retrieve_incoming() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(10),
                ts(20),
                ts(5),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Incoming to target
        let result = index.get_incoming_at_time(target, ts(15), ts(10));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], edge);

        // Incoming to source (should be empty)
        let result = index.get_incoming_at_time(source, ts(15), ts(10));
        assert!(result.is_empty());
    }

    #[test]
    fn test_temporal_validity() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(100),
                ts(200),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Valid boundary (inclusive start)
        assert_eq!(index.get_outgoing_at_time(source, ts(100), ts(10)).len(), 1);

        // Invalid boundary (exclusive end)
        assert_eq!(index.get_outgoing_at_time(source, ts(200), ts(10)).len(), 0);

        // Just before end
        assert_eq!(index.get_outgoing_at_time(source, ts(199), ts(10)).len(), 1);
    }

    #[test]
    fn test_transaction_visibility() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(10),
                ts(20),
                ts(100),
                ts(200), // Recorded between 100 and 200
            )
            .unwrap();

        // Query at valid time 15, tx time 150 (visible)
        assert_eq!(index.get_outgoing_at_time(source, ts(15), ts(150)).len(), 1);

        // Query at valid time 15, tx time 50 (before recorded)
        assert_eq!(index.get_outgoing_at_time(source, ts(15), ts(50)).len(), 0);

        // Query at valid time 15, tx time 250 (after superseded/deleted)
        assert_eq!(index.get_outgoing_at_time(source, ts(15), ts(250)).len(), 0);
    }

    #[test]
    fn test_edge_updates() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        // Insert open-ended edge
        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(10),
                ts(100), // Initially valid until 100
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        assert_eq!(index.get_outgoing_at_time(source, ts(50), ts(10)).len(), 1);

        // Close valid time at 40
        index.close_edge_valid_time(edge, source, target, ts(40));

        // Should be valid at 30
        assert_eq!(index.get_outgoing_at_time(source, ts(30), ts(10)).len(), 1);

        // Should NOT be valid at 50 anymore
        assert_eq!(index.get_outgoing_at_time(source, ts(50), ts(10)).len(), 0);
    }

    #[test]
    fn test_capacity_limit() {
        let config = TemporalAdjacencyConfig {
            max_entries_per_node: 2,
        };
        let index = TemporalAdjacencyIndex::new(config);
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label = InternedString::from_raw(1);

        // Insert 2 edges (limit reached)
        index
            .insert_edge(
                EdgeId::new(1).unwrap(),
                source,
                target,
                label,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();
        index
            .insert_edge(
                EdgeId::new(2).unwrap(),
                source,
                target,
                label,
                ts(20),
                ts(30),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Insert 3rd edge (should fail)
        let result = index.insert_edge(
            EdgeId::new(3).unwrap(),
            source,
            target,
            label,
            ts(30),
            ts(40),
            ts(0),
            TIMESTAMP_MAX,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            StorageError::CapacityExceeded { .. }
        ));
    }

    #[test]
    fn test_self_loop() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let node = NodeId::new(1).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        // Insert self-loop
        index
            .insert_edge(
                edge,
                node,
                node,
                label,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Should be outgoing from node
        let out_res = index.get_outgoing_at_time(node, ts(15), ts(10));
        assert_eq!(out_res.len(), 1);
        assert_eq!(out_res[0], edge);

        // Should be incoming to node
        let in_res = index.get_incoming_at_time(node, ts(15), ts(10));
        assert_eq!(in_res.len(), 1);
        assert_eq!(in_res[0], edge);
    }

    #[test]
    fn test_multiple_versions_deduplication() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap(); // Same edge ID
        let label = InternedString::from_raw(1);

        // Version 1: [10, 20)
        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Version 2: [30, 40)
        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(30),
                ts(40),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Query at 15 -> find V1
        assert_eq!(index.get_outgoing_at_time(source, ts(15), ts(10)).len(), 1);

        // Query at 35 -> find V2 (same edge ID)
        assert_eq!(index.get_outgoing_at_time(source, ts(35), ts(10)).len(), 1);

        // Query at 25 -> no version valid
        assert_eq!(index.get_outgoing_at_time(source, ts(25), ts(10)).len(), 0);
    }

    #[test]
    fn test_lock_ordering_deadlock_prevention() {
        // This test ensures that the insertion logic handles node ID ordering correctly.
        // While we can't easily detect deadlocks in a unit test, we can verify that insertions work
        // regardless of node ID order (source < target vs source > target).

        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let small_id = NodeId::new(10).unwrap();
        let large_id = NodeId::new(20).unwrap();
        let label = InternedString::from_raw(1);

        // Case 1: source < target
        index
            .insert_edge(
                EdgeId::new(1).unwrap(),
                small_id,
                large_id,
                label,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Case 2: source > target
        index
            .insert_edge(
                EdgeId::new(2).unwrap(),
                large_id,
                small_id,
                label,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Verify both were inserted
        assert_eq!(
            index.get_outgoing_at_time(small_id, ts(15), ts(10)).len(),
            1
        );
        assert_eq!(
            index.get_outgoing_at_time(large_id, ts(15), ts(10)).len(),
            1
        );
    }

    #[test]
    fn test_get_outgoing_with_label() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label1 = InternedString::from_raw(1);
        let label2 = InternedString::from_raw(2);

        index
            .insert_edge(
                EdgeId::new(1).unwrap(),
                source,
                target,
                label1,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();
        index
            .insert_edge(
                EdgeId::new(2).unwrap(),
                source,
                target,
                label2,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        let res1 = index.get_outgoing_with_label_at_time(source, label1, ts(15), ts(10));
        assert_eq!(res1.len(), 1);
        assert_eq!(res1[0], EdgeId::new(1).unwrap());

        let res2 = index.get_outgoing_with_label_at_time(source, label2, ts(15), ts(10));
        assert_eq!(res2.len(), 1);
        assert_eq!(res2[0], EdgeId::new(2).unwrap());
    }
}
