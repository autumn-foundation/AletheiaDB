//! Mosaic: Compositional Semantic Search Engine.
//!
//! "How do I build this concept out of the concepts I already have?"
//!
//! Standard semantic search answers: "What is the single closest node to this query?"
//! Mosaic answers: "What **combination** of nodes adds up to this query?"
//!
//! # The Spark 💡
//! We often query for complex ideas that don't exist perfectly as a single node.
//! If I search for a "flying electric car", and the database only has "airplane", "Tesla", and "car",
//! standard search will just return the closest single match (maybe "airplane").
//! Mosaic will return a set of nodes whose combined vectors construct the query.
//!
//! # The Feature 🚀
//! Mosaic takes a target vector and iteratively finds the best node to reduce the
//! "residual" vector (Target - Current Sum). It stops when the residual is small
//! or a maximum number of pieces is reached.
//!
//! # The Potential 🔮
//! - **Generative RAG**: Instead of returning a single document, return a "mosaic" of
//!   documents that together cover all aspects of a complex user query.
//! - **Concept Decomposition**: "What is this new node made of?"
//! - **Creative Synthesis**: Finding the "ingredients" for a new idea.

#![allow(clippy::needless_range_loop, clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
#[cfg(feature = "nova")]
use crate::core::vector::ops::magnitude;

/// A single piece of the semantic mosaic.
#[derive(Debug, Clone, PartialEq)]
pub struct MosaicPiece {
    /// The node providing this piece of the concept.
    pub node_id: NodeId,
    /// How much this node contributed to reducing the residual.
    /// (Similarity to the residual at the time it was selected).
    pub contribution_score: f32,
}

/// The result of a compositional search.
#[derive(Debug, Clone)]
pub struct MosaicResult {
    /// The pieces that make up the target concept.
    pub pieces: Vec<MosaicPiece>,
    /// The final magnitude of the remaining residual vector.
    /// A value close to 0.0 means perfect reconstruction.
    pub final_residual_magnitude: f32,
}

/// The Mosaic Engine.
pub struct Mosaic<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Mosaic<'a> {
    /// Create a new Mosaic instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Compose a target concept from existing nodes.
    ///
    /// # Arguments
    /// * `target_vector` - The concept we want to build.
    /// * `property` - The vector property to search over.
    /// * `max_pieces` - Maximum number of nodes to use in the composition.
    /// * `stop_threshold` - Stop adding pieces if the residual magnitude falls below this.
    pub fn compose(
        &self,
        target_vector: &[f32],
        property: &str,
        max_pieces: usize,
        stop_threshold: f32,
    ) -> Result<MosaicResult> {
        let mut residual = target_vector.to_vec();
        let mut pieces = Vec::new();
        let mut used_nodes = std::collections::HashSet::new();

        for _ in 0..max_pieces {
            let res_mag = magnitude(&residual);
            if res_mag <= stop_threshold {
                break;
            }

            // Find the single node most similar to our CURRENT residual
            // We ask for a few in case the top one is already used.
            let candidates = self.db.search_vectors_in(property, &residual, 10)?;

            let mut best_piece = None;
            let mut best_vec = None;

            for (node_id, score) in candidates {
                if used_nodes.contains(&node_id) {
                    continue;
                }

                // We need the actual vector of this node to subtract it from the residual.
                if let Ok(node) = self.db.get_node(node_id) {
                    if let Some(prop) = node.properties.get(property) {
                        if let Some(vec) = prop.as_vector() {
                            best_piece = Some(MosaicPiece {
                                node_id,
                                contribution_score: score,
                            });
                            best_vec = Some(vec.to_vec());
                            break; // We found the best unused candidate
                        }
                    }
                }
            }

            match (best_piece, best_vec) {
                (Some(piece), Some(vec)) => {
                    // 1. Add to our mosaic
                    used_nodes.insert(piece.node_id);
                    pieces.push(piece);

                    // 2. Subtract this vector from the residual
                    if residual.len() == vec.len() {
                        for i in 0..residual.len() {
                            residual[i] -= vec[i];
                        }
                    } else {
                        return Err(Error::other("Vector dimension mismatch during composition"));
                    }
                }
                _ => {
                    // No more valid candidates found
                    break;
                }
            }
        }

        Ok(MosaicResult {
            pieces,
            final_residual_magnitude: magnitude(&residual),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_mosaic_composition() {
        let db = AletheiaDB::new().unwrap();
        // Enable index so search works
        db.enable_vector_index("vec", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        // Let's create orthogonal basis vectors as our "concepts"
        // Concept X
        let px = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0, 0.0])
            .build();
        let nx = db.create_node("Concept", px).unwrap();

        // Concept Y
        let py = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0, 0.0])
            .build();
        let ny = db.create_node("Concept", py).unwrap();

        // Concept Z
        let pz = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0, 1.0])
            .build();
        let nz = db.create_node("Concept", pz).unwrap();

        let mosaic = Mosaic::new(&db);

        // Target: We want a concept that is a mix of X and Y, but no Z.
        // E.g., target = [1.0, 1.0, 0.0]
        let target = vec![1.0, 1.0, 0.0];

        let result = mosaic.compose(&target, "vec", 5, 0.01).unwrap();

        // It should pick exactly X and Y (in either order depending on float math / index ties)
        // and the residual should drop to near zero.
        assert_eq!(result.pieces.len(), 2);

        let found_ids: std::collections::HashSet<_> = result.pieces.iter().map(|p| p.node_id).collect();
        assert!(found_ids.contains(&nx));
        assert!(found_ids.contains(&ny));
        assert!(!found_ids.contains(&nz));

        // The final residual should be effectively 0
        assert!(result.final_residual_magnitude < 0.01);
    }
}