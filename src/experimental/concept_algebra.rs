//! Concept Algebra: Vector Arithmetic for Nodes.
//!
//! This module allows performing semantic arithmetic on nodes.
//! It treats nodes as their underlying vector embeddings and allows operations like:
//! - Addition: `Concept(A) + Concept(B)` (Composition)
//! - Subtraction: `Concept(A) - Concept(B)` (Removal of meaning)
//! - Analogy: `Concept(A) - Concept(B) + Concept(C)` (e.g. "King" - "Man" + "Woman" = "Queen")
//! - Mean: Centroid of multiple concepts.
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use gallifreydb::GallifreyDB;
//! use gallifreydb::experimental::concept_algebra::ConceptAlgebra;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = GallifreyDB::new()?;
//! // ... (assume db is populated with nodes and vectors) ...
//! let king = 1.into();
//! let man = 2.into();
//! let woman = 3.into();
//!
//! let algebra = ConceptAlgebra::new(&db);
//!
//! // Perform analogy: King - Man + Woman = ?
//! let results = algebra.analogy(king, man, woman, 5)?;
//!
//! for (node_id, score) in results {
//!     println!("Found node {} with score {}", node_id, score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::GallifreyDB;
use crate::core::id::NodeId;
use crate::utils::{Error, Result, VectorError};

/// A tool for performing vector arithmetic on graph nodes.
pub struct ConceptAlgebra<'a> {
    db: &'a GallifreyDB,
    /// The property name to use for vectors. If None, auto-detected.
    property_name: Option<String>,
}

impl<'a> ConceptAlgebra<'a> {
    /// Create a new ConceptAlgebra instance.
    pub fn new(db: &'a GallifreyDB) -> Self {
        Self {
            db,
            property_name: None,
        }
    }

    /// Set the property name to use for vector operations.
    pub fn with_property(mut self, name: impl Into<String>) -> Self {
        self.property_name = Some(name.into());
        self
    }

    /// Resolve the property name to use.
    fn get_property_name(&self) -> Result<String> {
        if let Some(ref name) = self.property_name {
            return Ok(name.clone());
        }

        let indexes = self.db.list_vector_indexes();
        if let Some(idx_info) = indexes.first() {
            Ok(idx_info.property_name.clone())
        } else {
            Err(Error::Vector(VectorError::IndexError(
                "No vector indexes configured. Specify a property name or enable an index."
                    .to_string(),
            )))
        }
    }

    /// Retrieve the vector for a node.
    fn get_vector(&self, node_id: NodeId, property: &str) -> Result<Vec<f32>> {
        let node = self.db.get_node(node_id)?;

        // We need to access the property value.
        // Node properties are in node.properties (PropertyMap).

        let val = node.properties.get(property).ok_or_else(|| {
            Error::Vector(VectorError::IndexError(format!(
                "Node {} does not have vector property '{}'",
                node_id, property
            )))
        })?;

        if let Some(vec) = val.as_vector() {
            Ok(vec.to_vec())
        } else {
            Err(Error::Vector(VectorError::IndexError(format!(
                "Property '{}' on node {} is not a vector",
                property, node_id
            ))))
        }
    }

    /// Perform vector addition: `A + B`.
    ///
    /// This operation combines the semantic meaning of two nodes.
    /// Returns the `k` nearest neighbors to the resulting vector.
    ///
    /// # Errors
    ///
    /// Returns an error if the nodes do not have vectors or if dimensions mismatch.
    pub fn add(&self, a: NodeId, b: NodeId, k: usize) -> Result<Vec<(NodeId, f32)>> {
        let prop = self.get_property_name()?;
        let vec_a = self.get_vector(a, &prop)?;
        let vec_b = self.get_vector(b, &prop)?;

        if vec_a.len() != vec_b.len() {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: vec_a.len(),
                actual: vec_b.len(),
            }));
        }

        let sum: Vec<f32> = vec_a.iter().zip(vec_b.iter()).map(|(x, y)| x + y).collect();
        // Normalization is handled by search_vectors_in if using Cosine, usually.
        // But for semantic arithmetic, the direction matters most.

        self.db.search_vectors_in(&prop, &sum, k)
    }

    /// Perform vector subtraction: `A - B`.
    ///
    /// This operation removes the semantic meaning of node B from node A.
    /// Returns the `k` nearest neighbors to the resulting vector.
    ///
    /// # Errors
    ///
    /// Returns an error if the nodes do not have vectors or if dimensions mismatch.
    pub fn subtract(&self, a: NodeId, b: NodeId, k: usize) -> Result<Vec<(NodeId, f32)>> {
        let prop = self.get_property_name()?;
        let vec_a = self.get_vector(a, &prop)?;
        let vec_b = self.get_vector(b, &prop)?;

        if vec_a.len() != vec_b.len() {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: vec_a.len(),
                actual: vec_b.len(),
            }));
        }

        let diff: Vec<f32> = vec_a.iter().zip(vec_b.iter()).map(|(x, y)| x - y).collect();
        self.db.search_vectors_in(&prop, &diff, k)
    }

    /// Perform vector analogy: `A - B + C`.
    ///
    /// Solves the analogy: "A is to B as X is to C", where X is the result.
    /// Example: "King" - "Man" + "Woman" = "Queen".
    ///
    /// Returns the `k` nearest neighbors to the resulting vector.
    ///
    /// # Errors
    ///
    /// Returns an error if the nodes do not have vectors or if dimensions mismatch.
    pub fn analogy(&self, a: NodeId, b: NodeId, c: NodeId, k: usize) -> Result<Vec<(NodeId, f32)>> {
        let prop = self.get_property_name()?;
        let vec_a = self.get_vector(a, &prop)?;
        let vec_b = self.get_vector(b, &prop)?;
        let vec_c = self.get_vector(c, &prop)?;

        if vec_a.len() != vec_b.len() || vec_a.len() != vec_c.len() {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: vec_a.len(),
                actual: if vec_a.len() != vec_b.len() {
                    vec_b.len()
                } else {
                    vec_c.len()
                },
            }));
        }

        let result: Vec<f32> = vec_a
            .iter()
            .zip(vec_b.iter())
            .zip(vec_c.iter())
            .map(|((x, y), z)| x - y + z)
            .collect();

        self.db.search_vectors_in(&prop, &result, k)
    }

    /// Calculate the mean (centroid) of a set of nodes.
    ///
    /// Finds the "center of mass" for the given nodes in vector space.
    /// Returns the `k` nearest neighbors to the centroid.
    ///
    /// # Errors
    ///
    /// Returns an error if the nodes do not have vectors or if dimensions mismatch.
    pub fn mean(&self, nodes: &[NodeId], k: usize) -> Result<Vec<(NodeId, f32)>> {
        if nodes.is_empty() {
            return Ok(Vec::new());
        }

        let prop = self.get_property_name()?;
        let mut sum_vec: Option<Vec<f32>> = None;
        let count = nodes.len() as f32;

        for &node_id in nodes {
            let vec = self.get_vector(node_id, &prop)?;

            if let Some(ref mut sum) = sum_vec {
                if sum.len() != vec.len() {
                    return Err(Error::Vector(VectorError::DimensionMismatch {
                        expected: sum.len(),
                        actual: vec.len(),
                    }));
                }
                for (s, v) in sum.iter_mut().zip(vec.iter()) {
                    *s += v;
                }
            } else {
                sum_vec = Some(vec);
            }
        }

        let centroid: Vec<f32> = sum_vec.unwrap().into_iter().map(|x| x / count).collect();
        self.db.search_vectors_in(&prop, &centroid, k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    // Helper to create a test DB with vector index enabled
    fn setup_db() -> GallifreyDB {
        let db = GallifreyDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();
        db
    }

    #[test]
    fn test_concept_addition() {
        let db = setup_db();

        // Node A: [1.0, 0.0] (Right)
        let props_a = PropertyMapBuilder::new()
            .insert("name", "A")
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        // Node B: [0.0, 1.0] (Up)
        let props_b = PropertyMapBuilder::new()
            .insert("name", "B")
            .insert_vector("embedding", &[0.0, 1.0])
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        // Node C: [1.0, 1.0] (Diagonal) - The expected result of A + B
        let props_c = PropertyMapBuilder::new()
            .insert("name", "C")
            .insert_vector("embedding", &[1.0, 1.0])
            .build();
        let c = db.create_node("Node", props_c).unwrap();

        let algebra = ConceptAlgebra::new(&db).with_property("embedding");

        // A + B should be closest to C
        let results = algebra.add(a, b, 1).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].0, c);
    }

    #[test]
    fn test_concept_analogy() {
        // King - Man + Woman = Queen
        // Let's use simple 2D vectors for intuition
        // Man = [1, 0]
        // Woman = [2, 0]
        // King = [1, 1]
        // Queen = [2, 1] (Expected: King - Man + Woman = [1,1] - [1,0] + [2,0] = [2,1])

        let db = setup_db();

        let man = db
            .create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
        let woman = db
            .create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[2.0, 0.0])
                    .build(),
            )
            .unwrap();
        let king = db
            .create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 1.0])
                    .build(),
            )
            .unwrap();
        let queen = db
            .create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[2.0, 1.0])
                    .build(),
            )
            .unwrap();

        let algebra = ConceptAlgebra::new(&db); // Should auto-detect "embedding" since it's the only index

        let results = algebra.analogy(king, man, woman, 1).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].0, queen);
    }

    #[test]
    fn test_concept_mean() {
        let db = setup_db();

        // Use vectors with distinct directions to avoid Cosine ambiguity with magnitude changes
        // A = [1, 0]
        // B = [0, 1]
        // Mean = [0.5, 0.5] -> Direction [0.707, 0.707]
        // Target C = [1, 1] -> Direction [0.707, 0.707]

        let n1 = db
            .create_node(
                "P",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
        let n2 = db
            .create_node(
                "P",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
        let n3 = db
            .create_node(
                "P",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 1.0])
                    .build(),
            )
            .unwrap();

        // Target node (n3) matches the direction of the mean of n1 and n2
        let target = n3;

        let algebra = ConceptAlgebra::new(&db);
        let results = algebra.mean(&[n1, n2], 1).unwrap();

        assert!(!results.is_empty());
        assert_eq!(results[0].0, target);
    }

    #[test]
    fn test_error_handling() {
        let db = GallifreyDB::new().unwrap();
        // No index enabled yet

        let algebra = ConceptAlgebra::new(&db);
        let n1 = db
            .create_node("N", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = db
            .create_node("N", PropertyMapBuilder::new().build())
            .unwrap();

        // Should fail auto-detection
        let res = algebra.add(n1, n2, 1);
        assert!(res.is_err());

        // Enable index
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();

        // Create nodes without vectors
        let n3 = db
            .create_node("N", PropertyMapBuilder::new().build())
            .unwrap();

        // Should fail getting vector
        let res = algebra.add(n3, n3, 1);
        assert!(res.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = res {
            assert!(msg.contains("does not have vector property"));
        } else {
            panic!("Expected IndexError");
        }
    }
}
