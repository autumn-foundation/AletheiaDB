use super::HistoricalStorage;
use crate::core::error::{Result, StorageError};
use crate::core::graph::{Edge, Node};
use crate::core::history::{EntityHistory, VersionDiff, VersionInfo};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::temporal::Timestamp;

#[cfg(feature = "observability")]
use tracing;

impl HistoricalStorage {
    /// Get a node as it existed at a specific point in bi-temporal space.
    ///
    /// Uses the temporal index for O(log n) candidate lookup, then verifies
    /// visibility (handles closed intervals from deletions).
    pub fn get_node_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_node_at_time").entered();

        let version_id = self
            .find_node_version_at_time(node_id, valid_time, transaction_time)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        // Note: find_node_version_at_time already checked visibility
        let version = self
            .get_node_version(version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        let properties = self.reconstruct_node_properties(version_id)?;

        Ok(Node::new(
            version.node_id,
            version.label,
            properties,
            version.id,
        ))
    }

    /// Get an edge as it existed at a specific point in bi-temporal space.
    ///
    /// Uses the temporal index for O(log n) candidate lookup, then verifies
    /// visibility (handles closed intervals from deletions).
    pub fn get_edge_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_edge_at_time").entered();

        let version_id = self
            .find_edge_version_at_time(edge_id, valid_time, transaction_time)
            .ok_or(StorageError::EdgeNotFound(edge_id))?;

        // Note: find_edge_version_at_time already checked visibility
        let version = self
            .get_edge_version(version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        let properties = self.reconstruct_edge_properties(version_id)?;

        Ok(Edge::new(
            version.edge_id,
            version.label,
            version.source,
            version.target,
            properties,
            version.id,
        ))
    }

    /// Get multiple nodes as they existed at a specific point in bi-temporal space.
    ///
    /// This retrieves nodes in batch to minimize overhead.
    /// If a node is not found or not visible at the time, the Option will be None.
    pub fn get_nodes_at_time(
        &self,
        node_ids: &[NodeId],
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Vec<(NodeId, Option<Node>)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_nodes_at_time").entered();

        let mut results = Vec::with_capacity(node_ids.len());

        for &node_id in node_ids {
            let node = if let Some(version_id) =
                self.find_node_version_at_time(node_id, valid_time, transaction_time)
            {
                // We found a visible version. Reconstruct it.
                match self.reconstruct_node_properties(version_id) {
                    Ok(properties) => {
                        let version = self
                            .node_versions
                            .get(&version_id)
                            .ok_or(StorageError::VersionNotFound(version_id))?;
                        Some(Node::new(
                            version.node_id,
                            version.label,
                            properties,
                            version.id,
                        ))
                    }
                    Err(_e) => {
                        #[cfg(feature = "observability")]
                        tracing::error!(
                            version_id = %version_id,
                            node_id = %node_id,
                            error = %_e,
                            "Property reconstruction failed in batch query"
                        );
                        None
                    }
                }
            } else {
                None
            };
            results.push((node_id, node));
        }

        Ok(results)
    }

    /// Get multiple edges as they existed at a specific point in bi-temporal space.
    ///
    /// This retrieves edges in batch to minimize overhead.
    /// If an edge is not found or not visible at the time, the Option will be None.
    pub fn get_edges_at_time(
        &self,
        edge_ids: &[EdgeId],
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Vec<(EdgeId, Option<Edge>)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_edges_at_time").entered();

        let mut results = Vec::with_capacity(edge_ids.len());

        for &edge_id in edge_ids {
            let edge = if let Some(version_id) =
                self.find_edge_version_at_time(edge_id, valid_time, transaction_time)
            {
                // We found a visible version. Reconstruct it.
                match self.reconstruct_edge_properties(version_id) {
                    Ok(properties) => {
                        let version = self
                            .edge_versions
                            .get(&version_id)
                            .ok_or(StorageError::VersionNotFound(version_id))?;
                        Some(Edge::new(
                            version.edge_id,
                            version.label,
                            version.source,
                            version.target,
                            properties,
                            version.id,
                        ))
                    }
                    Err(_e) => {
                        #[cfg(feature = "observability")]
                        tracing::error!(
                            version_id = %version_id,
                            edge_id = %edge_id,
                            error = %_e,
                            "Property reconstruction failed in batch query"
                        );
                        None
                    }
                }
            } else {
                None
            };
            results.push((edge_id, edge));
        }

        Ok(results)
    }

    /// Get the complete version history of a node.
    ///
    /// Returns all versions in chronological order (oldest first).
    pub fn get_node_history(&self, node_id: NodeId) -> Result<EntityHistory> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_node_history").entered();

        // Get the current version ID
        let current_version_id = self
            .get_current_node_version(node_id)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        // Traverse the version chain backwards to get all versions in order
        let mut version_ids = Vec::new();
        let mut current_id = Some(current_version_id);

        while let Some(vid) = current_id {
            version_ids.push(vid);
            current_id = self.get_node_version(vid).and_then(|v| v.prev_version);
        }

        // Reverse to get oldest-first order
        version_ids.reverse();

        // Build VersionInfo for each version
        let mut versions = Vec::with_capacity(version_ids.len());
        for (version_number, version_id) in version_ids.iter().enumerate() {
            if let Some(version) = self.get_node_version(*version_id) {
                let properties = self.reconstruct_node_properties(*version_id)?;

                versions.push(VersionInfo {
                    version_number: (version_number + 1) as u64, // 1-indexed
                    version_id: *version_id,
                    temporal: version.temporal,
                    properties,
                    label: GLOBAL_INTERNER
                        .resolve_with(version.label, |s| s.to_string())
                        .unwrap_or_else(|| version.label.to_string()),
                });
            }
        }

        Ok(EntityHistory { versions })
    }

    /// Get a node at a specific logical version number.
    ///
    /// Version numbers are 1-indexed (1 = first version, 2 = second version, etc.).
    pub fn get_node_at_version(&self, node_id: NodeId, version_number: u64) -> Result<Node> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_node_at_version").entered();

        // Get the current version ID
        let current_version_id = self
            .get_current_node_version(node_id)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        // Traverse the version chain backwards to collect all versions
        let mut version_ids = Vec::new();
        let mut current_id = Some(current_version_id);

        while let Some(vid) = current_id {
            version_ids.push(vid);
            current_id = self.get_node_version(vid).and_then(|v| v.prev_version);
        }

        // Reverse to get oldest-first order
        version_ids.reverse();

        // Convert 1-indexed version number to 0-indexed array index
        let index = version_number
            .checked_sub(1)
            .ok_or(StorageError::NodeNotFound(node_id))? as usize;

        // Get the version ID at that index
        let version_id = version_ids
            .get(index)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        // Reconstruct the node from that version
        let version = self
            .get_node_version(*version_id)
            .ok_or(StorageError::VersionNotFound(*version_id))?;

        let properties = self.reconstruct_node_properties(*version_id)?;

        Ok(Node::new(
            version.node_id,
            version.label,
            properties,
            version.id,
        ))
    }

    /// Compute the difference between two versions of a node.
    ///
    /// Shows which properties were added, removed, or modified.
    pub fn diff_node_versions(
        &self,
        node_id: NodeId,
        from_version: VersionId,
        to_version: VersionId,
    ) -> Result<VersionDiff> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("diff_node_versions").entered();

        // Validate that both versions belong to the requested node
        let from_ver = self
            .get_node_version(from_version)
            .ok_or(StorageError::VersionNotFound(from_version))?;
        let to_ver = self
            .get_node_version(to_version)
            .ok_or(StorageError::VersionNotFound(to_version))?;

        if from_ver.node_id != node_id {
            return Err(StorageError::InconsistentState {
                reason: format!(
                    "Version {} belongs to node {}, not node {}",
                    from_version, from_ver.node_id, node_id
                ),
            }
            .into());
        }
        if to_ver.node_id != node_id {
            return Err(StorageError::InconsistentState {
                reason: format!(
                    "Version {} belongs to node {}, not node {}",
                    to_version, to_ver.node_id, node_id
                ),
            }
            .into());
        }

        // Reconstruct both versions
        let from_props = self.reconstruct_node_properties(from_version)?;
        let to_props = self.reconstruct_node_properties(to_version)?;

        // Compute diff
        Ok(VersionDiff::compute(
            &from_props,
            &to_props,
            from_version,
            to_version,
        ))
    }

    /// Get the complete version history of an edge.
    ///
    /// Returns all versions in chronological order (oldest first).
    pub fn get_edge_history(&self, edge_id: EdgeId) -> Result<EntityHistory> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_edge_history").entered();

        // Get the current version ID
        let current_version_id = self
            .get_current_edge_version(edge_id)
            .ok_or(StorageError::EdgeNotFound(edge_id))?;

        // Traverse the version chain backwards to get all versions
        let mut version_ids = Vec::new();
        let mut current_id = Some(current_version_id);

        while let Some(vid) = current_id {
            version_ids.push(vid);
            current_id = self.get_edge_version(vid).and_then(|v| v.prev_version);
        }

        // Reverse to get oldest-first order
        version_ids.reverse();

        // Build VersionInfo for each version
        let mut versions = Vec::with_capacity(version_ids.len());
        for (version_number, version_id) in version_ids.iter().enumerate() {
            if let Some(version) = self.get_edge_version(*version_id) {
                let properties = self.reconstruct_edge_properties(*version_id)?;

                versions.push(VersionInfo {
                    version_number: (version_number + 1) as u64, // 1-indexed
                    version_id: *version_id,
                    temporal: version.temporal,
                    properties,
                    label: GLOBAL_INTERNER
                        .resolve_with(version.label, |s| s.to_string())
                        .unwrap_or_else(|| version.label.to_string()),
                });
            }
        }

        Ok(EntityHistory { versions })
    }

    /// Compute the difference between two versions of an edge.
    ///
    /// Shows which properties were added, removed, or modified.
    pub fn diff_edge_versions(
        &self,
        edge_id: EdgeId,
        from_version: VersionId,
        to_version: VersionId,
    ) -> Result<VersionDiff> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("diff_edge_versions").entered();

        // Validate that both versions belong to the requested edge
        let from_ver = self
            .get_edge_version(from_version)
            .ok_or(StorageError::VersionNotFound(from_version))?;
        let to_ver = self
            .get_edge_version(to_version)
            .ok_or(StorageError::VersionNotFound(to_version))?;

        if from_ver.edge_id != edge_id {
            return Err(StorageError::InconsistentState {
                reason: format!(
                    "Version {} belongs to edge {}, not edge {}",
                    from_version, from_ver.edge_id, edge_id
                ),
            }
            .into());
        }
        if to_ver.edge_id != edge_id {
            return Err(StorageError::InconsistentState {
                reason: format!(
                    "Version {} belongs to edge {}, not edge {}",
                    to_version, to_ver.edge_id, edge_id
                ),
            }
            .into());
        }

        // Reconstruct both versions
        let from_props = self.reconstruct_edge_properties(from_version)?;
        let to_props = self.reconstruct_edge_properties(to_version)?;

        // Compute diff
        Ok(VersionDiff::compute(
            &from_props,
            &to_props,
            from_version,
            to_version,
        ))
    }

    /// Get outgoing edges from a node at a specific point in time.
    ///
    /// This method uses the temporal adjacency index to efficiently find all
    /// edges that were valid at the specified time, including edges that have
    /// been deleted in current storage.
    ///
    /// # Arguments
    ///
    /// * `source` - The source node ID
    /// * `valid_time` - The valid time to query
    /// * `tx_time` - The transaction time to query
    ///
    /// # Returns
    ///
    /// A vector of edge IDs that were valid at the specified time. Returns an
    /// empty vector if no temporal adjacency index is configured.
    pub fn get_outgoing_edges_at_time(
        &self,
        source: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        if let Some(ref index) = self.temporal_adjacency_index {
            index.get_outgoing_at_time(source, valid_time, tx_time)
        } else {
            Vec::new()
        }
    }

    /// Get incoming edges to a node at a specific point in time.
    ///
    /// This method uses the temporal adjacency index to efficiently find all
    /// edges that were valid at the specified time, including edges that have
    /// been deleted in current storage.
    ///
    /// # Arguments
    ///
    /// * `target` - The target node ID
    /// * `valid_time` - The valid time to query
    /// * `tx_time` - The transaction time to query
    ///
    /// # Returns
    ///
    /// A vector of edge IDs that were valid at the specified time. Returns an
    /// empty vector if no temporal adjacency index is configured.
    pub fn get_incoming_edges_at_time(
        &self,
        target: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        if let Some(ref index) = self.temporal_adjacency_index {
            index.get_incoming_at_time(target, valid_time, tx_time)
        } else {
            Vec::new()
        }
    }
}
