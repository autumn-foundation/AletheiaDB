//! Echolocation: Semantic Boundary Detection.
//!
//! "Where does one concept end and another begin?"
//!
//! Echolocation detects "semantic boundaries" in a graph. A boundary node is one where
//! the semantic profile of its incoming neighborhood diverges significantly from the
//! semantic profile of its outgoing neighborhood.
//!
//! # Concepts
//! - **Incoming Centroid**: The average vector of all nodes that point to this node.
//! - **Outgoing Centroid**: The average vector of all nodes that this node points to.
//! - **Divergence Score**: The cosine distance between the incoming and outgoing centroids.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::echolocation::Echolocation;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let echo = Echolocation::new(&db);
//!
//! let boundaries = echo.find_boundaries("embedding", 0.5)?;
//! for (node_id, score) in boundaries {
//!     println!("Found semantic boundary at node {} with divergence score {:.2}", node_id, score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;

/// The Echolocation Engine for detecting semantic boundaries.
pub struct Echolocation<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Echolocation<'a> {
    /// Create a new Echolocation instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find nodes acting as semantic boundaries.
    ///
    /// # Arguments
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `threshold` - Minimum cosine distance between incoming and outgoing centroids to be considered a boundary (0.0 to 2.0).
    pub fn find_boundaries(
        &self,
        property_name: &str,
        threshold: f32,
    ) -> Result<Vec<(NodeId, f32)>> {
        let mut boundaries = Vec::new();

        let results = self.db.query().scan(None).execute(self.db)?;
        for row in results {
            let row = row?;
            #[allow(clippy::collapsible_if)]
            if let Some(node) = row.entity.as_node() {
                if let Ok(Some(divergence)) = self.calculate_divergence(node.id, property_name) {
                    if divergence >= threshold {
                        boundaries.push((node.id, divergence));
                    }
                }
            }
        }

        // Sort descending by divergence score
        boundaries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(boundaries)
    }

    fn calculate_divergence(&self, node_id: NodeId, property_name: &str) -> Result<Option<f32>> {
        // Use read closure for consistent snapshot
        let (in_vecs, out_vecs) = self.db.read(|tx| {
            let mut incoming_vecs = Vec::new();
            for edge_id in tx.get_incoming_edges(node_id) {
                #[allow(clippy::collapsible_if)]
                if let Ok(edge) = tx.get_edge(edge_id) {
                    if let Ok(source_node) = tx.get_node(edge.source) {
                        if let Some(prop) = source_node.get_property(property_name) {
                            if let Some(vec) = prop.as_vector() {
                                incoming_vecs.push(vec.to_vec());
                            }
                        }
                    }
                }
            }

            let mut outgoing_vecs = Vec::new();
            for edge_id in tx.get_outgoing_edges(node_id) {
                #[allow(clippy::collapsible_if)]
                if let Ok(edge) = tx.get_edge(edge_id) {
                    if let Ok(target_node) = tx.get_node(edge.target) {
                        if let Some(prop) = target_node.get_property(property_name) {
                            if let Some(vec) = prop.as_vector() {
                                outgoing_vecs.push(vec.to_vec());
                            }
                        }
                    }
                }
            }

            Ok::<(Vec<Vec<f32>>, Vec<Vec<f32>>), Error>((incoming_vecs, outgoing_vecs))
        })?;

        if in_vecs.is_empty() || out_vecs.is_empty() {
            return Ok(None);
        }

        let in_centroid = Self::average_vectors(&in_vecs)?;
        let out_centroid = Self::average_vectors(&out_vecs)?;

        let similarity = crate::core::vector::ops::cosine_similarity(&in_centroid, &out_centroid)?;
        let distance = 1.0 - similarity;

        Ok(Some(distance.max(0.0)))
    }

    fn average_vectors(vectors: &[Vec<f32>]) -> Result<Vec<f32>> {
        if vectors.is_empty() {
            return Err(Error::other("Cannot average empty vector list"));
        }

        let dim = vectors[0].len();
        let mut sum = vec![0.0; dim];

        for vec in vectors {
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

        Ok(sum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_find_boundaries() {
        let db = AletheiaDB::new().unwrap();

        // Create boundary node
        let boundary_props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 0.0])
            .build();
        let mut boundary = NodeId::new(0).unwrap();

        // Create incoming cluster nodes (all around [1.0, 0.0])
        let in_props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let mut in_n1 = NodeId::new(0).unwrap();
        let mut in_n2 = NodeId::new(0).unwrap();

        // Create outgoing cluster nodes (all around [0.0, 1.0])
        let out_props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 1.0])
            .build();
        let mut out_n1 = NodeId::new(0).unwrap();
        let mut out_n2 = NodeId::new(0).unwrap();

        db.write(|tx| {
            boundary = tx.create_node("Node", boundary_props).unwrap();
            in_n1 = tx.create_node("Node", in_props.clone()).unwrap();
            in_n2 = tx.create_node("Node", in_props).unwrap();
            out_n1 = tx.create_node("Node", out_props.clone()).unwrap();
            out_n2 = tx.create_node("Node", out_props).unwrap();

            // Connect incoming to boundary
            tx.create_edge(in_n1, boundary, "LINK", Default::default())
                .unwrap();
            tx.create_edge(in_n2, boundary, "LINK", Default::default())
                .unwrap();

            // Connect boundary to outgoing
            tx.create_edge(boundary, out_n1, "LINK", Default::default())
                .unwrap();
            tx.create_edge(boundary, out_n2, "LINK", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let echo = Echolocation::new(&db);
        // The cosine similarity between [1,0] and [0,1] is 0.0, so distance is 1.0.
        // With a threshold of 0.5, this should easily trigger.
        let boundaries = echo.find_boundaries("embedding", 0.5).unwrap();

        assert_eq!(boundaries.len(), 1);
        assert_eq!(boundaries[0].0, boundary);
        assert!((boundaries[0].1 - 1.0).abs() < 0.01);
    }
}
