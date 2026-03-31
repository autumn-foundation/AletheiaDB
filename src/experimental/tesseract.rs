#![allow(clippy::collapsible_if)]

//! Tesseract: Semantic Axis Projection. 🧊
//!
//! "Measure your graph along human-understandable dimensions."
//!
//! Tesseract allows you to define a "Semantic Axis" using two pole nodes
//! (e.g., "Good" and "Evil", or "Cheap" and "Expensive") and project other
//! nodes onto this axis. This turns opaque high-dimensional vectors into
//! a 1D measurable spectrum.
//!
//! # Concepts
//! - **Pole Nodes**: Two nodes that define the extremes of the axis.
//! - **Semantic Axis**: The vector difference between the two poles.
//! - **Projection Score**: A scalar value indicating where a node falls on the axis.
//!   Typically, 0.0 means close to Pole A, 1.0 means close to Pole B.
//!
//! # Use Cases
//! - **Automated Sorting**: Sort products from "Budget" to "Luxury".
//! - **Bias Detection**: Measure how strongly concepts align with "Male" vs "Female".
//! - **Interpretable AI**: Understand what a vector means by seeing its coordinates on known axes.
//!
//! # Example
//! ```rust,no_run
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::tesseract::Tesseract;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let pole_a = NodeId::new(1).unwrap();
//! # let pole_b = NodeId::new(2).unwrap();
//! # let target_nodes = vec![];
//!
//! let tesseract = Tesseract::new(&db);
//! // Measure nodes on the axis from Pole A to Pole B
//! let projections = tesseract.project_nodes(pole_a, pole_b, &target_nodes, "embedding")?;
//!
//! for proj in projections {
//!     println!("Node {}: Score {}", proj.node_id, proj.score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;

/// Result of a node projection onto a semantic axis.
#[derive(Debug, Clone)]
pub struct ProjectionResult {
    /// The node that was projected.
    pub node_id: NodeId,
    /// The scalar projection score.
    /// Typically, values near 0.0 are close to Pole A, and 1.0 close to Pole B.
    /// Values can fall outside [0, 1] if the node is "further" along the axis than the poles.
    pub score: f32,
}

/// The Tesseract Engine for semantic axis projection.
pub struct Tesseract<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Tesseract<'a> {
    /// Create a new Tesseract instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Project a list of nodes onto the semantic axis defined by two pole nodes.
    ///
    /// # Arguments
    /// * `pole_a` - The starting node of the axis (Score ~ 0.0).
    /// * `pole_b` - The ending node of the axis (Score ~ 1.0).
    /// * `target_nodes` - The nodes to measure along this axis.
    /// * `property_name` - The vector property to use for the analysis.
    pub fn project_nodes(
        &self,
        pole_a: NodeId,
        pole_b: NodeId,
        target_nodes: &[NodeId],
        property_name: &str,
    ) -> Result<Vec<ProjectionResult>> {
        self.db.read(|tx| {
            // Retrieve vectors for pole_a and pole_b
            let pole_a_node = tx.get_node(pole_a)?;
            let pole_a_vec = pole_a_node
                .get_property(property_name)
                .and_then(|p| p.as_vector())
                .ok_or_else(|| {
                    Error::other("Pole A does not have the specified vector property")
                })?;

            let pole_b_node = tx.get_node(pole_b)?;
            let pole_b_vec = pole_b_node
                .get_property(property_name)
                .and_then(|p| p.as_vector())
                .ok_or_else(|| {
                    Error::other("Pole B does not have the specified vector property")
                })?;

            if pole_a_vec.len() != pole_b_vec.len() {
                return Err(Error::other(
                    "Pole A and Pole B vectors must have the same dimension",
                ));
            }

            let dim = pole_a_vec.len();

            // Calculate axis vector: pole_b - pole_a
            let mut axis_vec = vec![0.0; dim];
            let mut sq_mag_axis = 0.0;
            for i in 0..dim {
                axis_vec[i] = pole_b_vec[i] - pole_a_vec[i];
                sq_mag_axis += axis_vec[i] * axis_vec[i];
            }

            if sq_mag_axis == 0.0 {
                return Err(Error::other(
                    "Pole A and Pole B vectors are identical; cannot define an axis",
                ));
            }

            let mut results = Vec::new();

            for &target in target_nodes {
                // Ignore missing nodes or missing properties on target
                if let Ok(target_node) = tx.get_node(target) {
                    if let Some(target_vec) = target_node
                        .get_property(property_name)
                        .and_then(|p| p.as_vector())
                    {
                        if target_vec.len() == dim {
                            // Calculate dot product of (target - pole_a) and axis_vec
                            let mut dot_product = 0.0;
                            for i in 0..dim {
                                let diff = target_vec[i] - pole_a_vec[i];
                                dot_product += diff * axis_vec[i];
                            }
                            let score = dot_product / sq_mag_axis;
                            results.push(ProjectionResult {
                                node_id: target,
                                score,
                            });
                        }
                    }
                }
            }

            Ok::<_, Error>(results)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_tesseract_projection() {
        let db = AletheiaDB::new().unwrap();

        let mut pole_a = NodeId::new(0).unwrap();
        let mut pole_b = NodeId::new(0).unwrap();
        let mut target_1 = NodeId::new(0).unwrap();
        let mut target_2 = NodeId::new(0).unwrap();
        let mut target_3 = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Pole A is at (0, 0)
            pole_a = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Pole B is at (10, 0)
            pole_b = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[10.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Target 1 is at (5, 5) - Halfway along the axis, but offset
            target_1 = tx
                .create_node(
                    "Target",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[5.0, 5.0])
                        .build(),
                )
                .unwrap();

            // Target 2 is at (-2, 0) - Behind Pole A
            target_2 = tx
                .create_node(
                    "Target",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[-2.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Target 3 is at (12, -5) - Past Pole B, offset
            target_3 = tx
                .create_node(
                    "Target",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[12.0, -5.0])
                        .build(),
                )
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let tesseract = Tesseract::new(&db);
        let targets = vec![pole_a, pole_b, target_1, target_2, target_3];
        let results = tesseract
            .project_nodes(pole_a, pole_b, &targets, "embedding")
            .unwrap();

        assert_eq!(results.len(), 5);

        // Score for Pole A should be ~0.0
        let res_a = results.iter().find(|r| r.node_id == pole_a).unwrap();
        assert!((res_a.score - 0.0).abs() < 0.001);

        // Score for Pole B should be ~1.0
        let res_b = results.iter().find(|r| r.node_id == pole_b).unwrap();
        assert!((res_b.score - 1.0).abs() < 0.001);

        // Score for Target 1 (5, 5) projected onto (10, 0) should be ~0.5
        let res_t1 = results.iter().find(|r| r.node_id == target_1).unwrap();
        assert!((res_t1.score - 0.5).abs() < 0.001);

        // Score for Target 2 (-2, 0) should be -0.2
        let res_t2 = results.iter().find(|r| r.node_id == target_2).unwrap();
        assert!((res_t2.score - -0.2).abs() < 0.001);

        // Score for Target 3 (12, -5) should be 1.2
        let res_t3 = results.iter().find(|r| r.node_id == target_3).unwrap();
        assert!((res_t3.score - 1.2).abs() < 0.001);
    }
}
