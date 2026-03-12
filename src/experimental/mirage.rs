#![allow(clippy::collapsible_if)]
//! Mirage: Contextual Mismatch Detector 🏜️
//!
//! "Things are not always as they seem."
//!
//! Mirage detects nodes that are semantically similar on the surface (their own vectors match),
//! but exist in completely different structural contexts (their neighbors' vectors do not match).
//! This is useful for finding homonyms, fake accounts, or out-of-context concepts.
//!
//! # Concepts
//! - **Surface Similarity**: The cosine similarity between two nodes' own vectors.
//! - **Context Similarity**: The cosine similarity between the average vectors of their neighbors.
//! - **Mirage Score**: High surface similarity but low contextual similarity.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::mirage::MirageDetector;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let n1 = NodeId::new(1)?;
//! # let n2 = NodeId::new(2)?;
//!
//! let mirage = MirageDetector::new(&db);
//! // let score = mirage.detect(n1, n2, "embedding")?;
//! // println!("Mirage Score: {}", score.mirage_score);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;

/// The result of a Mirage analysis.
#[derive(Debug, Clone)]
pub struct MirageScore {
    /// The similarity between the two nodes directly.
    pub surface_similarity: f32,
    /// The similarity between the neighborhoods of the two nodes.
    pub context_similarity: f32,
    /// The degree to which it is a mirage (surface - context).
    pub mirage_score: f32,
}

/// The Mirage Detector.
pub struct MirageDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> MirageDetector<'a> {
    /// Create a new Mirage Detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if the relationship between two nodes is a mirage.
    pub fn detect(
        &self,
        node_a: NodeId,
        node_b: NodeId,
        property_name: &str,
    ) -> Result<MirageScore> {
        let (surface_similarity, context_similarity) = self.db.read(|tx| {
            let vec_a = tx
                .get_node(node_a)?
                .get_property(property_name)
                .and_then(|p| p.as_vector())
                .ok_or_else(|| Error::other("Node A missing vector property"))?
                .to_vec();

            let vec_b = tx
                .get_node(node_b)?
                .get_property(property_name)
                .and_then(|p| p.as_vector())
                .ok_or_else(|| Error::other("Node B missing vector property"))?
                .to_vec();

            let surface_sim = ops::cosine_similarity(&vec_a, &vec_b)?;

            let context_a =
                self.get_neighborhood_centroid(tx, node_a, property_name, vec_a.len())?;
            let context_b =
                self.get_neighborhood_centroid(tx, node_b, property_name, vec_b.len())?;

            let context_sim = if context_a.is_empty() || context_b.is_empty() {
                0.0 // If either has no context, we consider context similarity 0
            } else {
                ops::cosine_similarity(&context_a, &context_b)?
            };

            Ok::<_, Error>((surface_sim, context_sim))
        })?;

        let mirage_score = (surface_similarity - context_similarity).max(0.0);

        Ok(MirageScore {
            surface_similarity,
            context_similarity,
            mirage_score,
        })
    }

    fn get_neighborhood_centroid<T: ReadOps>(
        &self,
        tx: &T,
        node_id: NodeId,
        property_name: &str,
        dim: usize,
    ) -> Result<Vec<f32>> {
        let mut sum = vec![0.0; dim];
        let mut count = 0;

        let outgoing = tx.get_outgoing_edges(node_id);
        for edge_id in outgoing {
            if let Ok(edge) = tx.get_edge(edge_id) {
                if let Ok(target) = tx.get_node(edge.target) {
                    if let Some(prop) = target.get_property(property_name) {
                        if let Some(vec) = prop.as_vector() {
                            if vec.len() == dim {
                                for i in 0..dim {
                                    sum[i] += vec[i];
                                }
                                count += 1;
                            }
                        }
                    }
                }
            }
        }

        let incoming = tx.get_incoming_edges(node_id);
        for edge_id in incoming {
            if let Ok(edge) = tx.get_edge(edge_id) {
                if let Ok(source) = tx.get_node(edge.source) {
                    if let Some(prop) = source.get_property(property_name) {
                        if let Some(vec) = prop.as_vector() {
                            if vec.len() == dim {
                                for i in 0..dim {
                                    sum[i] += vec[i];
                                }
                                count += 1;
                            }
                        }
                    }
                }
            }
        }

        if count == 0 {
            return Ok(Vec::new());
        }

        for val in &mut sum {
            *val /= count as f32;
        }

        Ok(sum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_mirage_detection() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();
        let mut ctx1 = NodeId::new(0).unwrap();
        let mut ctx2 = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Two nodes that are exactly the same on the surface
            n1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            n2 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            // But their contexts are orthogonal
            ctx1 = tx
                .create_node(
                    "Context1",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();
            ctx2 = tx
                .create_node(
                    "Context2",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, -1.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(n1, ctx1, "IN_CONTEXT", Default::default())
                .unwrap();
            tx.create_edge(n2, ctx2, "IN_CONTEXT", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let mirage = MirageDetector::new(&db);
        let score = mirage.detect(n1, n2, "vec").unwrap();

        assert!(score.surface_similarity > 0.99);
        assert!(score.context_similarity < 0.01);
        assert!(score.mirage_score > 0.98);
    }
}
