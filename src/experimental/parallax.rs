//! Parallax: Semantic Axis Projection.
//!
//! "Where do you stand between Order and Chaos?"
//!
//! Parallax allows you to define a semantic axis using two anchor nodes (e.g., 'Good' and 'Evil',
//! or 'Frontend' and 'Backend') and project other nodes onto this axis. This reveals where a node
//! lies on the spectrum between the two concepts.
//!
//! # Concepts
//! - **Anchors**: Two nodes that define the ends of the semantic axis.
//! - **Axis Vector**: The vector pointing from the first anchor (Anchor A) to the second anchor (Anchor B).
//! - **Projection**: The scalar value representing a node's position along the axis.
//!     - `0.0` means the node is exactly at Anchor A.
//!     - `1.0` means the node is exactly at Anchor B.
//!     - `< 0.0` means the node is "past" Anchor A.
//!     - `> 1.0` means the node is "past" Anchor B.
//!
//! # Use Cases
//! - **Spectrum Analysis**: Visualizing how different entities align on a specific topic (e.g., political spectrum).
//! - **Feature Extraction**: Converting high-dimensional embeddings into interpretable 1D features.
//! - **Comparative Ranking**: Sorting nodes based on their relative similarity to two opposing concepts.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::parallax::Parallax;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let anchor_a = db.create_node("Anchor", Default::default())?;
//! # let anchor_b = db.create_node("Anchor", Default::default())?;
//! # let target_node = db.create_node("Target", Default::default())?;
//! let parallax = Parallax::new(&db);
//!
//! // Project target_node onto the axis defined by anchor_a and anchor_b
//! let projections = parallax.project_nodes(anchor_a, anchor_b, &[target_node], "embedding")?;
//!
//! for proj in projections {
//!     println!("Node {} projection: {:.3}", proj.node_id, proj.score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::vector::ops::{dot_product, magnitude};

/// A node's projection onto a semantic axis.
#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    /// The node that was projected.
    pub node_id: NodeId,
    /// The projection score along the axis.
    /// `0.0` = Anchor A, `1.0` = Anchor B.
    pub score: f32,
    /// The orthogonal distance from the axis (how "far off" the spectrum it is).
    pub orthogonal_distance: f32,
}

/// The Parallax Engine.
pub struct Parallax<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Parallax<'a> {
    /// Create a new Parallax instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Project a set of target nodes onto the semantic axis defined by two anchor nodes.
    ///
    /// # Arguments
    ///
    /// * `anchor_a` - The starting node of the axis (score 0.0).
    /// * `anchor_b` - The ending node of the axis (score 1.0).
    /// * `targets` - A list of nodes to project onto the axis.
    /// * `property` - The vector property to use for projection (e.g., "embedding").
    ///
    /// # Returns
    ///
    /// A list of `Projection` structs for each target node that has the specified vector property.
    pub fn project_nodes(
        &self,
        anchor_a: NodeId,
        anchor_b: NodeId,
        targets: &[NodeId],
        property: &str,
    ) -> Result<Vec<Projection>> {
        let vec_a = self.get_node_vector(anchor_a, property)?;
        let vec_b = self.get_node_vector(anchor_b, property)?;

        if vec_a.len() != vec_b.len() {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: vec_a.len(),
                actual: vec_b.len(),
            }));
        }

        // Calculate the axis vector: V_axis = V_b - V_a
        let axis_vec: Vec<f32> = vec_b.iter().zip(vec_a.iter()).map(|(b, a)| b - a).collect();
        let axis_mag_sq = dot_product(&axis_vec, &axis_vec)?;

        if axis_mag_sq < f32::EPSILON {
            return Err(Error::Vector(VectorError::IndexError(
                "Anchors have identical vectors; cannot form an axis.".to_string(),
            )));
        }

        let _axis_mag = axis_mag_sq.sqrt();
        let mut projections = Vec::with_capacity(targets.len());

        for &target_id in targets {
            if let Ok(vec_t) = self.get_node_vector(target_id, property) {
                if vec_t.len() != vec_a.len() {
                    continue; // Skip dimension mismatches
                }

                // Vector from A to Target: V_t_rel = V_t - V_a
                let t_rel: Vec<f32> = vec_t.iter().zip(vec_a.iter()).map(|(t, a)| t - a).collect();

                // Projection score: (V_t_rel . V_axis) / (V_axis . V_axis)
                let dot_t_axis = dot_product(&t_rel, &axis_vec)?;
                let score = dot_t_axis / axis_mag_sq;

                // Orthogonal distance
                // Projected vector: P = V_a + score * V_axis
                // Distance = || V_t - P || = || V_t_rel - score * V_axis ||
                let mut orthog_vec = Vec::with_capacity(t_rel.len());
                for (t_r, a_v) in t_rel.iter().zip(axis_vec.iter()) {
                    orthog_vec.push(t_r - score * a_v);
                }
                let orthogonal_distance = magnitude(&orthog_vec);

                projections.push(Projection {
                    node_id: target_id,
                    score,
                    orthogonal_distance,
                });
            }
        }

        Ok(projections)
    }

    /// Internal helper to retrieve a node's vector.
    fn get_node_vector(&self, node_id: NodeId, property: &str) -> Result<Vec<f32>> {
        let node = self.db.get_node(node_id)?;
        let val = node.properties.get(property).ok_or_else(|| {
            Error::Vector(VectorError::IndexError(format!(
                "Property '{}' missing on node {}",
                property, node_id
            )))
        })?;
        let vec = val.as_vector().ok_or_else(|| {
            Error::Vector(VectorError::IndexError(format!(
                "Property '{}' on node {} is not a vector",
                property, node_id
            )))
        })?;
        Ok(vec.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_parallax_projection() {
        let db = AletheiaDB::new().unwrap();

        // Anchor A at [0.0, 0.0]
        let anchor_a = db
            .create_node(
                "A",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Anchor B at [10.0, 0.0]
        let anchor_b = db
            .create_node(
                "B",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[10.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Target exactly halfway [5.0, 0.0]
        let t_half = db
            .create_node(
                "THalf",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[5.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Target past B [15.0, 0.0]
        let t_past = db
            .create_node(
                "TPast",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[15.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Target off-axis [5.0, 5.0]
        let t_off = db
            .create_node(
                "TOff",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[5.0, 5.0])
                    .build(),
            )
            .unwrap();

        let parallax = Parallax::new(&db);
        let projections = parallax
            .project_nodes(anchor_a, anchor_b, &[t_half, t_past, t_off], "vec")
            .unwrap();

        assert_eq!(projections.len(), 3);

        // Check THalf
        let proj_half = projections.iter().find(|p| p.node_id == t_half).unwrap();
        assert!((proj_half.score - 0.5).abs() < 1e-5);
        assert!((proj_half.orthogonal_distance - 0.0).abs() < 1e-5);

        // Check TPast
        let proj_past = projections.iter().find(|p| p.node_id == t_past).unwrap();
        assert!((proj_past.score - 1.5).abs() < 1e-5);
        assert!((proj_past.orthogonal_distance - 0.0).abs() < 1e-5);

        // Check TOff
        let proj_off = projections.iter().find(|p| p.node_id == t_off).unwrap();
        assert!((proj_off.score - 0.5).abs() < 1e-5); // Still halfway along X
        assert!((proj_off.orthogonal_distance - 5.0).abs() < 1e-5); // 5 units off axis
    }

    #[test]
    fn test_parallax_identical_anchors() {
        let db = AletheiaDB::new().unwrap();

        let anchor = db
            .create_node(
                "A",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 1.0])
                    .build(),
            )
            .unwrap();

        let parallax = Parallax::new(&db);
        let res = parallax.project_nodes(anchor, anchor, &[], "vec");

        assert!(res.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = res {
            assert!(msg.contains("identical"));
        } else {
            panic!("Expected VectorError::IndexError");
        }
    }
}
