#![allow(clippy::collapsible_if)]

//! Doppelganger: Semantic Structural Equivalence 👥
//!
//! "Are these two nodes functionally identical in both meaning and context?"
//!
//! Doppelganger identifies nodes that not only share similar semantic embeddings
//! but also have similar structural contexts (neighbors with similar embeddings).
//!
//! # Concepts
//! - **Semantic Twin**: A node that shares a high cosine similarity with the target node's vector.
//! - **Structural Twin**: A node whose neighbors share high cosine similarities with the target node's neighbors.
//! - **Doppelganger Score**: A combined score of semantic and structural similarity.
//!
//! # Use Cases
//! - **Fraud Detection**: Finding new accounts that structurally and semantically resemble known bad actors.
//! - **Recommendation Systems**: Finding items that are functionally equivalent substitutes.
//! - **Entity Resolution**: Identifying duplicate records that have slight variations in edges.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::doppelganger::Doppelganger;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let target = NodeId::new(1)?;
//! # let candidates = vec![target];
//!
//! let doppelganger = Doppelganger::new(&db);
//! // Find twins among a list of candidates
//! let twins = doppelganger.find_twins(target, &candidates, "embedding", 0.8, 0.7)?;
//!
//! println!("Found {} potential doppelgangers!", twins.len());
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;

/// Result of a doppelganger analysis.
#[derive(Debug, Clone)]
pub struct TwinResult {
    /// The potential doppelganger node.
    pub node_id: NodeId,
    /// Semantic similarity to the target node's vector.
    pub semantic_similarity: f32,
    /// Structural similarity (how similar the neighborhood is).
    pub structural_similarity: f32,
    /// Combined score.
    pub doppelganger_score: f32,
}

/// The Doppelganger engine.
pub struct Doppelganger<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Doppelganger<'a> {
    /// Create a new Doppelganger engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Helper to get the average structural vector (neighbor embeddings).
    fn get_structural_vector(
        &self,
        tx: &crate::api::transaction::ReadTransaction,
        node_id: NodeId,
        property_name: &str,
    ) -> Result<Option<Vec<f32>>> {
        let mut neighbor_vecs = Vec::new();

        // Get outgoing
        let outgoing = tx.get_outgoing_edges(node_id);
        for edge_id in outgoing {
            if let Ok(edge) = tx.get_edge(edge_id) {
                if let Ok(neighbor) = tx.get_node(edge.target) {
                    if let Some(prop) = neighbor
                        .get_property(property_name)
                        .and_then(|p| p.as_vector())
                    {
                        neighbor_vecs.push(prop.to_vec());
                    }
                }
            }
        }

        // Get incoming
        let incoming = tx.get_incoming_edges(node_id);
        for edge_id in incoming {
            if let Ok(edge) = tx.get_edge(edge_id) {
                if let Ok(neighbor) = tx.get_node(edge.source) {
                    if let Some(prop) = neighbor
                        .get_property(property_name)
                        .and_then(|p| p.as_vector())
                    {
                        neighbor_vecs.push(prop.to_vec());
                    }
                }
            }
        }

        if neighbor_vecs.is_empty() {
            return Ok(None);
        }

        // Average the vectors
        let dim = neighbor_vecs[0].len();
        let mut sum = vec![0.0; dim];
        for vec in &neighbor_vecs {
            if vec.len() != dim {
                return Err(Error::other("Vector dimensions do not match"));
            }
            for i in 0..dim {
                sum[i] += vec[i];
            }
        }

        let count = neighbor_vecs.len() as f32;
        for val in &mut sum {
            *val /= count;
        }

        ops::normalize_in_place(&mut sum);

        Ok(Some(sum))
    }

    /// Find potential doppelgangers for a target node among a list of candidates.
    ///
    /// # Arguments
    /// * `target` - The node to find twins for.
    /// * `candidates` - Nodes to consider as potential doppelgangers.
    /// * `property_name` - The vector property to compare.
    /// * `semantic_threshold` - Minimum semantic similarity (0.0 to 1.0).
    /// * `structural_threshold` - Minimum structural similarity (0.0 to 1.0).
    pub fn find_twins(
        &self,
        target: NodeId,
        candidates: &[NodeId],
        property_name: &str,
        semantic_threshold: f32,
        structural_threshold: f32,
    ) -> Result<Vec<TwinResult>> {
        let mut results = Vec::new();

        self.db.read(|tx| {
            // 1. Get the target node's vector
            let target_node = match tx.get_node(target) {
                Ok(n) => n,
                Err(_) => return Err(Error::other("Target node not found")),
            };

            let mut target_vec = match target_node.get_property(property_name).and_then(|p| p.as_vector()) {
                Some(v) => v.to_vec(),
                None => return Err(Error::other("Target node does not have the specified vector property")),
            };

            ops::normalize_in_place(&mut target_vec);

            // 2. Gather the target's structural context (average of neighbor vectors)
            let target_structural_vec = match self.get_structural_vector(tx, target, property_name)? {
                Some(v) => v,
                None => return Err(Error::other("Target node has no structural neighbors with the specified vector property")),
            };

            // 3. Scan all candidate nodes
            for &candidate in candidates {
                if candidate == target {
                    continue; // Skip the target itself
                }

                if let Ok(cand_node) = tx.get_node(candidate) {
                    if let Some(prop) = cand_node.get_property(property_name).and_then(|p| p.as_vector()) {
                        let mut cand_vec = prop.to_vec();
                        ops::normalize_in_place(&mut cand_vec);

                        // Semantic similarity
                        let semantic_similarity = ops::cosine_similarity(&target_vec, &cand_vec)?;

                        if semantic_similarity >= semantic_threshold {
                            // Check structural similarity
                            if let Some(cand_structural_vec) = self.get_structural_vector(tx, candidate, property_name)? {
                                let structural_similarity = ops::cosine_similarity(&target_structural_vec, &cand_structural_vec)?;

                                if structural_similarity >= structural_threshold {
                                    let doppelganger_score = (semantic_similarity + structural_similarity) / 2.0;

                                    results.push(TwinResult {
                                        node_id: candidate,
                                        semantic_similarity,
                                        structural_similarity,
                                        doppelganger_score,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            Ok::<(), Error>(())
        })?;

        // Sort by score descending
        results.sort_by(|a, b| {
            b.doppelganger_score
                .partial_cmp(&a.doppelganger_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_doppelganger_find_twins() {
        let db = AletheiaDB::new().unwrap();

        let mut target = NodeId::new(0).unwrap();
        let mut target_neighbor = NodeId::new(0).unwrap();

        let mut twin = NodeId::new(0).unwrap();
        let mut twin_neighbor = NodeId::new(0).unwrap();

        let mut unrelated = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Target cluster
            target = tx
                .create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            target_neighbor = tx
                .create_node(
                    "Item",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(target, target_neighbor, "LIKES", Default::default())
                .unwrap();

            // Twin cluster (same vectors and structure)
            twin = tx
                .create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.99, 0.01]) // Very similar to target
                        .build(),
                )
                .unwrap();

            twin_neighbor = tx
                .create_node(
                    "Item",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.01, 0.99]) // Very similar to target_neighbor
                        .build(),
                )
                .unwrap();

            tx.create_edge(twin, twin_neighbor, "LIKES", Default::default())
                .unwrap();

            // Unrelated cluster (different vectors)
            unrelated = tx
                .create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[-1.0, 0.0])
                        .build(),
                )
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let doppelganger = Doppelganger::new(&db);
        let candidates = vec![target, target_neighbor, twin, twin_neighbor, unrelated];

        let twins = doppelganger
            .find_twins(target, &candidates, "embedding", 0.9, 0.9)
            .unwrap();

        assert_eq!(twins.len(), 1);
        assert_eq!(twins[0].node_id, twin);
        assert!(twins[0].semantic_similarity > 0.9);
        assert!(twins[0].structural_similarity > 0.9);
    }
}
