//! Telekinesis: Dynamic Structure Modification based on Vector Similarity
//!
//! The "Telekinesis" engine enables dynamic structure modification based on vector
//! queries. It automatically inserts links between previously unlinked nodes if they
//! are semantically similar.
//!
//! This is useful for building self-organizing knowledge graphs, where entities
//! that share similar semantic meaning organically form relationships over time.

use crate::AletheiaDB;
use crate::api::transaction::{ReadOps, WriteOps};
use crate::core::error::Result;
use crate::core::property::PropertyMapBuilder;

/// The Telekinesis Engine.
pub struct TelekinesisEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> TelekinesisEngine<'a> {
    /// Create a new TelekinesisEngine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Run the telekinesis process to attract similar nodes and form links.
    ///
    /// # Arguments
    /// * `vector_property` - The name of the vector property to compare.
    /// * `threshold` - The minimum cosine similarity required to form a link.
    /// * `label` - The label to use for the newly created edges.
    ///
    /// # Returns
    /// The number of new edges created.
    pub fn attract(&self, vector_property: &str, threshold: f32, label: &str) -> Result<usize> {
        let mut edges_created = 0;

        // 1. Scan the database for all nodes that have the vector property.
        // We do a full scan, but realistically this might be expensive.
        // Alternatively, we could just rely on vector searches.
        let results = self.db.query().scan(None).execute(self.db)?;

        let mut node_ids = Vec::new();
        for row in results {
            let row = row?;
            #[allow(clippy::collapsible_if)]
            if let Some(node) = row.entity.as_node() {
                if node.properties.contains_key(vector_property) {
                    node_ids.push(node.id);
                }
            }
        }

        // 2. We use a transaction to add edges
        self.db.write(|tx| {
            // For each node, find similar nodes
            // Note: Since `search_vectors_in` requires the actual vector, we could get it
            // but for simplicity we will just do an N^2 check or rely on index searches.
            // Let's rely on index search.

            for &node_id in &node_ids {
                // Read from the transaction to avoid deadlocks
                #[allow(clippy::collapsible_if)]
                if let Ok(node) = tx.get_node(node_id) {
                    #[allow(clippy::collapsible_if)]
                    if let Some(prop) = node.properties.get(vector_property) {
                        if let Some(vec) = prop.as_vector() {
                            if let Ok(similar_nodes) =
                                tx.current.search_vectors_in(vector_property, vec, 10)
                            {
                                for (similar_id, sim_score) in similar_nodes {
                                    // Skip self
                                    if similar_id == node_id {
                                        continue;
                                    }

                                    // Only connect if score exceeds threshold
                                    if sim_score >= threshold {
                                        // Check if an edge already exists using `tx`
                                        let outgoing = tx.get_outgoing_edges(node_id);
                                        let mut already_connected = false;
                                        for edge_id in outgoing {
                                            #[allow(clippy::collapsible_if)]
                                            if let Ok(edge) = tx.get_edge(edge_id) {
                                                if edge.target == similar_id
                                                    && edge.has_label_str(label)
                                                {
                                                    already_connected = true;
                                                    break;
                                                }
                                            }
                                        }

                                        if !already_connected {
                                            // Create the edge
                                            let props = PropertyMapBuilder::new()
                                                .insert("similarity", sim_score as f64)
                                                .build();

                                            if tx
                                                .create_edge(node_id, similar_id, label, props)
                                                .is_ok()
                                            {
                                                edges_created += 1;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok::<_, crate::core::error::Error>(())
        })?;

        Ok(edges_created)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_telekinesis_attracts_similar_nodes() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Node A: [1.0, 0.0]
        // Node B: [0.99, 0.1] - Similar
        // Node C: [0.0, 1.0] - Not Similar

        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let node_a = db.create_node("Node", props_a).unwrap();

        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.99, 0.1])
            .build();
        let node_b = db.create_node("Node", props_b).unwrap();

        let props_c = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let node_c = db.create_node("Node", props_c).unwrap();

        let telekinesis = TelekinesisEngine::new(&db);

        // Threshold 0.9. Expecting an edge between A and B, but not C.
        let edges_created = telekinesis.attract("vec", 0.9, "SEMANTIC_LINK").unwrap();

        // Either A->B or B->A might be created. Or both depending on logic.
        // In our logic, it iterates A, then B. So it might create A->B and B->A.
        // We expect at least 1 edge.
        assert!(edges_created >= 1);

        // Verify A->B exists
        let outgoing_a = db.get_outgoing_edges(node_a);
        let mut found_a_to_b = false;
        let mut found_a_to_c = false;

        for eid in outgoing_a {
            if let Ok(edge) = db.get_edge(eid) {
                if edge.target == node_b && edge.has_label_str("SEMANTIC_LINK") {
                    found_a_to_b = true;
                }
                if edge.target == node_c {
                    found_a_to_c = true;
                }
            }
        }

        assert!(found_a_to_b, "Expected edge from A to B");
        assert!(!found_a_to_c, "Did not expect edge from A to C");
    }
}
