//! Temporal Diff Engine.
//!
//! Computes the structural and semantic difference between two points in time.

use crate::AletheiaDB;
use crate::core::id::{EdgeId, EntityId, NodeId, VersionId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMap;
use crate::core::temporal::Timestamp;
use crate::utils::{Result, StorageError};
use std::collections::HashMap;

/// A report of changes between two timestamps.
#[derive(Debug, Clone)]
pub struct DiffReport {
    pub t1: Timestamp,
    pub t2: Timestamp,
    pub changes: Vec<EntityChange>,
}

/// A change to a single entity.
#[derive(Debug, Clone)]
pub enum EntityChange {
    Node { id: NodeId, change: ChangeType },
    Edge { id: EdgeId, change: ChangeType },
}

/// The type of change.
#[derive(Debug, Clone)]
pub enum ChangeType {
    /// Entity exists at T2 but not at T1.
    Added,
    /// Entity exists at T1 but not at T2.
    Removed,
    /// Entity exists at both but properties differ.
    Modified { diff: PropertyDiff },
}

/// Difference in properties.
#[derive(Debug, Clone)]
pub struct PropertyDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    // (Old, New)
    pub changed: HashMap<String, (String, String)>,
}

/// The Temporal Diff Engine.
pub struct TemporalDiff<'a> {
    db: &'a AletheiaDB,
}

impl<'a> TemporalDiff<'a> {
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Compute the difference between the graph state at `t1` and `t2`.
    ///
    /// This compares the set of valid entities and their properties.
    /// It uses `valid_time` = T and `transaction_time` = T for both points,
    /// effectively comparing "What we knew at T1 about T1" vs "What we knew at T2 about T2".
    /// This is the standard "Snapshot Diff".
    pub fn compute_diff(&self, t1: Timestamp, t2: Timestamp) -> Result<DiffReport> {
        let entities = self.db.temporal_indexes.get_all_entity_ids();
        let mut changes = Vec::new();

        for entity_id in entities {
            // Find version visible at t1
            let v1 = self.find_version(entity_id, t1);
            // Find version visible at t2
            let v2 = self.find_version(entity_id, t2);

            let change = match (v1, v2) {
                (None, None) => continue, // Not visible at either time
                (None, Some(_)) => Some(ChangeType::Added),
                (Some(_), None) => Some(ChangeType::Removed),
                (Some(ver1), Some(ver2)) => {
                    if ver1 == ver2 {
                        // Same version ID means no change in properties
                        None
                    } else {
                        // Different versions, check properties
                        let props1 = self.get_properties(entity_id, ver1)?;
                        let props2 = self.get_properties(entity_id, ver2)?;

                        let diff = self.diff_properties(&props1, &props2);
                        if diff.is_empty() {
                            None
                        } else {
                            Some(ChangeType::Modified { diff })
                        }
                    }
                }
            };

            if let Some(change_type) = change {
                match entity_id {
                    EntityId::Node(id) => changes.push(EntityChange::Node {
                        id,
                        change: change_type,
                    }),
                    EntityId::Edge(id) => changes.push(EntityChange::Edge {
                        id,
                        change: change_type,
                    }),
                }
            }
        }

        Ok(DiffReport { t1, t2, changes })
    }

    fn find_version(&self, entity_id: EntityId, time: Timestamp) -> Option<VersionId> {
        match entity_id {
            EntityId::Node(id) => {
                let versions = self
                    .db
                    .temporal_indexes
                    .find_node_version_at_point(id, time, time);
                versions.first().copied()
            }
            EntityId::Edge(id) => {
                let versions = self
                    .db
                    .temporal_indexes
                    .find_edge_version_at_point(id, time, time);
                versions.first().copied()
            }
        }
    }

    fn get_properties(&self, entity_id: EntityId, version_id: VersionId) -> Result<PropertyMap> {
        let historical = self.db.historical.read();
        match entity_id {
            EntityId::Node(_) => {
                // Ensure version exists first
                historical
                    .get_node_version(version_id)
                    .ok_or_else(|| StorageError::VersionNotFound(version_id))?;

                // Reconstruct properties (handles deltas)
                historical.reconstruct_node_properties(version_id)
            }
            EntityId::Edge(_) => {
                historical
                    .get_edge_version(version_id)
                    .ok_or_else(|| StorageError::VersionNotFound(version_id))?;

                historical.reconstruct_edge_properties(version_id)
            }
        }
    }

    fn diff_properties(&self, p1: &PropertyMap, p2: &PropertyMap) -> PropertyDiff {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut changed = HashMap::new();

        // Check for removed or changed
        for (key, val1) in p1.iter() {
            match p2.get_by_interned_key(key) {
                None => {
                    if let Some(s) = GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string()) {
                        removed.push(s);
                    }
                }
                Some(val2) => {
                    // Simple comparison using PartialEq
                    if val1 != val2 {
                        if let Some(s) = GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string()) {
                            changed.insert(s, (format!("{:?}", val1), format!("{:?}", val2)));
                        }
                    }
                }
            }
        }

        // Check for added
        for (key, _) in p2.iter() {
            if !p1.contains_interned_key(key) {
                if let Some(s) = GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string()) {
                    added.push(s);
                }
            }
        }

        PropertyDiff {
            added,
            removed,
            changed,
        }
    }
}

impl PropertyDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_temporal_diff_lifecycle() {
        let db = AletheiaDB::new().unwrap();

        let t0 = time::now();
        // Ensure t1 > t0
        std::thread::sleep(std::time::Duration::from_millis(10));

        // T1: Create Node
        let props1 = PropertyMapBuilder::new()
            .insert("name", "Nova")
            .insert("level", 1)
            .build();
        let node_id = db.create_node("Agent", props1).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let t1 = time::now();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // T2: Update Node
        let props2 = PropertyMapBuilder::new()
            .insert("level", 2)
            .insert("status", "Active")
            .build();
        // Update usually merges/replaces properties.
        db.write(|tx| tx.update_node(node_id, props2)).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let t2 = time::now();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // T3: Delete Node
        db.write(|tx| tx.delete_node(node_id)).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let t3 = time::now();

        let diff_engine = TemporalDiff::new(&db);

        // Diff T0 -> T1 (Added)
        let report_0_1 = diff_engine.compute_diff(t0, t1).unwrap();
        assert_eq!(report_0_1.changes.len(), 1);
        if let EntityChange::Node { id, change } = &report_0_1.changes[0] {
            assert_eq!(*id, node_id);
            assert!(matches!(change, ChangeType::Added));
        } else {
            panic!("Expected Node change");
        }

        // Diff T1 -> T2 (Modified)
        let report_1_2 = diff_engine.compute_diff(t1, t2).unwrap();
        assert_eq!(report_1_2.changes.len(), 1);
        if let EntityChange::Node { id, change } = &report_1_2.changes[0] {
            assert_eq!(*id, node_id);
            match change {
                ChangeType::Modified { diff } => {
                    // "level" changed 1 -> 2.
                    assert!(diff.changed.contains_key("level"));
                    // "status" added.
                    assert!(diff.added.contains(&"status".to_string()));
                }
                _ => panic!("Expected Modified, got {:?}", change),
            }
        } else {
            panic!("Expected Node change");
        }

        // Diff T2 -> T3 (Removed)
        let report_2_3 = diff_engine.compute_diff(t2, t3).unwrap();
        assert_eq!(report_2_3.changes.len(), 1);
        if let EntityChange::Node { id, change } = &report_2_3.changes[0] {
            assert_eq!(*id, node_id);
            assert!(matches!(change, ChangeType::Removed));
        } else {
            panic!("Expected Node change");
        }
    }
}
