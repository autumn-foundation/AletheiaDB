//! Historical storage engine with anchor+delta compression.
//!
//! This module implements temporal versioning using version chains. Each node
//! and edge can have multiple versions over time, linked together in a chain
//! ordered by transaction time.
//!
//! The anchor+delta strategy minimizes storage overhead:
//! - Anchors are created every N versions (configurable)
//! - Deltas store only changed properties
//! - Reconstruction walks backward to nearest anchor and applies deltas forward

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::PropertyMap;
use crate::core::temporal::{BiTemporalInterval, Timestamp};
use crate::storage::version::{AnchorConfig, EdgeVersion, NodeVersion, VersionData};
use crate::utils::error::{Result, StorageError, TemporalError};
use std::collections::HashMap;

/// Historical storage for versioned nodes and edges.
///
/// This storage engine maintains version chains for all temporal data,
/// using anchor+delta compression to minimize storage overhead.
pub struct HistoricalStorage {
    /// Configuration for anchor creation strategy
    config: AnchorConfig,
    /// All node versions, indexed by version ID
    node_versions: HashMap<VersionId, NodeVersion>,
    /// All edge versions, indexed by version ID
    edge_versions: HashMap<VersionId, EdgeVersion>,
    /// Current version pointer for each node
    node_current_version: HashMap<NodeId, VersionId>,
    /// Current version pointer for each edge
    edge_current_version: HashMap<EdgeId, VersionId>,
    /// Head version ID for each node (most recent)
    node_version_heads: HashMap<NodeId, VersionId>,
    /// Head version ID for each edge (most recent)
    edge_version_heads: HashMap<EdgeId, VersionId>,
}

impl HistoricalStorage {
    /// Create a new historical storage with default configuration.
    pub fn new() -> Self {
        Self::with_config(AnchorConfig::default())
    }

    /// Create a new historical storage with custom configuration.
    pub fn with_config(config: AnchorConfig) -> Self {
        HistoricalStorage {
            config,
            node_versions: HashMap::new(),
            edge_versions: HashMap::new(),
            node_current_version: HashMap::new(),
            edge_current_version: HashMap::new(),
            node_version_heads: HashMap::new(),
            edge_version_heads: HashMap::new(),
        }
    }

    /// Add a new version of a node.
    ///
    /// This will automatically determine whether to create an anchor or delta
    /// based on the version chain length.
    pub fn add_node_version(
        &mut self,
        node_id: NodeId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
        label: InternedString,
        properties: PropertyMap,
    ) -> Result<()> {
        // Check if this node already has versions
        let prev_version_id = self.node_version_heads.get(&node_id).copied();

        let version = if let Some(prev_id) = prev_version_id {
            // Get the previous version (verify it exists)
            let _prev_version = self
                .node_versions
                .get(&prev_id)
                .ok_or(StorageError::VersionNotFound(prev_id))?;

            // Count versions since last anchor (including this new version)
            let versions_since_anchor = self.count_versions_since_anchor_node(prev_id) + 1;

            // Decide whether to create anchor or delta
            if versions_since_anchor >= self.config.anchor_interval as usize {
                // Create anchor
                NodeVersion::new_anchor(version_id, node_id, temporal, label, properties)
            } else {
                // Create delta from previous version
                let old_properties = self.reconstruct_node_properties(prev_id)?;
                NodeVersion::new_delta(
                    version_id,
                    node_id,
                    temporal,
                    label,
                    &old_properties,
                    &properties,
                    prev_id,
                )
            }
        } else {
            // First version is always an anchor
            NodeVersion::new_anchor(version_id, node_id, temporal, label, properties)
        };

        // Link the previous version to this one
        if let Some(prev_id) = prev_version_id {
            if let Some(prev) = self.node_versions.get_mut(&prev_id) {
                prev.next_version = Some(version_id);
            }
        }

        // Store the version
        self.node_versions.insert(version_id, version);
        self.node_version_heads.insert(node_id, version_id);

        Ok(())
    }

    /// Add a new version of an edge.
    pub fn add_edge_version(
        &mut self,
        edge_id: EdgeId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
        label: InternedString,
        source: NodeId,
        target: NodeId,
        properties: PropertyMap,
    ) -> Result<()> {
        let prev_version_id = self.edge_version_heads.get(&edge_id).copied();

        let version = if let Some(prev_id) = prev_version_id {
            let _prev_version = self
                .edge_versions
                .get(&prev_id)
                .ok_or(StorageError::VersionNotFound(prev_id))?;

            let versions_since_anchor = self.count_versions_since_anchor_edge(prev_id) + 1;

            if versions_since_anchor >= self.config.anchor_interval as usize {
                EdgeVersion::new_anchor(
                    version_id, edge_id, temporal, label, source, target, properties,
                )
            } else {
                let old_properties = self.reconstruct_edge_properties(prev_id)?;
                EdgeVersion::new_delta(
                    version_id,
                    edge_id,
                    temporal,
                    label,
                    source,
                    target,
                    &old_properties,
                    &properties,
                    prev_id,
                )
            }
        } else {
            EdgeVersion::new_anchor(
                version_id, edge_id, temporal, label, source, target, properties,
            )
        };

        if let Some(prev_id) = prev_version_id {
            if let Some(prev) = self.edge_versions.get_mut(&prev_id) {
                prev.next_version = Some(version_id);
            }
        }

        self.edge_versions.insert(version_id, version);
        self.edge_version_heads.insert(edge_id, version_id);

        Ok(())
    }

    /// Reconstruct the properties of a node version.
    ///
    /// This walks backward to find the nearest anchor, then applies all deltas
    /// forward to reconstruct the full property state.
    pub fn reconstruct_node_properties(&self, version_id: VersionId) -> Result<PropertyMap> {
        let version = self
            .node_versions
            .get(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        match &version.data {
            VersionData::Anchor { properties } => Ok(properties.clone()),
            VersionData::Delta { delta } => {
                // Find the previous version
                let prev_id = version
                    .prev_version
                    .ok_or(TemporalError::CorruptedVersionChain {
                        entity_id: format!("{:?}", version.node_id),
                        reason: "Delta version has no previous version".to_string(),
                    })?;

                // Recursively reconstruct previous version
                let base_properties = self.reconstruct_node_properties(prev_id)?;

                // Apply this delta
                Ok(delta.apply(&base_properties))
            }
        }
    }

    /// Reconstruct the properties of an edge version.
    pub fn reconstruct_edge_properties(&self, version_id: VersionId) -> Result<PropertyMap> {
        let version = self
            .edge_versions
            .get(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        match &version.data {
            VersionData::Anchor { properties } => Ok(properties.clone()),
            VersionData::Delta { delta } => {
                let prev_id = version
                    .prev_version
                    .ok_or(TemporalError::CorruptedVersionChain {
                        entity_id: format!("{:?}", version.edge_id),
                        reason: "Delta version has no previous version".to_string(),
                    })?;

                let base_properties = self.reconstruct_edge_properties(prev_id)?;
                Ok(delta.apply(&base_properties))
            }
        }
    }

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

    /// Find a node version valid at a specific point in time.
    ///
    /// This searches the version chain for a version whose temporal interval
    /// contains the given timestamp.
    pub fn find_node_version_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Option<VersionId> {
        // Start from the head version and walk backward
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
    pub fn find_edge_version_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Option<VersionId> {
        let mut current_id = self.edge_version_heads.get(&edge_id).copied()?;

        loop {
            let version = self.edge_versions.get(&current_id)?;

            if version.temporal.is_visible_at(valid_time, transaction_time) {
                return Some(current_id);
            }

            current_id = version.prev_version?;
        }
    }

    /// Count how many versions exist since the last anchor (for a node).
    fn count_versions_since_anchor_node(&self, version_id: VersionId) -> usize {
        let mut count = 0;
        let mut current_id = version_id;

        loop {
            if let Some(version) = self.node_versions.get(&current_id) {
                if version.is_anchor() {
                    return count;
                }
                count += 1;

                // Move to previous version
                if let Some(prev_id) = version.prev_version {
                    current_id = prev_id;
                } else {
                    // Reached the beginning without finding an anchor
                    return count;
                }
            } else {
                return count;
            }
        }
    }

    /// Count how many versions exist since the last anchor (for an edge).
    fn count_versions_since_anchor_edge(&self, version_id: VersionId) -> usize {
        let mut count = 0;
        let mut current_id = version_id;

        loop {
            if let Some(version) = self.edge_versions.get(&current_id) {
                if version.is_anchor() {
                    return count;
                }
                count += 1;

                if let Some(prev_id) = version.prev_version {
                    current_id = prev_id;
                } else {
                    return count;
                }
            } else {
                return count;
            }
        }
    }

    /// Get statistics about the storage.
    pub fn stats(&self) -> HistoricalStats {
        let mut node_anchor_count = 0;
        let mut node_delta_count = 0;
        let mut edge_anchor_count = 0;
        let mut edge_delta_count = 0;

        for version in self.node_versions.values() {
            if version.is_anchor() {
                node_anchor_count += 1;
            } else {
                node_delta_count += 1;
            }
        }

        for version in self.edge_versions.values() {
            if version.is_anchor() {
                edge_anchor_count += 1;
            } else {
                edge_delta_count += 1;
            }
        }

        HistoricalStats {
            total_node_versions: self.node_versions.len(),
            total_edge_versions: self.edge_versions.len(),
            node_anchor_count,
            node_delta_count,
            edge_anchor_count,
            edge_delta_count,
            unique_nodes: self.node_version_heads.len(),
            unique_edges: self.edge_version_heads.len(),
        }
    }
}

impl Default for HistoricalStorage {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about the historical storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalStats {
    /// Total number of node versions stored
    pub total_node_versions: usize,
    /// Total number of edge versions stored
    pub total_edge_versions: usize,
    /// Number of anchor node versions
    pub node_anchor_count: usize,
    /// Number of delta node versions
    pub node_delta_count: usize,
    /// Number of anchor edge versions
    pub edge_anchor_count: usize,
    /// Number of delta edge versions
    pub edge_delta_count: usize,
    /// Number of unique nodes with version history
    pub unique_nodes: usize,
    /// Number of unique edges with version history
    pub unique_edges: usize,
}

impl HistoricalStats {
    /// Calculate the compression ratio (anchors vs total versions).
    pub fn compression_ratio(&self) -> f64 {
        let total_versions = self.total_node_versions + self.total_edge_versions;
        let total_anchors = self.node_anchor_count + self.edge_anchor_count;

        if total_versions == 0 {
            return 1.0;
        }

        total_anchors as f64 / total_versions as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::TimeRange;

    #[test]
    fn test_create_first_version() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1);
        let version_id = VersionId::new(100);
        let label = GLOBAL_INTERNER.intern("Person");
        let temporal = BiTemporalInterval::current(1000);
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();

        storage
            .add_node_version(node_id, version_id, temporal, label, props)
            .unwrap();

        // First version should be an anchor
        let version = storage.get_node_version(version_id).unwrap();
        assert!(version.is_anchor());
        assert_eq!(version.node_id, node_id);
        assert_eq!(version.prev_version, None);
    }

    #[test]
    fn test_version_chain() {
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 3,
            max_delta_chain: 10,
        });

        let node_id = NodeId::new(1);
        let label = GLOBAL_INTERNER.intern("Person");

        // Create 5 versions
        let mut version_ids = Vec::new();
        for i in 0..5 {
            let version_id = VersionId::new(100 + i);
            let temporal = BiTemporalInterval::current(1000 + (i as i64) * 100);
            let props = PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", i as i64)
                .build();

            storage
                .add_node_version(node_id, version_id, temporal, label, props)
                .unwrap();

            version_ids.push(version_id);
        }

        // Check version types
        // v0: anchor (first)
        // v1: delta
        // v2: delta
        // v3: anchor (interval = 3)
        // v4: delta

        assert!(storage.get_node_version(version_ids[0]).unwrap().is_anchor());
        assert!(storage.get_node_version(version_ids[1]).unwrap().is_delta());
        assert!(storage.get_node_version(version_ids[2]).unwrap().is_delta());
        assert!(storage.get_node_version(version_ids[3]).unwrap().is_anchor());
        assert!(storage.get_node_version(version_ids[4]).unwrap().is_delta());
    }

    #[test]
    fn test_property_reconstruction() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1);
        let label = GLOBAL_INTERNER.intern("Person");

        // Version 1: name=Alice, age=30
        let v1 = VersionId::new(1);
        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::current(1000),
                label,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .build(),
            )
            .unwrap();

        // Version 2: name=Alice, age=31 (delta)
        let v2 = VersionId::new(2);
        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::current(2000),
                label,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 31i64)
                    .build(),
            )
            .unwrap();

        // Reconstruct v2 properties
        let props = storage.reconstruct_node_properties(v2).unwrap();
        assert_eq!(props.get("name").and_then(|v| v.as_str()), Some("Alice"));
        assert_eq!(props.get("age").and_then(|v| v.as_int()), Some(31));
    }

    #[test]
    fn test_find_version_at_time() {
        let mut storage = HistoricalStorage::new();

        let node_id = NodeId::new(1);
        let label = GLOBAL_INTERNER.intern("Person");

        // Create versions at different times
        let v1 = VersionId::new(1);
        let v2 = VersionId::new(2);
        let v3 = VersionId::new(3);

        storage
            .add_node_version(
                node_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0, 1000),
                    TimeRange::new(0, Timestamp::MAX),
                ),
                label,
                PropertyMapBuilder::new().insert("age", 30i64).build(),
            )
            .unwrap();

        storage
            .add_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(1000, 2000),
                    TimeRange::new(0, Timestamp::MAX),
                ),
                label,
                PropertyMapBuilder::new().insert("age", 31i64).build(),
            )
            .unwrap();

        storage
            .add_node_version(
                node_id,
                v3,
                BiTemporalInterval::new(
                    TimeRange::new(2000, Timestamp::MAX),
                    TimeRange::new(0, Timestamp::MAX),
                ),
                label,
                PropertyMapBuilder::new().insert("age", 32i64).build(),
            )
            .unwrap();

        // Query at different times
        assert_eq!(
            storage.find_node_version_at_time(node_id, 500, 100),
            Some(v1)
        );
        assert_eq!(
            storage.find_node_version_at_time(node_id, 1500, 100),
            Some(v2)
        );
        assert_eq!(
            storage.find_node_version_at_time(node_id, 2500, 100),
            Some(v3)
        );
    }

    #[test]
    fn test_stats() {
        let mut storage = HistoricalStorage::with_config(AnchorConfig {
            anchor_interval: 2,
            max_delta_chain: 10,
        });

        let label = GLOBAL_INTERNER.intern("Test");

        // Add 3 node versions (anchor, delta, anchor)
        for i in 0..3 {
            storage
                .add_node_version(
                    NodeId::new(1),
                    VersionId::new(i),
                    BiTemporalInterval::current(1000 + (i as i64) * 100),
                    label,
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
        }

        let stats = storage.stats();
        assert_eq!(stats.total_node_versions, 3);
        assert_eq!(stats.node_anchor_count, 2);
        assert_eq!(stats.node_delta_count, 1);
        assert_eq!(stats.unique_nodes, 1);

        // Compression ratio should be 2/3 ≈ 0.67
        assert!((stats.compression_ratio() - 0.6666).abs() < 0.01);
    }
}
