//! Reverb: Semantic Echo Chamber Detector.
//!
//! "Are a node's neighbors simply repeating its own semantic vector back to it?"
//!
//! The Reverb engine calculates the "Echo Chamber Score" for a node by measuring
//! the average cosine similarity between a node and its incoming/outgoing neighbors.
//! A high score (> 0.9) indicates the node is surrounded by identical concepts,
//! while a low score indicates diverse or diverging neighbors.
//!
//! # Example
//!
//! ```rust
//! // [dependencies]
//! // aletheiadb = { version = "0.1", features = ["nova"] }
//!
//! # #[cfg(feature = "nova")]
//! # {
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::reverb::ReverbEngine;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("User", Default::default())?;
//!
//! let reverb = ReverbEngine::new(&db);
//! let score = reverb.analyze(node_id, "embedding")?;
//!
//! if score > 0.9 {
//!     println!("🔊 Node {} is in an echo chamber! (score: {:.2})", node_id, score);
//! }
//! # Ok(())
//! # }
//! # }
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::cosine_similarity;

/// ReverbEngine analyzes a node to determine its Echo Chamber Score.
pub struct ReverbEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> ReverbEngine<'a> {
    /// Create a new ReverbEngine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the Echo Chamber Score for a given node.
    ///
    /// The score is the average cosine similarity between the node's vector
    /// and the vectors of its direct neighbors (both incoming and outgoing).
    /// Returns 0.0 if the node has no neighbors with the specified property.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic comparison.
    pub fn analyze(&self, node_id: NodeId, property_name: &str) -> Result<f32> {
        let node_vector = self.get_node_vector(node_id, property_name)?;

        let mut similarity_sum = 0.0;
        let mut valid_neighbors = 0;

        // Iterate over outgoing neighbors
        let outgoing = self.db.get_outgoing_edges(node_id);
        for edge_id in outgoing {
            #[allow(clippy::collapsible_if)]
            if let Ok(edge) = self.db.get_edge(edge_id) {
                if let Ok(neighbor_vec) = self.get_node_vector(edge.target, property_name) {
                    if let Ok(sim) = cosine_similarity(&node_vector, &neighbor_vec) {
                        similarity_sum += sim;
                        valid_neighbors += 1;
                    }
                }
            }
        }

        // Iterate over incoming neighbors
        let incoming = self.db.get_incoming_edges(node_id);
        for edge_id in incoming {
            // Don't double count if there's a bidirectional edge
            // For simplicity, we just count all edges. In a strict echo chamber,
            // bidirectional confirmation is even stronger.
            #[allow(clippy::collapsible_if)]
            if let Ok(edge) = self.db.get_edge(edge_id) {
                if let Ok(neighbor_vec) = self.get_node_vector(edge.source, property_name) {
                    if let Ok(sim) = cosine_similarity(&node_vector, &neighbor_vec) {
                        similarity_sum += sim;
                        valid_neighbors += 1;
                    }
                }
            }
        }

        if valid_neighbors == 0 {
            return Ok(0.0);
        }

        Ok(similarity_sum / valid_neighbors as f32)
    }

    /// Helper to get a node's vector property safely.
    fn get_node_vector(&self, node_id: NodeId, property_name: &str) -> Result<Vec<f32>> {
        let node = self.db.get_node(node_id)?;
        let val = node
            .properties
            .get(property_name)
            .ok_or_else(|| Error::other(format!("Property '{}' missing", property_name)))?;
        let vec = val
            .as_vector()
            .ok_or_else(|| Error::other(format!("Property '{}' not a vector", property_name)))?;
        Ok(vec.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_reverb_echo_chamber() {
        let db = AletheiaDB::new().unwrap();

        // Node with an opinion [1.0, 1.0]
        let mut n1 = NodeId::new(0).unwrap();
        // Neighbors with same opinion
        let mut n2 = NodeId::new(0).unwrap();
        let mut n3 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert_vector("opinion", &[1.0, 1.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert_vector("opinion", &[0.99, 1.0])
                        .build(),
                )
                .unwrap();

            n3 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert_vector("opinion", &[1.0, 0.99])
                        .build(),
                )
                .unwrap();

            // Create echo chamber
            tx.create_edge(n1, n2, "FOLLOWS", Default::default())
                .unwrap();
            tx.create_edge(n3, n1, "FOLLOWS", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let reverb = ReverbEngine::new(&db);
        let score = reverb.analyze(n1, "opinion").unwrap();

        // Should be very close to 1.0
        assert!(score > 0.99);
    }

    #[test]
    fn test_reverb_diverse_neighbors() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        // Neighbors with orthogonal opinions
        let mut n2 = NodeId::new(0).unwrap();
        let mut n3 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert_vector("opinion", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Orthogonal
            n2 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert_vector("opinion", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            // Opposite
            n3 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert_vector("opinion", &[-1.0, 0.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(n1, n2, "KNOWS", Default::default()).unwrap();
            tx.create_edge(n3, n1, "KNOWS", Default::default()).unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let reverb = ReverbEngine::new(&db);
        let score = reverb.analyze(n1, "opinion").unwrap();

        // One neighbor has sim 0.0, the other has sim -1.0
        // Average should be -0.5
        assert!((score - (-0.5)).abs() < 0.01);
    }

    #[test]
    fn test_reverb_no_neighbors() {
        let db = AletheiaDB::new().unwrap();
        let mut n1 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert_vector("opinion", &[1.0, 1.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let reverb = ReverbEngine::new(&db);
        let score = reverb.analyze(n1, "opinion").unwrap();

        // No neighbors should give 0.0
        assert_eq!(score, 0.0);
    }
}
