//! Polaris: Semantic Compass & Dimensionality Reduction.
//!
//! "Find your True North."
//!
//! Polaris enables "Semantic Compassing". Given a high-dimensional vector space,
//! it's often hard to understand where a node lies. Polaris lets you define an `Axis`
//! using two concepts (e.g., "Good" vs "Bad", or "Fast" vs "Slow") and scores
//! nodes along this axis.
//!
//! This is effectively a 1D projection (dimensionality reduction) of semantic
//! space onto an interpretable human concept.
//!
//! # Concepts
//! - **Axis**: Defined by two pole vectors: `Positive` (+1.0) and `Negative` (-1.0).
//! - **Score**: Where a node falls on the axis. Close to +1.0 means it aligns with
//!   the positive pole; close to -1.0 means it aligns with the negative pole.
//!   0.0 means it is orthogonal or equidistant.
//!
//! # Use Cases
//! - **Interpretable Sliders**: Sorting products from "Traditional" to "Innovative".
//! - **Bias Detection**: Measuring how strongly a set of concepts leans toward a specific pole.
//! - **Semantic Filtering**: "Only show me nodes that score > 0.5 on the 'Safety' axis."
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::polaris::{Polaris, Axis};
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let polaris = Polaris::new(&db);
//!
//! // Define an axis: e.g. "Good" vs "Bad"
//! let axis = Axis::new(vec![1.0, 1.0, 1.0], vec![-1.0, -1.0, -1.0]);
//!
//! # let my_node = NodeId::new(1).unwrap();
//! // See where my_node falls on this axis
//! if let Some(score) = polaris.analyze_node(my_node, "embedding", &axis)? {
//!     println!("Node score on axis: {:.3}", score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::dot_product;

/// A semantic axis defined by two opposing poles.
#[derive(Debug, Clone)]
pub struct Axis {
    /// The positive pole (+1 direction).
    pub positive_pole: Vec<f32>,
    /// The negative pole (-1 direction).
    pub negative_pole: Vec<f32>,
    /// The normalized direction vector from negative to positive pole.
    axis_vector: Vec<f32>,
}

impl Axis {
    /// Creates a new `Axis` from two pole vectors.
    ///
    /// # Panics
    /// Panics if the vectors have different dimensions or are empty.
    pub fn new(positive_pole: Vec<f32>, negative_pole: Vec<f32>) -> Self {
        assert!(!positive_pole.is_empty(), "Poles cannot be empty");
        assert_eq!(
            positive_pole.len(),
            negative_pole.len(),
            "Poles must have the same dimension"
        );

        // Axis vector D = Positive - Negative
        let mut axis_vector = Vec::with_capacity(positive_pole.len());
        let mut mag_sq = 0.0;

        for (p, n) in positive_pole.iter().zip(negative_pole.iter()) {
            let diff = p - n;
            axis_vector.push(diff);
            mag_sq += diff * diff;
        }

        // Normalize the axis vector
        let mag = mag_sq.sqrt();
        if mag > 1e-9 {
            for v in &mut axis_vector {
                *v /= mag;
            }
        }

        Self {
            positive_pole,
            negative_pole,
            axis_vector,
        }
    }
}

/// The Polaris Semantic Compass Engine.
pub struct Polaris<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Polaris<'a> {
    /// Creates a new `Polaris` instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyzes a specific vector's position on the given `Axis`.
    ///
    /// The score is computed as the cosine similarity between the vector's direction
    /// relative to the midpoint of the poles, and the axis vector.
    /// Returns a value typically in `[-1.0, 1.0]`.
    pub fn analyze_vector(&self, vector: &[f32], axis: &Axis) -> Result<f32> {
        // Find the midpoint of the axis M = (P + N) / 2
        let mut midpoint = Vec::with_capacity(axis.positive_pole.len());
        for (p, n) in axis.positive_pole.iter().zip(axis.negative_pole.iter()) {
            midpoint.push((p + n) / 2.0);
        }

        // Vector relative to midpoint: V' = V - M
        let mut relative_vec = Vec::with_capacity(vector.len());
        let mut mag_sq = 0.0;
        for (v, m) in vector.iter().zip(midpoint.iter()) {
            let diff = v - m;
            relative_vec.push(diff);
            mag_sq += diff * diff;
        }

        if mag_sq < 1e-9 {
            // Vector is exactly at the midpoint
            return Ok(0.0);
        }

        // We use cosine similarity between the relative vector and the normalized axis vector.
        // Since axis_vector is already normalized, we can just do dot_product / ||V'||.
        let mag = mag_sq.sqrt();
        let dot = dot_product(&relative_vec, &axis.axis_vector)?;

        Ok(dot / mag)
    }

    /// Convenience method to analyze a node's vector property on the given `Axis`.
    ///
    /// Returns `Ok(None)` if the node does not exist, lacks the property, or the
    /// property is not a vector.
    pub fn analyze_node(
        &self,
        node_id: NodeId,
        property_name: &str,
        axis: &Axis,
    ) -> Result<Option<f32>> {
        let node = match self.db.get_node(node_id) {
            Ok(n) => n,
            Err(_) => return Ok(None),
        };

        if let Some(vec) = node.get_property(property_name).and_then(|p| p.as_vector()) {
            return Ok(Some(self.analyze_vector(vec, axis)?));
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_axis_creation() {
        let axis = Axis::new(vec![1.0, 0.0], vec![-1.0, 0.0]);
        assert_eq!(axis.axis_vector, vec![1.0, 0.0]); // Vector from [-1, 0] to [1, 0] is [2, 0], normalized to [1, 0]
    }

    #[test]
    fn test_analyze_vector_poles() {
        let db = AletheiaDB::new().unwrap();
        let polaris = Polaris::new(&db);
        let axis = Axis::new(vec![1.0, 0.0], vec![-1.0, 0.0]);

        // Positive pole should score 1.0
        let pos_score = polaris.analyze_vector(&[1.0, 0.0], &axis).unwrap();
        assert!((pos_score - 1.0).abs() < 1e-6);

        // Negative pole should score -1.0
        let neg_score = polaris.analyze_vector(&[-1.0, 0.0], &axis).unwrap();
        assert!((neg_score + 1.0).abs() < 1e-6);

        // Midpoint should score 0.0
        let mid_score = polaris.analyze_vector(&[0.0, 0.0], &axis).unwrap();
        assert!(mid_score.abs() < 1e-6);

        // Orthogonal point should also score 0.0
        let orth_score = polaris.analyze_vector(&[0.0, 1.0], &axis).unwrap();
        assert!(orth_score.abs() < 1e-6);
    }

    #[test]
    fn test_analyze_node() {
        let db = AletheiaDB::new().unwrap();

        // Node A: at positive pole
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        let polaris = Polaris::new(&db);
        let axis = Axis::new(vec![1.0, 0.0], vec![-1.0, 0.0]);

        let score = polaris.analyze_node(a, "vec", &axis).unwrap().unwrap();
        assert!((score - 1.0).abs() < 1e-6);
    }
}
