//! Highlander: Semantic Entity Resolution.
//!
//! "There can be only one."
//!
//! This module provides tools to identify and merge duplicate nodes in the graph.
//! It uses vector similarity to find candidates and graph transactions to merge them.
//!
//! # Use Cases
//! - **Entity Resolution**: "J. Smith" vs "John Smith".
//! - **Deduplication**: Cleaning up data ingestion errors.
//! - **Knowledge Fusion**: Merging two knowledge graphs.

use crate::AletheiaDB;
use crate::api::transaction::{ReadOps, WriteOps};
use crate::core::id::NodeId;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMapBuilder;
use crate::{Error, Result, StorageError};

/// Detector for finding potential duplicate nodes.
///
/// # Example
///
/// ```rust,no_run
/// use aletheiadb::AletheiaDB;
/// use aletheiadb::experimental::highlander::HighlanderDetector;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let db = AletheiaDB::new()?;
/// let detector = HighlanderDetector::new(&db);
///
/// // Find candidates similar to node_id
/// // let candidates = detector.find_duplicates(node_id, 0.9, 5)?;
/// # Ok(())
/// # }
/// ```
pub struct HighlanderDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> HighlanderDetector<'a> {
    /// Create a new HighlanderDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find potential duplicates for a given node.
    ///
    /// # Arguments
    /// * `target` - The node to check.
    /// * `threshold` - Minimum similarity score (0.0 to 1.0).
    /// * `limit` - Maximum number of candidates.
    pub fn find_duplicates(
        &self,
        target: NodeId,
        threshold: f32,
        limit: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.db.find_similar(target, limit).map(|candidates| {
            candidates
                .into_iter()
                .filter(|&(_, score)| score >= threshold)
                .collect()
        })
    }
}

/// Merger for combining two nodes into one.
pub struct EntityMerger<'a> {
    db: &'a AletheiaDB,
}

impl<'a> EntityMerger<'a> {
    /// Create a new EntityMerger.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Merge a victim node into a survivor node.
    ///
    /// This moves all edges from the victim to the survivor, merges properties,
    /// and deletes the victim.
    ///
    /// # Arguments
    /// * `survivor` - The node that will remain.
    /// * `victim` - The node that will be merged and deleted.
    ///
    /// # Errors
    /// Returns `Error::Other` if `survivor` and `victim` are the same node.
    pub fn merge(&self, survivor: NodeId, victim: NodeId) -> Result<()> {
        if survivor == victim {
            return Err(Error::other(
                "Cannot merge a node into itself (survivor == victim)",
            ));
        }

        self.db.write(|tx| {
            // 1. Move Edges
            // We collect all edges connected to the victim.
            // Note: Self-loops appear in both outgoing and incoming lists.
            // We handle them by deduplicating or checking ID.
            let mut edges_processed = std::collections::HashSet::new();

            // Outgoing: Victim -> Target
            let outgoing = tx.get_outgoing_edges(victim);
            for edge_id in outgoing {
                if edges_processed.insert(edge_id) {
                    let edge = tx.get_edge(edge_id)?;
                    let new_target = if edge.target == victim {
                        survivor // Handle self-loop: Victim->Victim becomes Survivor->Survivor
                    } else {
                        edge.target
                    };
                    // Resolve label
                    let label = GLOBAL_INTERNER
                        .resolve_with(edge.label, |s| s.to_string())
                        .ok_or_else(|| {
                            Error::Storage(StorageError::InconsistentState {
                                reason: format!(
                                    "Edge label with ID {} not found in interner",
                                    edge.label.as_u32()
                                ),
                            })
                        })?;

                    // Create edge from Survivor -> New Target
                    tx.create_edge(survivor, new_target, &label, edge.properties)?;
                    // Delete old edge
                    tx.delete_edge(edge_id)?;
                }
            }

            // Incoming: Source -> Victim
            let incoming = tx.get_incoming_edges(victim);
            for edge_id in incoming {
                if edges_processed.insert(edge_id) {
                    let edge = tx.get_edge(edge_id)?;
                    let new_source = if edge.source == victim {
                        survivor // Handle self-loop (should be caught above, but for safety)
                    } else {
                        edge.source
                    };
                    // Resolve label
                    let label = GLOBAL_INTERNER
                        .resolve_with(edge.label, |s| s.to_string())
                        .ok_or_else(|| {
                            Error::Storage(StorageError::InconsistentState {
                                reason: format!(
                                    "Edge label with ID {} not found in interner",
                                    edge.label.as_u32()
                                ),
                            })
                        })?;

                    // Create edge from New Source -> Survivor
                    tx.create_edge(new_source, survivor, &label, edge.properties)?;
                    // Delete old edge
                    tx.delete_edge(edge_id)?;
                }
            }

            // 2. Merge Properties
            // Strategy: Survivor Wins. We only add properties from Victim that Survivor lacks.
            let victim_node = tx.get_node(victim)?;
            let survivor_node = tx.get_node(survivor)?;

            let mut props_to_add = PropertyMapBuilder::new();
            let mut has_changes = false;

            for (key, val) in victim_node.properties.iter() {
                // Optimized check using interned keys
                if !survivor_node.properties.contains_interned_key(key) {
                    props_to_add = props_to_add.try_insert_by_key(*key, val.clone())?;
                    has_changes = true;
                }
            }

            if has_changes {
                tx.update_node(survivor, props_to_add.build())?;
            }

            // 3. Delete Victim
            // We have manually deleted all edges, so use delete_node to avoid cascade issues with self-loops
            tx.delete_node(victim)?;

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::vector::{DistanceMetric, HnswConfig};
    use tempfile::tempdir;

    fn create_test_db() -> (AletheiaDB, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let wal_path = dir.path().join("wal");
        let data_path = dir.path().join("data");
        std::fs::create_dir_all(&wal_path).unwrap();
        std::fs::create_dir_all(&data_path).unwrap();

        let wal_config = crate::config::WalConfigBuilder::new()
            .wal_dir(wal_path)
            .build();

        let persistence_config = crate::storage::index_persistence::PersistenceConfig {
            data_dir: data_path,
            enabled: false,
            ..Default::default()
        };

        let config = crate::AletheiaDBConfig::builder()
            .wal(wal_config)
            .persistence(persistence_config)
            .build();

        (AletheiaDB::with_unified_config(config).unwrap(), dir)
    }

    #[test]
    fn test_detect_duplicates() {
        let (db, _dir) = create_test_db();

        // Enable vector index
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();

        // 1. Create Target Node: "John Smith" [1.0, 0.0]
        let props1 = PropertyMapBuilder::new()
            .insert("name", "John Smith")
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let target = db.create_node("Person", props1).unwrap();

        // 2. Create Duplicate: "J. Smith" [0.99, 0.01] (Very similar)
        let props2 = PropertyMapBuilder::new()
            .insert("name", "J. Smith")
            .insert_vector("embedding", &[0.99, 0.01])
            .build();
        let duplicate = db.create_node("Person", props2).unwrap();

        // 3. Create Distinct: "Alice" [0.0, 1.0] (Orthogonal)
        let props3 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert_vector("embedding", &[0.0, 1.0])
            .build();
        let distinct = db.create_node("Person", props3).unwrap();

        let detector = HighlanderDetector::new(&db);
        let duplicates = detector.find_duplicates(target, 0.9, 5).unwrap();

        // Should find "J. Smith"
        assert!(!duplicates.is_empty(), "Should find at least one duplicate");
        assert_eq!(duplicates[0].0, duplicate);
        assert!(duplicates[0].1 > 0.9);

        // Should NOT find "Alice"
        assert!(
            !duplicates.iter().any(|(id, _)| *id == distinct),
            "Should not find orthogonal vector"
        );
    }

    #[test]
    fn test_merge_entities() {
        let (db, _dir) = create_test_db();

        // 1. Create Survivor: "Survivor"
        let props1 = PropertyMapBuilder::new()
            .insert("name", "Survivor")
            .insert("age", 30)
            .build();
        let survivor = db.create_node("Person", props1).unwrap();

        // 2. Create Victim: "Victim"
        let props2 = PropertyMapBuilder::new()
            .insert("name", "Victim") // Should be overwritten? Or ignored? Strategy dependent.
            .insert("city", "London") // Should be added to Survivor
            .build();
        let victim = db.create_node("Person", props2).unwrap();

        // 3. Create Edge to Victim: Friend -> Victim
        let friend = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(
            friend,
            victim,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020).build(),
        )
        .unwrap();

        // 4. Create Edge from Victim: Victim -> Place
        let place = db
            .create_node("Place", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(victim, place, "LIVES_IN", PropertyMapBuilder::new().build())
            .unwrap();

        let merger = EntityMerger::new(&db);
        merger.merge(survivor, victim).unwrap();

        // Verify Victim is deleted
        assert!(
            db.get_node(victim).is_err(),
            "Victim node should be deleted"
        );

        // Verify Edges Moved
        // Friend -> Survivor
        let outgoing_friend = db.get_outgoing_edges_with_label(friend, "KNOWS");
        assert_eq!(outgoing_friend.len(), 1);
        let edge_id = outgoing_friend[0];
        let edge = db
            .get_edge(edge_id)
            .unwrap_or_else(|_| panic!("Failed to get friend edge {:?}", edge_id));
        assert_eq!(edge.target, survivor, "Edge should point to Survivor");
        assert_eq!(
            edge.properties.get("since").unwrap().as_int(),
            Some(2020),
            "Edge properties should be preserved"
        );

        // Survivor -> Place
        let outgoing_survivor = db.get_outgoing_edges_with_label(survivor, "LIVES_IN");
        assert_eq!(outgoing_survivor.len(), 1);
        let edge_id = outgoing_survivor[0];
        let edge = db
            .get_edge(edge_id)
            .unwrap_or_else(|_| panic!("Failed to get edge {:?}", edge_id));
        assert_eq!(edge.target, place, "Edge should start from Survivor");

        // Verify Properties Merged
        let survivor_node = db.get_node(survivor).unwrap();
        assert_eq!(
            survivor_node.properties.get("name").unwrap().as_str(),
            Some("Survivor"),
            "Survivor name should be preserved"
        );
        assert_eq!(
            survivor_node.properties.get("city").unwrap().as_str(),
            Some("London"),
            "City should be copied from Victim"
        );
    }

    #[test]
    fn test_merge_self_error() {
        let (db, _dir) = create_test_db();
        let node = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let merger = EntityMerger::new(&db);
        let result = merger.merge(node, node);
        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("Cannot merge a node into itself"));
    }

    #[test]
    fn test_merge_self_loops() {
        let (db, _dir) = create_test_db();
        let survivor = db
            .create_node("Survivor", PropertyMapBuilder::new().build())
            .unwrap();
        let victim = db
            .create_node("Victim", PropertyMapBuilder::new().build())
            .unwrap();

        // Create self-loop on victim: Victim -> Victim
        let _old_edge = db
            .create_edge(victim, victim, "SELF", PropertyMapBuilder::new().build())
            .unwrap();

        let merger = EntityMerger::new(&db);
        merger.merge(survivor, victim).unwrap();

        // Should become Survivor -> Survivor
        let outgoing = db.get_outgoing_edges_with_label(survivor, "SELF");
        assert_eq!(outgoing.len(), 1);
        let edge = db.get_edge(outgoing[0]).unwrap();
        assert_eq!(edge.source, survivor);
        assert_eq!(edge.target, survivor);
    }

    #[test]
    fn test_merge_connected_nodes() {
        let (db, _dir) = create_test_db();
        let survivor = db
            .create_node("Survivor", PropertyMapBuilder::new().build())
            .unwrap();
        let victim = db
            .create_node("Victim", PropertyMapBuilder::new().build())
            .unwrap();

        // Edge: Survivor -> Victim
        db.create_edge(survivor, victim, "LINK", PropertyMapBuilder::new().build())
            .unwrap();

        // Edge: Victim -> Survivor
        db.create_edge(victim, survivor, "BACK", PropertyMapBuilder::new().build())
            .unwrap();

        let merger = EntityMerger::new(&db);
        merger.merge(survivor, victim).unwrap();

        // Survivor -> Victim should become Survivor -> Survivor (Self loop)
        let outgoing_link = db.get_outgoing_edges_with_label(survivor, "LINK");
        assert_eq!(outgoing_link.len(), 1);
        let edge_link = db.get_edge(outgoing_link[0]).unwrap();
        assert_eq!(edge_link.source, survivor);
        assert_eq!(edge_link.target, survivor);

        // Victim -> Survivor should become Survivor -> Survivor (Self loop)
        let outgoing_back = db.get_outgoing_edges_with_label(survivor, "BACK");
        assert_eq!(outgoing_back.len(), 1);
        let edge_back = db.get_edge(outgoing_back[0]).unwrap();
        assert_eq!(edge_back.source, survivor);
        assert_eq!(edge_back.target, survivor);
    }
}
