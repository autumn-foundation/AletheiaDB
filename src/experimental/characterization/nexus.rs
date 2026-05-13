//! Nexus: Semantic Centroid Discovery.
//!
//! "What is the center of the universe?"
//!
//! The Nexus engine calculates the semantic centroid of a set of nodes
//! and identifies the node that is closest to this center. This node acts as the "Nexus"
//! or the most representative concept of the entire dataset.
//!
//! # Concepts
//! - **Centroid**: The average vector of all nodes in the set.
//! - **Nexus Node**: The actual node in the graph that is closest to the Centroid.
//!
//! # Use Cases
//! - **Summarization**: What is the single most representative concept in this dataset?
//! - **Community Analysis**: Finding the core topic of a cluster of nodes.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::nexus::Nexus;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let nexus_engine = Nexus::new(&db);
//!
//! # let n1 = db.create_node("Concept", Default::default())?;
//! # let n2 = db.create_node("Concept", Default::default())?;
//! // Find the nexus among a set of nodes
//! let (nexus_node, score) = nexus_engine.find_nexus(&[n1, n2], "embedding")?;
//!
//! println!("The Nexus node is {} with similarity score {}", nexus_node, score);
//! # Ok(())
//! # }
//! ```

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

/// The Nexus Engine.
pub struct Nexus<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Nexus<'a> {
    /// Create a new Nexus instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Finds the Nexus (most representative node) for a given set of nodes.
    ///
    /// Calculates the centroid of all provided nodes' vectors, then returns the node
    /// that has the highest cosine similarity to that centroid, along with the similarity score.
    pub fn find_nexus(&self, nodes: &[NodeId], vector_property: &str) -> Result<(NodeId, f32)> {
        if nodes.is_empty() {
            return Err(Error::other("Cannot find nexus of an empty set"));
        }

        let mut centroid: Option<Vec<f32>> = None;
        let mut count = 0;

        for &node_id in nodes {
            let node = self.db.get_node(node_id)?;
            if let Some(prop) = node.get_property(vector_property) {
                if let Some(vec) = prop.as_vector() {
                    if let Some(c) = &mut centroid {
                        if c.len() != vec.len() {
                            return Err(Error::Vector(VectorError::DimensionMismatch {
                                expected: c.len(),
                                actual: vec.len(),
                            }));
                        }
                        for i in 0..c.len() {
                            c[i] += vec[i];
                        }
                    } else {
                        centroid = Some(vec.to_vec());
                    }
                    count += 1;
                }
            }
        }

        let mut centroid =
            centroid.ok_or_else(|| Error::other("No valid vectors found in the provided nodes"))?;

        for val in &mut centroid {
            *val /= count as f32;
        }

        let mut best_node = None;
        let mut best_sim = f32::NEG_INFINITY;

        for &node_id in nodes {
            let node = self.db.get_node(node_id)?;
            if let Some(prop) = node.get_property(vector_property) {
                if let Some(vec) = prop.as_vector() {
                    let sim = cosine_similarity(&centroid, vec)?;
                    if sim > best_sim {
                        best_sim = sim;
                        best_node = Some(node_id);
                    }
                }
            }
        }

        best_node.map(|n| (n, best_sim)).ok_or_else(|| {
            Error::other("Failed to calculate similarities (possibly zero magnitude vectors)")
        })
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;
    use crate::api::transaction::WriteOps;

    #[test]
    fn test_find_nexus() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(1).unwrap();
        let mut n2 = NodeId::new(2).unwrap();
        let mut n3 = NodeId::new(3).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "N",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "N",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            n3 = tx
                .create_node(
                    "N",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.8, 0.8]) // Closer to the centroid [0.6, 0.6]
                        .build(),
                )
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let nexus = Nexus::new(&db);
        let (node, score) = nexus.find_nexus(&[n1, n2, n3], "vec").unwrap();

        assert_eq!(node, n3);
        assert!(score > 0.9);
    }
}
