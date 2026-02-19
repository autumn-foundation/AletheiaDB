use super::HistoricalStorage;
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::temporal::Timestamp;
use crate::core::version::{EdgeVersion, EntityVersion, NodeVersion};
use std::collections::HashMap;
use std::sync::Arc;

impl HistoricalStorage {
    /// Get a node version by ID.
    pub fn get_node_version(&self, version_id: VersionId) -> Option<&NodeVersion> {
        self.node_versions.get(&version_id)
    }

    /// Get an edge version by ID.
    pub fn get_edge_version(&self, version_id: VersionId) -> Option<&EdgeVersion> {
        self.edge_versions.get(&version_id)
    }

    /// Get the current version ID for a node.
    pub fn get_current_node_version(&self, node_id: NodeId) -> Option<VersionId> {
        self.node_version_heads.get(&node_id).copied()
    }

    /// Get the current version ID for an edge.
    pub fn get_current_edge_version(&self, edge_id: EdgeId) -> Option<VersionId> {
        self.edge_version_heads.get(&edge_id).copied()
    }

    /// Set the temporal indexes for optimized version lookups (Issue #209).
    ///
    /// When temporal indexes are configured, `find_node_version_at_time` and
    /// `find_edge_version_at_time` will use O(log n) binary search instead of
    /// O(n) linear scans through version chains.
    ///
    /// This is typically called during database initialization to share the
    /// temporal indexes between the database and historical storage.
    pub fn set_temporal_indexes(&mut self, indexes: Arc<crate::index::temporal::TemporalIndexes>) {
        self.temporal_indexes = Some(indexes);
    }

    /// Set the temporal adjacency index for this storage.
    ///
    /// When the temporal adjacency index is set, it will be automatically updated
    /// when edges are added or modified, enabling efficient temporal pathfinding
    /// queries that can find paths through deleted edges.
    ///
    /// This is typically called during database initialization.
    pub fn set_temporal_adjacency_index(
        &mut self,
        index: Arc<crate::index::temporal_adjacency::TemporalAdjacencyIndex>,
    ) {
        self.temporal_adjacency_index = Some(index);
    }

    /// Get a reference to the temporal adjacency index if configured.
    ///
    /// Used by persistence layer to save the index to disk.
    pub fn get_temporal_adjacency_index(
        &self,
    ) -> Option<&Arc<crate::index::temporal_adjacency::TemporalAdjacencyIndex>> {
        self.temporal_adjacency_index.as_ref()
    }

    /// Find a node version valid at a specific point in time.
    ///
    /// **Performance (Issue #209)**:
    /// - **With temporal indexes**: O(log N) binary search where N = version count
    /// - **Without temporal indexes**: O(N) linear scan through version chain
    ///
    /// When temporal indexes are configured via `set_temporal_indexes()`, this
    /// method uses efficient binary search. Otherwise, it falls back to walking
    /// the version chain. For entities with 100s-1000s of versions, the temporal
    /// index provides significant performance improvements (10-100x faster).
    ///
    /// # Arguments
    /// * `node_id` - The node to query
    /// * `valid_time` - When the fact was true in reality
    /// * `transaction_time` - When the fact was recorded in the database
    ///
    /// # Returns
    /// The version ID visible at the given bi-temporal point, or None if no
    /// version exists at that time.
    pub fn find_node_version_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Option<VersionId> {
        // Fast path: Use temporal index if available (O(log n)) - Issue #209
        // The temporal indexes are now properly updated when intervals are closed
        if let Some(ref indexes) = self.temporal_indexes {
            return indexes
                .find_node_version_at_point_iter(node_id, valid_time, transaction_time)
                .find(|&version_id| {
                    // Robustness check: verify visibility against actual version data
                    self.node_versions
                        .get(&version_id)
                        .map(|v| v.temporal.is_visible_at(valid_time, transaction_time))
                        .unwrap_or(false)
                });
        }

        // Fallback: Linear scan through version chain (O(n))
        // This is only used when temporal indexes are not configured
        let mut current_id = self.node_version_heads.get(&node_id).copied()?;

        loop {
            let version = self.node_versions.get(&current_id)?;

            // Check if this version's temporal interval contains the query time
            if version.temporal.is_visible_at(valid_time, transaction_time) {
                return Some(current_id);
            }

            // Move to previous version
            current_id = version.prev_version?;
        }
    }

    /// Find an edge version valid at a specific point in time.
    ///
    /// **Performance (Issue #209)**:
    /// - **With temporal indexes**: O(log N) binary search where N = version count
    /// - **Without temporal indexes**: O(N) linear scan through version chain
    ///
    /// When temporal indexes are configured via `set_temporal_indexes()`, this
    /// method uses efficient binary search. Otherwise, it falls back to walking
    /// the version chain. For entities with 100s-1000s of versions, the temporal
    /// index provides significant performance improvements (10-100x faster).
    ///
    /// # Arguments
    /// * `edge_id` - The edge to query
    /// * `valid_time` - When the fact was true in reality
    /// * `transaction_time` - When the fact was recorded in the database
    ///
    /// # Returns
    /// The version ID visible at the given bi-temporal point, or None if no
    /// version exists at that time.
    pub fn find_edge_version_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Option<VersionId> {
        // Fast path: Use temporal index if available (O(log n)) - Issue #209
        // The temporal indexes are now properly updated when intervals are closed
        if let Some(ref indexes) = self.temporal_indexes {
            return indexes
                .find_edge_version_at_point_iter(edge_id, valid_time, transaction_time)
                .find(|&version_id| {
                    // Robustness check: verify visibility against actual version data
                    self.edge_versions
                        .get(&version_id)
                        .map(|v| v.temporal.is_visible_at(valid_time, transaction_time))
                        .unwrap_or(false)
                });
        }

        // Fallback: Linear scan through version chain (O(n))
        // This is only used when temporal indexes are not configured
        let mut current_id = self.edge_version_heads.get(&edge_id).copied()?;

        loop {
            let version = self.edge_versions.get(&current_id)?;

            if version.temporal.is_visible_at(valid_time, transaction_time) {
                return Some(current_id);
            }

            current_id = version.prev_version?;
        }
    }

    /// Get all node versions for all nodes.
    ///
    /// Returns a map of NodeId -> `Vec<NodeVersion>` for recovery property tests.
    /// This walks through all node versions and groups them by entity ID.
    pub fn get_all_node_versions(&self) -> HashMap<NodeId, Vec<&NodeVersion>> {
        let mut result: HashMap<NodeId, Vec<&NodeVersion>> = HashMap::new();

        for version in self.node_versions.values() {
            result.entry(version.node_id).or_default().push(version);
        }

        result
    }

    /// Get all edge versions for all edges.
    ///
    /// Returns a map of EdgeId -> `Vec<EdgeVersion>` for recovery property tests.
    /// This walks through all edge versions and groups them by entity ID.
    pub fn get_all_edge_versions(&self) -> HashMap<EdgeId, Vec<&EdgeVersion>> {
        let mut result: HashMap<EdgeId, Vec<&EdgeVersion>> = HashMap::new();

        for version in self.edge_versions.values() {
            result.entry(version.edge_id).or_default().push(version);
        }

        result
    }

    /// Get an iterator over all node versions (test-only helper).
    ///
    /// This method provides access to the node versions for integration test
    /// verification purposes. It is public to allow access from integration tests
    /// but is hidden from documentation and marked with `__test_` prefix to
    /// discourage production use.
    ///
    /// **Warning**: This method exposes internal implementation details and
    /// should only be used in tests.
    #[doc(hidden)]
    pub fn __test_get_node_versions_iterator(&self) -> impl Iterator<Item = &NodeVersion> {
        self.node_versions.values()
    }

    /// Get all node versions for persistence.
    ///
    /// This is a crate-internal method used by the index persistence layer.
    pub(crate) fn get_node_versions(&self) -> &HashMap<VersionId, NodeVersion> {
        &self.node_versions
    }

    /// Get all edge versions for persistence.
    ///
    /// This is a crate-internal method used by the index persistence layer.
    pub(crate) fn get_edge_versions(&self) -> &HashMap<VersionId, EdgeVersion> {
        &self.edge_versions
    }

    /// Count versions since the last anchor using a generic version lookup function.
    #[cfg(test)]
    fn count_versions_since_anchor<'a, V: EntityVersion + 'a>(
        &'a self,
        version_id: VersionId,
        get_version: impl Fn(VersionId) -> Option<&'a V>,
    ) -> usize {
        let mut count = 0;
        let mut current_id = version_id;

        loop {
            if let Some(version) = get_version(current_id) {
                if version.is_anchor() {
                    return count;
                }
                count += 1;

                if let Some(prev_id) = version.prev_version() {
                    current_id = prev_id;
                } else {
                    return count;
                }
            } else {
                return count;
            }
        }
    }

    /// Count how many versions exist since the last anchor (for a node).
    #[cfg(test)]
    pub(crate) fn count_versions_since_anchor_node(&self, version_id: VersionId) -> usize {
        self.count_versions_since_anchor(version_id, |vid| self.node_versions.get(&vid))
    }
}
