//! Interpolation: Semantic Midpoint Discovery.
//!
//! "What lies in the space between two concepts?"
//!
//! Semantic Interpolation calculates the theoretical midpoint between two nodes
//! in vector space and queries the database to find the existing concepts that
//! best bridge that gap.
//!
//! # Concepts
//! - **Interpolation**: The geometric midpoint of two vectors.
//! - **Bridging Concept**: An existing node that is located near the theoretical midpoint.

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;

/// Result of an interpolation query.
#[derive(Debug, Clone)]
pub struct InterpolationResult {
    /// The theoretical vector midpoint.
    pub theoretical_midpoint: Vec<f32>,
    /// Existing nodes that are nearest to this theoretical midpoint.
    pub bridging_concepts: Vec<(NodeId, f32)>,
}

/// The Semantic Interpolation Engine.
pub struct Interpolation<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-reasoning")]
impl<'a> Interpolation<'a> {
    /// Create a new Interpolation instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Interpolate between two nodes to find bridging concepts.
    ///
    /// # Arguments
    /// * `node_a` - The first node.
    /// * `node_b` - The second node.
    /// * `property` - The name of the vector property to use for calculation.
    /// * `limit` - Max number of bridging concepts to return.
    pub fn interpolate(
        &self,
        node_a: NodeId,
        node_b: NodeId,
        property: &str,
        limit: usize,
    ) -> Result<InterpolationResult> {
        let node_a_obj = self.db.get_node(node_a)?;
        let node_b_obj = self.db.get_node(node_b)?;

        let vec_a = node_a_obj
            .properties
            .get(property)
            .and_then(|v| v.as_vector())
            .ok_or_else(|| {
                Error::Vector(VectorError::IndexError(format!(
                    "Property '{}' not found or not a vector on node {}",
                    property, node_a
                )))
            })?;

        let vec_b = node_b_obj
            .properties
            .get(property)
            .and_then(|v| v.as_vector())
            .ok_or_else(|| {
                Error::Vector(VectorError::IndexError(format!(
                    "Property '{}' not found or not a vector on node {}",
                    property, node_b
                )))
            })?;

        if vec_a.len() != vec_b.len() {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: vec_a.len(),
                actual: vec_b.len(),
            }));
        }

        // Calculate the exact midpoint.
        let dim = vec_a.len();
        let mut midpoint = vec![0.0; dim];
        for i in 0..dim {
            midpoint[i] = (vec_a[i] + vec_b[i]) / 2.0;
        }

        // For cosine-based spaces, direction is what matters, so we normalize the midpoint.
        let theoretical_midpoint = crate::core::vector::ops::normalize(&midpoint);

        // Search for existing concepts near the theoretical midpoint.
        let mut neighbors =
            self.db
                .search_vectors_in(property, &theoretical_midpoint, limit + 2)?;

        // Filter out the source nodes from the results, so we only return true bridging concepts
        neighbors.retain(|(id, _)| *id != node_a && *id != node_b);
        neighbors.truncate(limit);

        Ok(InterpolationResult {
            theoretical_midpoint,
            bridging_concepts: neighbors,
        })
    }
}

#[cfg(not(feature = "semantic-reasoning"))]
impl<'a> Interpolation<'a> {
    /// Create a new Interpolation instance.
    pub fn new(_db: &'a AletheiaDB) -> Self {
        panic!(
            "Experimental feature 'Interpolation' requires the 'semantic-reasoning' feature (or 'nova'). Please enable it in Cargo.toml."
        );
    }

    /// Interpolate between two nodes to find bridging concepts.
    pub fn interpolate(
        &self,
        _node_a: NodeId,
        _node_b: NodeId,
        _property: &str,
        _limit: usize,
    ) -> Result<InterpolationResult> {
        panic!(
            "Experimental feature 'Interpolation' requires the 'semantic-reasoning' feature (or 'nova'). Please enable it in Cargo.toml."
        );
    }
}

#[cfg(all(test, feature = "semantic-reasoning"))]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_semantic_interpolation_bridging() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // Node A: [1.0, 0.0]
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        // Node B: [0.0, 1.0]
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        // Node C: Bridge [0.707, 0.707] (Normalized [0.5, 0.5])
        let props_c = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.707, 0.707])
            .build();
        let c = db.create_node("Node", props_c).unwrap();

        let engine = Interpolation::new(&db);
        let result = engine.interpolate(a, b, "vec", 5).unwrap();

        // Check Centroid/Theoretical Midpoint
        let midpoint = &result.theoretical_midpoint;
        assert!((midpoint[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01);
        assert!((midpoint[1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01);

        // Check Bridging Concepts
        // The nearest neighbor to the midpoint should be C.
        // A and B should be filtered out.
        assert_eq!(result.bridging_concepts.len(), 1);
        assert_eq!(result.bridging_concepts[0].0, c);
    }
}
