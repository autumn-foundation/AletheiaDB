//! Temporal Indexer.
//!
//! This module allows indexing "Temporal Fingerprints" as vector properties.
//! This enables O(log N) search for nodes with similar behavioral patterns
//! using the existing Vector Search engine.

use crate::GallifreyDB;
use crate::experimental::echo::EchoChamber;
use crate::core::property::PropertyMapBuilder;
use crate::utils::error::Result;

/// A tool for materializing temporal fingerprints into node properties.
pub struct TemporalIndexer<'a> {
    db: &'a GallifreyDB,
}

impl<'a> TemporalIndexer<'a> {
    /// Create a new TemporalIndexer.
    pub fn new(db: &'a GallifreyDB) -> Self {
        Self { db }
    }

    /// Index all nodes by generating their temporal fingerprint and storing it
    /// as a vector property.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property name to store the fingerprint (e.g., "_temporal_fingerprint").
    ///
    /// # Returns
    ///
    /// The number of nodes updated.
    pub fn index_all_nodes(&self, property_name: &str) -> Result<usize> {
        let chamber = EchoChamber::new(self.db);
        let node_ids = self.db.current.get_all_node_ids();
        let mut count = 0;

        for node_id in node_ids {
            // Generate fingerprint
            // We ignore errors for individual nodes (e.g. if history is missing)
            if let Ok(fp) = chamber.generate_fingerprint(node_id) {
                // Update node property
                // We use a separate write transaction for each node (or small batches)
                // to avoid holding a massive lock.
                // Note: updating the node adds a history entry!
                // This is the "Observer Effect".

                let property_map = PropertyMapBuilder::new()
                    .insert_vector(property_name, &fp.bins)
                    .build();

                // We use update_node, which merges properties.
                // We wrap it in a transaction block.
                // We ignore errors here to continue indexing other nodes.
                if let Ok(_) = self.db.write(|tx| {
                    use crate::api::transaction::WriteOps;
                    tx.update_node(node_id, property_map)
                }) {
                    count += 1;
                }
            }
        }

        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::temporal::{time};
    use crate::index::vector::{HnswConfig, DistanceMetric};

    #[test]
    fn test_temporal_indexing_flow() {
        let db = GallifreyDB::new().unwrap();

        // 1. Setup: Create a node with some history
        let now = time::now().wallclock();
        let t1 = HybridTimestamp::new(now - 60 * 1_000_000, 0).unwrap();
        let t2 = HybridTimestamp::new(now - 30 * 1_000_000, 0).unwrap();

        let props = PropertyMapBuilder::new().insert("val", 1).build();

        let node_id = {
             // Workaround for Core Bug: Cannot Create and Update in same transaction
             // because update_node checks storage instead of write buffer.
             let mut tx = db.write_transaction().unwrap();
             let id = tx.create_node_with_valid_time("Node", props.clone(), Some(t1)).unwrap();
             tx.commit().unwrap();

             let mut tx2 = db.write_transaction().unwrap();
             tx2.update_node_with_valid_time(id, props.clone(), Some(t2)).unwrap();
             tx2.commit().unwrap();
             id
        };

        // 2. Enable Vector Index for the temporal fingerprint
        let index_prop = "_echo";
        let config = HnswConfig::new(60, DistanceMetric::Cosine); // Default Resonator has 60 bins
        db.enable_vector_index(index_prop, config).unwrap();

        // 3. Run Indexer
        let indexer = TemporalIndexer::new(&db);
        let count = indexer.index_all_nodes(index_prop).unwrap();
        assert_eq!(count, 1);

        // 4. Verify Property exists
        let node = db.get_node(node_id).unwrap();
        let vec_val = node.properties.get(index_prop).unwrap();
        assert!(vec_val.as_vector().is_some());

        // 5. Verify Search
        // We search using the node's own vector (it should find itself if we don't exclude it,
        // but find_similar excludes query node).
        // Let's create a "Twin" node with same history.
        let twin_id = {
             let mut tx = db.write_transaction().unwrap();
             let id = tx.create_node_with_valid_time("Node", props.clone(), Some(t1)).unwrap();
             tx.commit().unwrap();

             let mut tx2 = db.write_transaction().unwrap();
             tx2.update_node_with_valid_time(id, props.clone(), Some(t2)).unwrap();
             tx2.commit().unwrap();
             id
        };

        // Re-index to catch the twin
        indexer.index_all_nodes(index_prop).unwrap();

        // Now search for Twin using original node
        let results = db.find_similar_in(index_prop, node_id, 10).unwrap();

        assert!(!results.is_empty());
        assert_eq!(results[0].0, twin_id);
        assert!(results[0].1 > 0.9); // Should be very similar
    }
}
