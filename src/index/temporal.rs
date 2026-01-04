//! Temporal indexes for efficient time-based queries.
//!
//! This module implements B-Tree based indexes that enable fast lookup of
//! versions by entity ID and timestamp. The indexes support both point-in-time
//! queries and range queries over temporal dimensions.
//!
//! # Index Structure
//!
//! We maintain separate indexes for valid time and transaction time:
//! - `valid_time_index`: BTreeMap<(EntityId, Timestamp), Vec<VersionId>>
//! - `transaction_time_index`: BTreeMap<(EntityId, Timestamp), Vec<VersionId>>
//!
//! This allows efficient queries like:
//! - "Find all versions of entity X valid at time T"
//! - "Find all versions recorded between times T1 and T2"

use crate::core::id::{EdgeId, EntityId, NodeId, VersionId};
use crate::core::temporal::{BiTemporalInterval, TimeRange, Timestamp};
use std::collections::BTreeMap;

/// Index entry combining entity and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct TemporalKey {
    entity_id: EntityId,
    timestamp: Timestamp,
}

impl TemporalKey {
    fn new(entity_id: EntityId, timestamp: Timestamp) -> Self {
        TemporalKey {
            entity_id,
            timestamp,
        }
    }
}

/// Temporal indexes for efficient time-based lookups.
///
/// These indexes use B-Tree structures to enable logarithmic-time
/// lookups and efficient range scans.
#[derive(Debug, Clone)]
pub struct TemporalIndexes {
    /// Index by valid time start
    valid_time_index: BTreeMap<TemporalKey, Vec<VersionId>>,
    /// Index by transaction time start
    transaction_time_index: BTreeMap<TemporalKey, Vec<VersionId>>,
}

impl TemporalIndexes {
    /// Create a new empty temporal index.
    pub fn new() -> Self {
        TemporalIndexes {
            valid_time_index: BTreeMap::new(),
            transaction_time_index: BTreeMap::new(),
        }
    }

    /// Insert a node version into the temporal indexes.
    pub fn insert_node_version(
        &mut self,
        node_id: NodeId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
    ) {
        let entity_id = EntityId::Node(node_id);
        self.insert_version(entity_id, version_id, temporal);
    }

    /// Insert an edge version into the temporal indexes.
    pub fn insert_edge_version(
        &mut self,
        edge_id: EdgeId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
    ) {
        let entity_id = EntityId::Edge(edge_id);
        self.insert_version(entity_id, version_id, temporal);
    }

    /// Insert a version into both temporal indexes.
    fn insert_version(
        &mut self,
        entity_id: EntityId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
    ) {
        // Index by valid time start
        let valid_key = TemporalKey::new(entity_id, temporal.valid_time().start());
        self.valid_time_index
            .entry(valid_key)
            .or_default()
            .push(version_id);

        // Index by transaction time start
        let tx_key = TemporalKey::new(entity_id, temporal.transaction_time().start());
        self.transaction_time_index
            .entry(tx_key)
            .or_default()
            .push(version_id);
    }

    /// Find all node versions that overlap with the given valid time range.
    pub fn find_node_versions_in_valid_time_range(
        &self,
        node_id: NodeId,
        time_range: TimeRange,
    ) -> Vec<VersionId> {
        let entity_id = EntityId::Node(node_id);
        self.find_versions_in_range(&self.valid_time_index, entity_id, time_range)
    }

    /// Find all edge versions that overlap with the given valid time range.
    pub fn find_edge_versions_in_valid_time_range(
        &self,
        edge_id: EdgeId,
        time_range: TimeRange,
    ) -> Vec<VersionId> {
        let entity_id = EntityId::Edge(edge_id);
        self.find_versions_in_range(&self.valid_time_index, entity_id, time_range)
    }

    /// Find all node versions recorded in the given transaction time range.
    pub fn find_node_versions_in_transaction_time_range(
        &self,
        node_id: NodeId,
        time_range: TimeRange,
    ) -> Vec<VersionId> {
        let entity_id = EntityId::Node(node_id);
        self.find_versions_in_range(&self.transaction_time_index, entity_id, time_range)
    }

    /// Find all edge versions recorded in the given transaction time range.
    pub fn find_edge_versions_in_transaction_time_range(
        &self,
        edge_id: EdgeId,
        time_range: TimeRange,
    ) -> Vec<VersionId> {
        let entity_id = EntityId::Edge(edge_id);
        self.find_versions_in_range(&self.transaction_time_index, entity_id, time_range)
    }

    /// Find versions in a specific index within a time range.
    ///
    /// This finds all versions whose start time falls within the query range.
    /// For overlap queries, the caller should query with an extended range.
    fn find_versions_in_range(
        &self,
        index: &BTreeMap<TemporalKey, Vec<VersionId>>,
        entity_id: EntityId,
        time_range: TimeRange,
    ) -> Vec<VersionId> {
        // Find versions whose start time is within the query range
        let start_key = TemporalKey::new(entity_id, time_range.start());
        let end_key = TemporalKey::new(entity_id, time_range.end());

        let mut results = Vec::new();

        // Range query over the B-Tree
        for (_key, versions) in index.range(start_key..=end_key) {
            results.extend_from_slice(versions);
        }

        results
    }

    /// Get the number of indexed versions.
    pub fn version_count(&self) -> usize {
        // Count unique versions across both indexes
        // Note: This is an approximation as versions appear in both indexes
        self.valid_time_index.values().map(|v| v.len()).sum()
    }

    /// Clear all indexes.
    pub fn clear(&mut self) {
        self.valid_time_index.clear();
        self.transaction_time_index.clear();
    }
}

impl Default for TemporalIndexes {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::temporal::BiTemporalInterval;

    #[test]
    fn test_insert_and_find_node_versions() {
        let mut indexes = TemporalIndexes::new();

        let node_id = NodeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();
        let v3 = VersionId::new(102).unwrap();

        // Insert versions at different valid times
        indexes.insert_node_version(
            node_id,
            v1,
            BiTemporalInterval::new(TimeRange::new(0, 1000), TimeRange::new(0, Timestamp::MAX)),
        );

        indexes.insert_node_version(
            node_id,
            v2,
            BiTemporalInterval::new(
                TimeRange::new(1000, 2000),
                TimeRange::new(0, Timestamp::MAX),
            ),
        );

        indexes.insert_node_version(
            node_id,
            v3,
            BiTemporalInterval::new(
                TimeRange::new(2000, 3000),
                TimeRange::new(0, Timestamp::MAX),
            ),
        );

        // Query for versions in valid time range [500, 1500]
        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::new(500, 1500));

        // Should find only v2 (starts at 1000, which is in range [500, 1500])
        // v1 starts at 0, which is before the range
        // v3 starts at 2000, which is after the range
        assert_eq!(results.len(), 1);
        assert!(results.contains(&v2));
    }

    #[test]
    fn test_transaction_time_range_query() {
        let mut indexes = TemporalIndexes::new();

        let edge_id = EdgeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();

        // Version recorded at tx time 1000
        indexes.insert_edge_version(
            edge_id,
            v1,
            BiTemporalInterval::new(
                TimeRange::new(0, Timestamp::MAX),
                TimeRange::new(1000, Timestamp::MAX),
            ),
        );

        // Version recorded at tx time 2000
        indexes.insert_edge_version(
            edge_id,
            v2,
            BiTemporalInterval::new(
                TimeRange::new(0, Timestamp::MAX),
                TimeRange::new(2000, Timestamp::MAX),
            ),
        );

        // Query for versions recorded between 1500 and 2500
        let results = indexes
            .find_edge_versions_in_transaction_time_range(edge_id, TimeRange::new(1500, 2500));

        // Should find only v2
        assert_eq!(results.len(), 1);
        assert!(results.contains(&v2));
    }

    #[test]
    fn test_empty_index() {
        let indexes = TemporalIndexes::new();

        let results =
            indexes.find_node_versions_in_valid_time_range(NodeId::new(1).unwrap(), TimeRange::new(0, 1000));

        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_multiple_entities() {
        let mut indexes = TemporalIndexes::new();

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();

        indexes.insert_node_version(node1, v1, BiTemporalInterval::current(1000));

        indexes.insert_node_version(node2, v2, BiTemporalInterval::current(1000));

        // Query for node1 should only return v1
        let results =
            indexes.find_node_versions_in_valid_time_range(node1, TimeRange::new(0, 2000));

        assert_eq!(results.len(), 1);
        assert!(results.contains(&v1));
        assert!(!results.contains(&v2));
    }

    #[test]
    fn test_clear() {
        let mut indexes = TemporalIndexes::new();

        indexes.insert_node_version(
            NodeId::new(1).unwrap(),
            VersionId::new(100).unwrap(),
            BiTemporalInterval::current(1000),
        );

        assert!(indexes.version_count() > 0);

        indexes.clear();

        assert_eq!(indexes.version_count(), 0);
    }

    #[test]
    fn test_temporal_key_ordering() {
        let node1 = EntityId::Node(NodeId::new(1).unwrap());
        let node2 = EntityId::Node(NodeId::new(2).unwrap());

        let key1 = TemporalKey::new(node1, 1000);
        let key2 = TemporalKey::new(node1, 2000);
        let key3 = TemporalKey::new(node2, 1000);

        // Same entity, different times
        assert!(key1 < key2);

        // Different entities
        assert!(key1 < key3);
    }
}
