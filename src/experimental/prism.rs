//! Prism: Semantic Spectroscopy for Vectors.
//!
//! "Unbox the Black Box."
//!
//! The Prism decomposes a vector embedding into named semantic components (facets).
//! It allows you to understand *why* a vector is positioned where it is by projecting
//! it onto a set of user-defined axes (concepts).
//!
//! # Use Cases
//! - **Explainability**: Why did the search retrieve this? "It's 80% 'Sci-Fi' and 20% 'Romance'."
//! - **Debugging**: Detect if a vector has drifted into an unwanted semantic region (e.g., "NSFW").
//! - **Filtering**: "Show me nodes that are high in 'Innovation' but low in 'Risk'."
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::prism::Prism;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let mut prism = Prism::new(&db);
//!
//! // Define the "lens"
//! prism.add_axis_from_node("Tech", tech_node_id)?;
//! prism.add_axis_from_node("Magic", magic_node_id)?;
//!
//! // Analyze a target
//! let spectrum = prism.analyze_node(target_node_id)?;
//!
//! println!("Tech: {:.2}", spectrum.get("Tech").unwrap_or(&0.0));
//! println!("Magic: {:.2}", spectrum.get("Magic").unwrap_or(&0.0));
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::id::NodeId;
use crate::core::vector::ops::{dot_product, magnitude, normalize};
use crate::utils::{Error, Result, VectorError};
use std::collections::HashMap;

/// A named axis in the semantic space.
#[derive(Debug, Clone)]
struct Axis {
    name: String,
    vector: Vec<f32>,
}

/// The Prism engine for vector decomposition.
pub struct Prism<'a> {
    db: &'a AletheiaDB,
    axes: Vec<Axis>,
}

impl<'a> Prism<'a> {
    /// Create a new Prism.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            db,
            axes: Vec::new(),
        }
    }

    /// Add an axis defined by a raw vector.
    /// The vector will be normalized to unit length.
    pub fn add_axis(&mut self, name: &str, vector: Vec<f32>) {
        // We normalize axes to ensure consistent projection values (cosine similarity-like)
        let normalized = normalize(&vector);
        self.axes.push(Axis {
            name: name.to_string(),
            vector: normalized,
        });
    }

    /// Add an axis defined by a node's vector.
    /// Uses the first available vector property of the node.
    pub fn add_axis_from_node(&mut self, name: &str, node_id: NodeId) -> Result<()> {
        let node = self.db.get_node(node_id)?;

        // Find first vector property
        let vector = node
            .properties
            .iter()
            .find_map(|(_, val)| val.as_vector())
            .ok_or_else(|| {
                Error::Vector(VectorError::IndexError(format!(
                    "Node {} has no vector properties to use as an axis",
                    node_id
                )))
            })?;

        self.add_axis(name, vector.to_vec());
        Ok(())
    }

    /// Orthogonalize the current axes using the Gram-Schmidt process.
    ///
    /// This ensures that the axes are mutually perpendicular, which makes
    /// the decomposition strictly additive (Component A + Component B = Target).
    ///
    /// Priority is given to axes added earlier (they keep their direction).
    /// Later axes have the components of earlier axes removed from them.
    pub fn orthogonalize(&mut self) {
        let mut ortho_axes: Vec<Axis> = Vec::with_capacity(self.axes.len());

        for axis in &self.axes {
            let mut v = axis.vector.clone();

            // Subtract projections onto existing orthogonal axes
            for u_axis in &ortho_axes {
                let u = &u_axis.vector;
                // proj_u(v) = (v . u) / (u . u) * u
                // Since u is normalized, u . u = 1.0
                // So proj_u(v) = (v . u) * u
                let dot = dot_product(&v, u).unwrap_or(0.0);

                // v = v - (dot * u)
                for (vi, ui) in v.iter_mut().zip(u.iter()) {
                    *vi -= dot * ui;
                }
            }

            // If the remaining vector is significant, add it
            if magnitude(&v) > 1e-6 {
                let normalized = normalize(&v);
                ortho_axes.push(Axis {
                    name: axis.name.clone(),
                    vector: normalized,
                });
            } else {
                // The axis was redundant (linear combination of previous axes)
                // We keep it but it will be effectively zero or we drop it?
                // Let's drop it to maintain a clean basis, but this might confuse users who added it.
                // Better to keep it? If it's zero, we can't normalize.
                // Let's skip it.
            }
        }

        self.axes = ortho_axes;
    }

    /// Analyze a raw vector against the axes.
    ///
    /// Returns a map of `Axis Name -> Score`.
    /// The score is the scalar projection of the target onto the axis.
    /// If axes are normalized, this is the dot product.
    pub fn analyze(&self, target: &[f32]) -> Result<HashMap<String, f32>> {
        let mut spectrum = HashMap::new();

        for axis in &self.axes {
            if axis.vector.len() != target.len() {
                // Skip mismatching dimensions or return error?
                // Returning error is safer.
                return Err(Error::Vector(VectorError::DimensionMismatch {
                    expected: axis.vector.len(),
                    actual: target.len(),
                }));
            }

            let score = dot_product(target, &axis.vector)?;
            spectrum.insert(axis.name.clone(), score);
        }

        Ok(spectrum)
    }

    /// Analyze a node's vector.
    /// Uses the first available vector property.
    pub fn analyze_node(&self, node_id: NodeId) -> Result<HashMap<String, f32>> {
        let node = self.db.get_node(node_id)?;

        // Find first vector property
        let vector = node
            .properties
            .iter()
            .find_map(|(_, val)| val.as_vector())
            .ok_or_else(|| {
                Error::Vector(VectorError::IndexError(format!(
                    "Node {} has no vector properties to analyze",
                    node_id
                )))
            })?;

        self.analyze(vector)
    }

    /// Calculate the "Residual" energy.
    ///
    /// Returns the magnitude of the part of the vector *not* explained by the current axes.
    /// `Residual = || Vector - Sum(Projection_i) ||`
    ///
    /// NOTE: This is only mathematically rigorous if the axes are orthogonal.
    pub fn residual(&self, target: &[f32]) -> Result<f32> {
        // Reconstruct the vector from components
        let mut reconstruction = vec![0.0; target.len()];

        for axis in &self.axes {
            let score = dot_product(target, &axis.vector)?;

            // Add (score * axis_vector) to reconstruction
            for (r, a) in reconstruction.iter_mut().zip(axis.vector.iter()) {
                *r += score * a;
            }
        }

        // Calculate difference
        let diff: Vec<f32> = target
            .iter()
            .zip(reconstruction.iter())
            .map(|(t, r)| t - r)
            .collect();

        Ok(magnitude(&diff))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_prism_analysis() {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index to ensure vector logic works (though Prism works on raw props too)
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();

        let mut prism = Prism::new(&db);

        // Axis 1: X-axis (e.g., "Logic")
        prism.add_axis("Logic", vec![1.0, 0.0]);
        // Axis 2: Y-axis (e.g., "Emotion")
        prism.add_axis("Emotion", vec![0.0, 1.0]);

        // Target: [1.0, 1.0] (Logic + Emotion)
        let target = vec![1.0, 1.0];
        let spectrum = prism.analyze(&target).unwrap();

        assert!((spectrum.get("Logic").unwrap() - 1.0).abs() < 1e-5);
        assert!((spectrum.get("Emotion").unwrap() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_prism_orthogonalization() {
        let db = AletheiaDB::new().unwrap();
        let mut prism = Prism::new(&db);

        // Axis 1: [1.0, 0.0]
        prism.add_axis("X", vec![1.0, 0.0]);
        // Axis 2: [1.0, 1.0] (Not orthogonal to X)
        prism.add_axis("Diag", vec![1.0, 1.0]);

        // Orthogonalize
        prism.orthogonalize();

        // Check axes
        // X should remain [1.0, 0.0]
        let x_axis = prism.axes.iter().find(|a| a.name == "X").unwrap();
        assert!((x_axis.vector[0] - 1.0).abs() < 1e-5);
        assert!((x_axis.vector[1] - 0.0).abs() < 1e-5);

        // Diag should become [0.0, 1.0] (Y-axis)
        // Original Diag was [0.707, 0.707] (normalized).
        // Wait, add_axis normalizes input!
        // Input: [1.0, 1.0] -> Normalized: [0.707, 0.707]
        // Proj_X(Diag) = (Diag . X) * X = 0.707 * [1, 0] = [0.707, 0.0]
        // Remainder = [0.707, 0.707] - [0.707, 0.0] = [0.0, 0.707]
        // Normalized Remainder = [0.0, 1.0]
        let diag_axis = prism.axes.iter().find(|a| a.name == "Diag").unwrap();
        assert!((diag_axis.vector[0] - 0.0).abs() < 1e-5);
        assert!((diag_axis.vector[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_prism_residual() {
        let db = AletheiaDB::new().unwrap();
        let mut prism = Prism::new(&db);

        // Basis: Just X axis
        prism.add_axis("X", vec![1.0, 0.0]);

        // Target: [1.0, 1.0]
        // Explained: [1.0, 0.0]
        // Unexplained: [0.0, 1.0]
        // Residual Magnitude: 1.0

        let res = prism.residual(&[1.0, 1.0]).unwrap();
        assert!((res - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_prism_from_node() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let node = db.create_node("Basis", props).unwrap();

        let mut prism = Prism::new(&db);
        prism.add_axis_from_node("BasisNode", node).unwrap();

        let spectrum = prism.analyze(&[1.0, 0.0]).unwrap();
        assert!((spectrum.get("BasisNode").unwrap() - 1.0).abs() < 1e-5);
    }
}
