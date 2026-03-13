#![allow(clippy::collapsible_if)]
//! Nexus: Semantic Center of Mass Engine 🌌
//!
//! "What is the heart of this cluster?"
//!
//! Nexus computes the "semantic centroid" (the average vector) of a specific
//! group of nodes. This allows you to find the core concept or "zeitgeist"
//! that unites a set of disparate entities.
//!
//! # Concepts
//! - **Centroid**: The average vector of a set of nodes.
//! - **Divergence**: How far an individual node is from the cluster's centroid.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::nexus::Nexus;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let nodes = vec![];
//!
//! let nexus = Nexus::new(&db);
//! let centroid = nexus.calculate_centroid(&nodes, "embedding")?;
//!
//! println!("Centroid Vector Dimensions: {}", centroid.len());
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;

/// The Nexus Engine.
pub struct Nexus<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Nexus<'a> {
    /// Create a new Nexus Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the semantic centroid (average vector) of a given set of nodes.
    ///
    /// # Arguments
    /// * `nodes` - The list of node IDs to analyze.
    /// * `property_name` - The vector property to average.
    pub fn calculate_centroid(&self, nodes: &[NodeId], property_name: &str) -> Result<Vec<f32>> {
        if nodes.is_empty() {
            return Err(Error::other(
                "Cannot calculate centroid of an empty node list",
            ));
        }

        let mut vectors = Vec::new();

        self.db.read(|tx| {
            for &node_id in nodes {
                if let Ok(node) = tx.get_node(node_id) {
                    if let Some(prop) = node.get_property(property_name).and_then(|p| p.as_vector())
                    {
                        vectors.push(prop.to_vec());
                    }
                }
            }
            Ok::<(), Error>(())
        })?;

        if vectors.is_empty() {
            return Err(Error::other(
                "None of the provided nodes have the specified vector property",
            ));
        }

        let dim = vectors[0].len();
        let mut sum = vec![0.0; dim];

        for vec in &vectors {
            if vec.len() != dim {
                return Err(Error::other("Vector dimensions do not match"));
            }
            for i in 0..dim {
                sum[i] += vec[i];
            }
        }

        let count = vectors.len() as f32;
        for val in &mut sum {
            *val /= count;
        }

        // Normalize the centroid vector for reliable cosine similarity later
        ops::normalize_in_place(&mut sum);

        Ok(sum)
    }

    /// Calculate how far each node diverges from the group's centroid.
    /// Returns a list of (NodeId, Divergence Score) pairs.
    /// A lower score means the node is closer to the center (more prototypical).
    pub fn calculate_divergence(
        &self,
        nodes: &[NodeId],
        property_name: &str,
    ) -> Result<Vec<(NodeId, f32)>> {
        let centroid = self.calculate_centroid(nodes, property_name)?;
        let mut divergences = Vec::new();

        self.db.read(|tx| {
            for &node_id in nodes {
                if let Ok(node) = tx.get_node(node_id) {
                    if let Some(prop) = node.get_property(property_name).and_then(|p| p.as_vector())
                    {
                        let mut vec = prop.to_vec();
                        ops::normalize_in_place(&mut vec);
                        // Using cosine distance (1.0 - cosine_similarity)
                        let similarity = ops::cosine_similarity(&centroid, &vec)?;
                        let distance = (1.0_f32 - similarity).max(0.0);
                        divergences.push((node_id, distance));
                    }
                }
            }
            Ok::<(), Error>(())
        })?;

        // Sort by divergence (closest to centroid first)
        divergences.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(divergences)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_nexus_centroid_and_divergence() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();
        let mut n3 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 1.0, 0.0])
                        .build(),
                )
                .unwrap();

            n3 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.5, 0.5, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let nexus = Nexus::new(&db);
        let nodes = vec![n1, n2, n3];

        let centroid = nexus.calculate_centroid(&nodes, "embedding").unwrap();
        assert_eq!(centroid.len(), 3);
        // The centroid of [1,0,0], [0,1,0], and [0.5,0.5,0] is [0.5, 0.5, 0], which normalized is [0.707, 0.707, 0]
        assert!(centroid[0] > 0.0);
        assert!(centroid[1] > 0.0);
        assert_eq!(centroid[2], 0.0);

        let divergences = nexus.calculate_divergence(&nodes, "embedding").unwrap();
        assert_eq!(divergences.len(), 3);

        // n3 should be the most central (least divergent)
        assert_eq!(divergences[0].0, n3);
        assert!(divergences[0].1 < 0.01); // Divergence should be ~0.0
    }
}
