//! Compass: Semantic Direction Finder
//!
//! "Which way is true north?"
//!
//! The Compass module provides spatial vector guidance combined with graph traversals.
//! It answers questions like:
//! - "Given my current node, which neighbors point most directly towards this target concept vector?"
//! - "Which path should I take to get closer to a specific semantic idea?"
//!
//! # Theory
//!
//! It evaluates the outgoing neighbors of a given node, extracts their vector property,
//! and computes the cosine similarity against a target vector. The neighbors are then
//! ranked by their similarity score, acting as a compass pointing the way through the graph.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::compass::Compass;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let current_node = aletheiadb::core::id::NodeId::new(0).unwrap();
//! let compass = Compass::new(&db);
//!
//! let target_concept = vec![0.5, 0.8]; // Example target vector
//! let bearings = compass.find_bearing(current_node, &target_concept, "embedding")?;
//!
//! if let Some((best_node, score)) = bearings.first() {
//!     println!("Best path is towards node {} with similarity score {}", best_node, score);
//! }
//! # Ok(())
//! # }
//! ```

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::cosine_similarity;

/// The Semantic Compass for vector-guided graph traversal.
pub struct Compass<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Compass<'a> {
    /// Create a new Compass instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find the best outgoing neighbors that point towards the target vector.
    ///
    /// # Arguments
    ///
    /// * `current_node` - The node to start from.
    /// * `target_vector` - The semantic concept vector we want to move towards.
    /// * `vector_property` - The property key containing the node vectors.
    ///
    /// # Returns
    ///
    /// A list of `(NodeId, f32)` tuples, representing the target nodes of outgoing edges
    /// and their cosine similarity score to the `target_vector`, sorted descending
    /// by similarity. Nodes without the specified vector property are skipped.
    pub fn find_bearing(
        &self,
        current_node: NodeId,
        target_vector: &[f32],
        vector_property: &str,
    ) -> Result<Vec<(NodeId, f32)>> {
        let mut bearings = Vec::new();
        let outgoing_edges = self.db.get_outgoing_edges(current_node);

        for edge_id in outgoing_edges {
            let edge = self.db.get_edge(edge_id)?;
            let neighbor_node = self.db.get_node(edge.target)?;

            if let Some(prop) = neighbor_node.get_property(vector_property) {
                if let Some(neighbor_vec) = prop.as_vector() {
                    // Silently skip neighbors where cosine similarity fails
                    // (e.g., due to dimension mismatch) rather than failing the whole query
                    if let Ok(sim) = cosine_similarity(target_vector, neighbor_vec) {
                        bearings.push((edge.target, sim));
                    }
                }
            }
        }

        // Sort descending by similarity score, then by NodeId to group duplicates
        bearings.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        // Deduplicate targets in case of multiple edges to the same node
        bearings.dedup_by_key(|(id, _)| *id);

        Ok(bearings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::error::Error;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_compass_find_bearing() {
        let db = AletheiaDB::new().unwrap();

        // Target Concept: [1.0, 1.0] (Northeast)

        // Node A: Origin
        let a = db
            .write(|tx| {
                tx.create_node(
                    "Origin",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0])
                        .build(),
                )
            })
            .unwrap();

        // Node B: [1.0, 0.0] (East)
        let b = db
            .write(|tx| {
                tx.create_node(
                    "Point",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
            })
            .unwrap();

        // Node C: [0.9, 0.9] (Northeast - closest to target)
        let c = db
            .write(|tx| {
                tx.create_node(
                    "Point",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.9, 0.9])
                        .build(),
                )
            })
            .unwrap();

        // Node D: [-1.0, -1.0] (Southwest - opposite)
        let d = db
            .write(|tx| {
                tx.create_node(
                    "Point",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[-1.0, -1.0])
                        .build(),
                )
            })
            .unwrap();

        // Node E: Missing vector property
        let e = db
            .write(|tx| {
                tx.create_node(
                    "Point",
                    PropertyMapBuilder::new().insert("name", "E").build(),
                )
            })
            .unwrap();

        // Connect A to B, C, D, E
        db.write(|tx| {
            tx.create_edge(a, b, "PATH", PropertyMapBuilder::new().build())?;
            tx.create_edge(a, c, "PATH", PropertyMapBuilder::new().build())?;
            tx.create_edge(a, d, "PATH", PropertyMapBuilder::new().build())?;
            tx.create_edge(a, e, "PATH", PropertyMapBuilder::new().build())?;
            Ok::<(), Error>(())
        })
        .unwrap();

        let compass = Compass::new(&db);
        let target = vec![1.0, 1.0];

        let bearings = compass.find_bearing(a, &target, "vec").unwrap();

        // Expect 3 results (E is skipped)
        assert_eq!(bearings.len(), 3);

        // Node C should be first (closest to Northeast)
        assert_eq!(bearings[0].0, c);
        assert!(bearings[0].1 > 0.9);

        // Node B should be second
        assert_eq!(bearings[1].0, b);

        // Node D should be last (opposite)
        assert_eq!(bearings[2].0, d);
        assert!(bearings[2].1 < 0.0);
    }
}
