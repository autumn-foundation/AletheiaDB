//! Mirage: Semantic Ghost Node Injection.
//!
//! "What if?"
//!
//! Mirage provides an overlay on top of AletheiaDB, allowing users to inject ephemeral "ghost nodes"
//! into the semantic space without committing them to the database or affecting the transaction history.
//! This enables counterfactual "What-If" semantic analysis, such as answering questions like:
//! - "If we introduced a product with this embedding, which existing users would be closest to it?"
//! - "Let's create a hypothetical 'Perfect Candidate' node and find its k-NN neighbors."

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::graph::Node;
use crate::core::id::{MAX_VALID_ID, NodeId};
use crate::core::property::PropertyMap;
use crate::core::version::VersionMetadata;
use crate::core::{GLOBAL_INTERNER, VersionId};
use std::collections::HashMap;

/// An ephemeral overlay that blends real nodes from an underlying `AletheiaDB` with hypothetical "ghost" nodes.
pub struct MirageDB<'a> {
    db: &'a AletheiaDB,
    ghost_nodes: HashMap<NodeId, Node>,
}

impl<'a> MirageDB<'a> {
    /// Creates a new `MirageDB` overlaying the given database.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            db,
            ghost_nodes: HashMap::new(),
        }
    }

    /// Injects a new ghost node into the mirage overlay.
    /// Ghost node IDs will be generated starting from `MAX_VALID_ID + 1`.
    pub fn inject_ghost_node(&mut self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        let interned_label = GLOBAL_INTERNER.intern(label);

        let id_val = MAX_VALID_ID - (self.ghost_nodes.len() as u64);
        let node_id = NodeId::new(id_val)?;

        let ghost = Node {
            id: node_id,
            label: interned_label?,
            properties,
            current_version: VersionId::new(0)?, // Dummy version
            metadata: VersionMetadata::new(
                crate::core::id::TxId::new(0),
                crate::core::temporal::time::now(),
            ), // Dummy metadata
        };

        self.ghost_nodes.insert(node_id, ghost);
        Ok(node_id)
    }

    /// Retrieves a node by ID, checking the ghost nodes first, then falling back to the real database.
    pub fn get_node(&self, id: NodeId) -> Result<Node> {
        if let Some(ghost) = self.ghost_nodes.get(&id) {
            Ok(ghost.clone())
        } else {
            // Read from DB using a read transaction
            self.db
                .read(|tx| crate::api::transaction::ReadOps::get_node(tx, id))
        }
    }

    /// Finds similar ghost nodes in the overlay.
    fn find_similar_ghosts(
        &self,
        vector_property: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let mut distances = Vec::new();

        for ghost in self.ghost_nodes.values() {
            #[allow(clippy::collapsible_if)]
            if let Some(prop_val) = ghost.properties.get(vector_property) {
                if let Some(vec) = prop_val.as_vector() {
                    // Calculate cosine distance
                    let dist = self.cosine_distance(embedding, vec);
                    distances.push((ghost.id, dist));
                }
            }
        }

        // Sort by distance (ascending)
        distances.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        distances.truncate(k);
        Ok(distances)
    }

    /// Finds similar nodes, blending results from the real database and ghost nodes.
    pub fn find_similar_mixed(
        &self,
        vector_property: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        // Query the underlying database for similar nodes.
        // The db implementation usually provides a way to find similar items based on vector and metric
        // We can get similar nodes via the internal vector similarity lookup.
        let mut db_distances =
            self.db
                .current
                .find_similar_by_embedding_in(vector_property, embedding, k)?;

        // Find similar ghost nodes
        let ghost_distances = self.find_similar_ghosts(vector_property, embedding, k)?;

        // Blend and sort
        db_distances.extend(ghost_distances);
        db_distances.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        db_distances.truncate(k);

        Ok(db_distances)
    }

    fn cosine_distance(&self, a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() || a.is_empty() {
            return f32::MAX;
        }
        let mut dot = 0.0;
        let mut norm_a = 0.0;
        let mut norm_b = 0.0;
        for i in 0..a.len() {
            dot += a[i] * b[i];
            norm_a += a[i] * a[i];
            norm_b += b[i] * b[i];
        }
        if norm_a == 0.0 || norm_b == 0.0 {
            return f32::MAX;
        }
        let cos_sim = dot / (norm_a.sqrt() * norm_b.sqrt());
        1.0 - cos_sim
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_mirage_inject_and_query() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("embedding", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Add a real node
        let props_real = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let real_id = db.create_node("Real", props_real).unwrap();

        // Create Mirage overlay
        let mut mirage = MirageDB::new(&db);

        // Inject ghost node
        let props_ghost = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 1.0])
            .build();
        let ghost_id = mirage.inject_ghost_node("Ghost", props_ghost).unwrap();

        // We can retrieve both through mirage
        let retrieved_real = mirage.get_node(real_id).unwrap();
        assert_eq!(retrieved_real.id, real_id);

        let retrieved_ghost = mirage.get_node(ghost_id).unwrap();
        assert_eq!(retrieved_ghost.id, ghost_id);

        // Query mixed: Vector close to ghost
        let query_vec = &[0.1, 0.9];
        let mixed_results = mirage
            .find_similar_mixed("embedding", query_vec, 2)
            .unwrap();

        assert_eq!(mixed_results.len(), 2);
        // Ghost should be closest
        assert_eq!(mixed_results[0].0, ghost_id);
        assert_eq!(mixed_results[1].0, real_id);
    }
}
