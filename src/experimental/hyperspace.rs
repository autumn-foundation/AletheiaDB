//! Hyperspace: Multi-Dimensional Semantic Subspace Projection.
//!
//! "What if we only look at the 'color' dimensions of this concept?"
//!
//! The Hyperspace engine allows projecting high-dimensional vectors (e.g., 1536-d from OpenAI)
//! into targeted, lower-dimensional subspaces using predefined dimension masks or dynamic
//! PCA-like approximations. It enables reasoning about entities from specific semantic "angles."
//!
//! # Concept
//!
//! An embedding for "Apple" contains information about its shape, color, technology company, etc.
//! By masking or projecting the vector, we can compare "Apple" and "Ferrari" strictly on
//! their "color" subspace (both are red) rather than their total semantic meaning.
//!
//! # Use Cases
//! - **Faceted Similarity**: Comparing objects only by specific traits (e.g., sentiment, color, size).
//! - **Concept Disentanglement**: Isolating the "technology" vs. "fruit" aspects of a word.
//! - **Explainable AI**: Understanding which dimensions contribute most to a similarity score.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::hyperspace::{Hyperspace, SubspaceMask};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let apple_id = db.create_node("Concept", Default::default())?;
//! # let ferrari_id = db.create_node("Concept", Default::default())?;
//!
//! let hyperspace = Hyperspace::new(&db);
//!
//! // Define a subspace mask focusing only on indices 10-20 (hypothetical "color" dimensions)
//! let color_mask = SubspaceMask::Range(10..20);
//!
//! // Compare Apple and Ferrari only in the "color" subspace
//! let color_similarity = hyperspace.subspace_similarity(
//!     apple_id,
//!     ferrari_id,
//!     "embedding",
//!     &color_mask
//! )?;
//!
//! println!("Color similarity: {:.2}", color_similarity);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::ops::Range;

/// Defines a subspace mask to isolate specific dimensions of a vector.
#[derive(Debug, Clone)]
pub enum SubspaceMask {
    /// A contiguous range of dimensions.
    Range(Range<usize>),
    /// A list of specific indices.
    Indices(Vec<usize>),
}

impl SubspaceMask {
    /// Applies the mask to a vector, extracting the targeted dimensions.
    pub fn apply(&self, vector: &[f32]) -> Result<Vec<f32>> {
        match self {
            SubspaceMask::Range(range) => {
                if range.end > vector.len() {
                    return Err(Error::other("Mask range exceeds vector dimensions"));
                }
                Ok(vector[range.clone()].to_vec())
            }
            SubspaceMask::Indices(indices) => {
                let mut result = Vec::with_capacity(indices.len());
                for &idx in indices {
                    if idx >= vector.len() {
                        return Err(Error::other("Mask index exceeds vector dimensions"));
                    }
                    result.push(vector[idx]);
                }
                Ok(result)
            }
        }
    }
}

/// The Hyperspace Engine runner.
pub struct Hyperspace<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Hyperspace<'a> {
    /// Create a new Hyperspace engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculates the cosine similarity between two nodes within a specific subspace.
    pub fn subspace_similarity(
        &self,
        node_a: NodeId,
        node_b: NodeId,
        property_name: &str,
        mask: &SubspaceMask,
    ) -> Result<f32> {
        let vec_a = self.get_vector(node_a, property_name)?;
        let vec_b = self.get_vector(node_b, property_name)?;

        let sub_a = mask.apply(&vec_a)?;
        let sub_b = mask.apply(&vec_b)?;

        // If the subspace vectors are empty or all zeros, similarity is undefined (return 0.0)
        if sub_a.is_empty() || sub_b.is_empty() {
            return Ok(0.0);
        }

        ops::cosine_similarity(&sub_a, &sub_b)
    }

    /// Projects a node's vector into the specified subspace.
    pub fn project(
        &self,
        node_id: NodeId,
        property_name: &str,
        mask: &SubspaceMask,
    ) -> Result<Vec<f32>> {
        let vec = self.get_vector(node_id, property_name)?;
        mask.apply(&vec)
    }

    /// Helper to fetch a vector property for a given node.
    fn get_vector(&self, node_id: NodeId, property_name: &str) -> Result<Vec<f32>> {
        let node = self.db.get_node(node_id)?;
        let prop = node.get_property(property_name).ok_or_else(|| {
            Error::other(format!(
                "Node {} missing property '{}'",
                node_id, property_name
            ))
        })?;

        let vec = prop
            .as_vector()
            .ok_or_else(|| Error::other(format!("Property '{}' is not a vector", property_name)))?;

        Ok(vec.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_subspace_mask_range() {
        let vec = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let mask = SubspaceMask::Range(1..4);
        let result = mask.apply(&vec).unwrap();
        assert_eq!(result, vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_subspace_mask_indices() {
        let vec = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let mask = SubspaceMask::Indices(vec![0, 2, 4]);
        let result = mask.apply(&vec).unwrap();
        assert_eq!(result, vec![1.0, 3.0, 5.0]);
    }

    #[test]
    fn test_subspace_mask_out_of_bounds() {
        let vec = vec![1.0, 2.0];
        let mask_range = SubspaceMask::Range(0..3);
        assert!(mask_range.apply(&vec).is_err());

        let mask_indices = SubspaceMask::Indices(vec![0, 5]);
        assert!(mask_indices.apply(&vec).is_err());
    }

    #[test]
    fn test_subspace_similarity() {
        let db = AletheiaDB::new().unwrap();

        // Node A: Red Apple
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0, 0.5, 0.5]) // [Fruit, Tech, Red, Round]
            .build();
        let node_a = db.create_node("Concept", props_a).unwrap();

        // Node B: Red Ferrari
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0, 0.5, 0.0]) // [Fruit, Tech, Red, Round]
            .build();
        let node_b = db.create_node("Concept", props_b).unwrap();

        let hyperspace = Hyperspace::new(&db);

        // Overall similarity should be low (Fruit vs Tech)
        let total_mask = SubspaceMask::Range(0..4);
        let total_sim = hyperspace
            .subspace_similarity(node_a, node_b, "vec", &total_mask)
            .unwrap();

        // Color subspace similarity (index 2) should be high (both are Red)
        let color_mask = SubspaceMask::Indices(vec![2]);
        let color_sim = hyperspace
            .subspace_similarity(node_a, node_b, "vec", &color_mask)
            .unwrap();

        assert!(color_sim > total_sim);
        // Both have 0.5 in the color dimension, cosine sim of [0.5] and [0.5] is 1.0
        assert!((color_sim - 1.0).abs() < 0.001);
    }
}
