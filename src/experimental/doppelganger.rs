#![allow(clippy::collapsible_if)]

//! Doppelganger: Semantic Clone Detection.
//!
//! "Same soul, different worlds."
//!
//! The Doppelganger Engine finds nodes that are almost identical in semantic meaning
//! but exist in completely different structural contexts.
//!
//! # Concepts
//! - **Doppelganger**: A node pair with extremely high cosine similarity but near-zero
//!   structural overlap (they share no neighbors).
//! - **Structural Overlap**: The Jaccard index of the neighbor sets of two nodes.
//!
//! # Use Cases
//! - **Duplicate Entity Resolution**: Identify two "User" profiles that represent the same
//!   semantic persona but have interacted with completely different parts of the system.
//! - **Parallel Evolution**: Discover two distinct communities that independently evolved
//!   the exact same concept.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::doppelganger::DoppelgangerEngine;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//!
//! let engine = DoppelgangerEngine::new(&db);
//! let clones = engine.find_doppelgangers("embedding", 0.95, 0.1)?;
//!
//! for pair in clones {
//!     println!("Found doppelgangers: {} and {} (Similarity: {:.2})",
//!              pair.node_a, pair.node_b, pair.similarity);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::collections::HashSet;

/// A pair of nodes that are semantically identical but structurally distinct.
#[derive(Debug, Clone)]
pub struct DoppelgangerPair {
    /// The first node.
    pub node_a: NodeId,
    /// The second node.
    pub node_b: NodeId,
    /// The semantic similarity between them (e.g., cosine similarity).
    pub similarity: f32,
    /// The Jaccard index of their neighbor sets.
    pub structural_overlap: f32,
}

/// The Doppelganger Engine.
pub struct DoppelgangerEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DoppelgangerEngine<'a> {
    /// Create a new Doppelganger Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find all doppelganger pairs in the graph based on a vector property.
    ///
    /// # Arguments
    /// * `property` - The name of the vector property to compare.
    /// * `min_similarity` - The minimum cosine similarity to be considered a doppelganger (e.g., 0.95).
    /// * `max_structural_overlap` - The maximum Jaccard index of neighbor overlap (e.g., 0.1).
    pub fn find_doppelgangers(
        &self,
        property: &str,
        min_similarity: f32,
        max_structural_overlap: f32,
    ) -> Result<Vec<DoppelgangerPair>> {
        // 1. Gather all nodes that have the specified vector property
        let mut nodes_with_vectors = Vec::new();
        let results = self.db.query().scan(None).execute(self.db)?;

        for row in results {
            let row = row?;
            if let Some(node) = row.entity.as_node() {
                if let Some(prop) = node.get_property(property).and_then(|p| p.as_vector()) {
                    nodes_with_vectors.push((node.id, prop.to_vec()));
                }
            }
        }

        if nodes_with_vectors.is_empty() {
            return Ok(Vec::new());
        }

        // Pre-compute neighbor sets for all these nodes to avoid repeated queries
        let mut neighbor_sets = std::collections::HashMap::new();
        for (node_id, _) in &nodes_with_vectors {
            neighbor_sets.insert(*node_id, self.get_all_neighbors(*node_id)?);
        }

        let mut doppelgangers = Vec::new();

        // 2. Perform pairwise comparison
        // Optimization: In a real system, we'd use the vector index.
        // For the Nova prototype, O(N^2) pairwise over the filtered set is acceptable.
        for i in 0..nodes_with_vectors.len() {
            for j in (i + 1)..nodes_with_vectors.len() {
                let (id_a, mut vec_a) = nodes_with_vectors[i].clone();
                let (id_b, mut vec_b) = nodes_with_vectors[j].clone();

                if vec_a.len() != vec_b.len() {
                    continue; // Dimension mismatch, cannot compare
                }

                // Calculate semantic similarity
                ops::normalize_in_place(&mut vec_a);
                ops::normalize_in_place(&mut vec_b);
                let sim = ops::cosine_similarity(&vec_a, &vec_b)?;

                if sim >= min_similarity {
                    // Check structural overlap
                    let neighbors_a = neighbor_sets.get(&id_a).unwrap();
                    let neighbors_b = neighbor_sets.get(&id_b).unwrap();

                    let overlap = self.calculate_jaccard_index(neighbors_a, neighbors_b);

                    if overlap <= max_structural_overlap {
                        doppelgangers.push(DoppelgangerPair {
                            node_a: id_a,
                            node_b: id_b,
                            similarity: sim,
                            structural_overlap: overlap,
                        });
                    }
                }
            }
        }

        // Sort by similarity descending, then by overlap ascending
        doppelgangers.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    a.structural_overlap
                        .partial_cmp(&b.structural_overlap)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });

        Ok(doppelgangers)
    }

    /// Helper to get all neighbors (incoming + outgoing) for a node.
    fn get_all_neighbors(&self, node_id: NodeId) -> Result<HashSet<NodeId>> {
        let mut neighbors = HashSet::new();

        // Use read closure to safely query connections
        self.db.read(|tx| {
            // Outgoing edges
            let outgoing = tx.get_outgoing_edges(node_id);
            for edge_id in outgoing {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    neighbors.insert(edge.target);
                }
            }

            // Incoming edges
            let incoming = tx.get_incoming_edges(node_id);
            for edge_id in incoming {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    neighbors.insert(edge.source);
                }
            }
            Ok::<(), Error>(())
        })?;

        Ok(neighbors)
    }

    /// Calculate the Jaccard Index of two sets.
    fn calculate_jaccard_index(&self, set_a: &HashSet<NodeId>, set_b: &HashSet<NodeId>) -> f32 {
        if set_a.is_empty() && set_b.is_empty() {
            // If neither has any connections, they are completely structurally identical (isolated).
            // Thus, overlap is 1.0 (they share the exact same structural context: nothing).
            return 1.0;
        }

        let intersection_count = set_a.intersection(set_b).count() as f32;
        let union_count = set_a.union(set_b).count() as f32;

        intersection_count / union_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_find_doppelgangers() {
        let db = AletheiaDB::new().unwrap();

        let mut a1 = NodeId::new(0).unwrap();
        let mut a2 = NodeId::new(0).unwrap();
        let mut c1 = NodeId::new(0).unwrap();
        let mut c2 = NodeId::new(0).unwrap();

        db.write(|tx| {
            // A1 and A2 are semantically identical (same vector)
            a1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();

            a2 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();

            // C1 and C2 are distinct contexts
            c1 = tx.create_node("Context", Default::default()).unwrap();
            c2 = tx.create_node("Context", Default::default()).unwrap();

            // A1 connects to C1
            tx.create_edge(a1, c1, "LINK", Default::default()).unwrap();
            // A2 connects to C2
            tx.create_edge(a2, c2, "LINK", Default::default()).unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let engine = DoppelgangerEngine::new(&db);

        // Find pairs with >90% similarity but <10% shared neighbors
        let clones = engine.find_doppelgangers("vec", 0.9, 0.1).unwrap();

        assert_eq!(clones.len(), 1);
        let pair = &clones[0];

        // It found A1 and A2
        assert!(
            (pair.node_a == a1 && pair.node_b == a2) || (pair.node_a == a2 && pair.node_b == a1)
        );
        assert!(pair.similarity > 0.99); // They are identical
        assert_eq!(pair.structural_overlap, 0.0); // They share no neighbors
    }

    #[test]
    fn test_rejects_clones_with_shared_structure() {
        let db = AletheiaDB::new().unwrap();

        let mut a1 = NodeId::new(0).unwrap();
        let mut a2 = NodeId::new(0).unwrap();
        let mut shared_context = NodeId::new(0).unwrap();

        db.write(|tx| {
            // A1 and A2 are semantically identical
            a1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0, 0.0])
                        .build(),
                )
                .unwrap();

            a2 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0, 0.0])
                        .build(),
                )
                .unwrap();

            // They share the exact same context
            shared_context = tx.create_node("Context", Default::default()).unwrap();

            tx.create_edge(a1, shared_context, "LINK", Default::default())
                .unwrap();
            tx.create_edge(a2, shared_context, "LINK", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let engine = DoppelgangerEngine::new(&db);

        // Max structural overlap is 0.1. Since they share 100% of neighbors, they should be rejected.
        let clones = engine.find_doppelgangers("vec", 0.9, 0.1).unwrap();

        assert_eq!(clones.len(), 0);
    }
}
