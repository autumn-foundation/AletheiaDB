#![allow(clippy::collapsible_if)]

//! Nemesis Engine: Detecting Structural Connection and Semantic Opposition.
//!
//! "Keep your friends close, and your enemies closer."
//!
//! The Nemesis Engine identifies entities that are structurally connected
//! (e.g., have an edge between them) but are semantically opposed
//! (e.g., have negative cosine similarity between their vectors).
//!
//! # Concepts
//! - **Nemesis Pair**: Two nodes connected by an edge, where the angle between
//!   their vector representations approaches 180 degrees (cosine similarity < 0).
//! - **Nemesis Score**: Measures the degree of opposition. A higher score
//!   indicates stronger semantic divergence despite structural proximity.
//!
//! # Use Cases
//! - **Debate Detection**: Identifying individuals who interact frequently
//!   but hold completely opposite ideological views.
//! - **Anomaly Detection**: Finding integrated components that behave or
//!   represent fundamentally incompatible concepts.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::nemesis::NemesisDetector;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph with vectors and edges ...
//! # let subject_node = NodeId::new(0).unwrap();
//!
//! let detector = NemesisDetector::new(&db);
//! let nemeses = detector.find_nemeses(subject_node, "embedding", -0.5)?;
//!
//! if !nemeses.is_empty() {
//!     println!("Found {} nemeses!", nemeses.len());
//! }
//! # Ok(())
//! # }
//! ```

use crate::api::transaction::ReadOps;
use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;

/// Represents a detected nemesis relationship.
#[derive(Debug, Clone, PartialEq)]
pub struct NemesisResult {
    /// The node identified as a nemesis.
    pub nemesis_node: NodeId,
    /// The cosine similarity score (will be negative).
    pub similarity_score: f32,
}

/// The Nemesis Detector Engine.
pub struct NemesisDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> NemesisDetector<'a> {
    /// Create a new Nemesis Detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find nemeses for a given subject node.
    ///
    /// Evaluates all structural neighbors (incoming and outgoing edges).
    /// If a neighbor has a cosine similarity below the specified threshold,
    /// it is considered a nemesis.
    ///
    /// # Arguments
    /// * `subject_node` - The node whose neighbors will be evaluated.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `threshold` - The maximum cosine similarity score to qualify as a nemesis (e.g., -0.5).
    ///   Must be <= 0.0.
    ///
    /// # Returns
    /// A list of `NemesisResult`, sorted by severity (most negative similarity first).
    pub fn find_nemeses(
        &self,
        subject_node: NodeId,
        property_name: &str,
        threshold: f32,
    ) -> Result<Vec<NemesisResult>> {
        if threshold > 0.0 {
            return Err(Error::other("Nemesis threshold must be <= 0.0"));
        }

        let mut nemeses = Vec::new();

        self.db.read(|tx| {
            // 1. Get the subject's vector
            let subject_vec = match tx.get_node(subject_node) {
                Ok(node) => {
                    if let Some(prop) = node.get_property(property_name).and_then(|p| p.as_vector())
                    {
                        prop.to_vec()
                    } else {
                        return Ok(()); // Subject has no vector, cannot compare
                    }
                }
                Err(_) => return Ok(()), // Subject not found
            };

            let mut subject_normalized = subject_vec.clone();
            ops::normalize_in_place(&mut subject_normalized);

            // 2. Gather all unique neighbors
            let mut neighbors = std::collections::HashSet::new();

            for edge_id in tx.get_outgoing_edges(subject_node) {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    neighbors.insert(edge.target);
                }
            }

            for edge_id in tx.get_incoming_edges(subject_node) {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    neighbors.insert(edge.source);
                }
            }

            // Remove self-loops
            neighbors.remove(&subject_node);

            // 3. Evaluate each neighbor
            for neighbor_node in neighbors {
                if let Ok(node) = tx.get_node(neighbor_node) {
                    if let Some(prop) = node.get_property(property_name).and_then(|p| p.as_vector())
                    {
                        let mut neighbor_vec = prop.to_vec();
                        ops::normalize_in_place(&mut neighbor_vec);

                        // Safe to unwrap since lengths are known and matched by properties (or similarity returns error if diff size)
                        if let Ok(similarity) =
                            ops::cosine_similarity(&subject_normalized, &neighbor_vec)
                        {
                            if similarity <= threshold {
                                nemeses.push(NemesisResult {
                                    nemesis_node: neighbor_node,
                                    similarity_score: similarity,
                                });
                            }
                        }
                    }
                }
            }

            Ok::<(), Error>(())
        })?;

        // Sort by severity (most negative first)
        nemeses.sort_by(|a, b| {
            a.similarity_score
                .partial_cmp(&b.similarity_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(nemeses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_find_nemeses() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();
        let mut n3 = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Subject
            n1 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Nemesis (Opposite vector, connected)
            n2 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[-1.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Friend (Same vector, connected)
            n3 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(n1, n2, "KNOWS", Default::default()).unwrap();
            tx.create_edge(n1, n3, "KNOWS", Default::default()).unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = NemesisDetector::new(&db);

        // Find nemeses with threshold -0.5
        let nemeses = detector.find_nemeses(n1, "vec", -0.5).unwrap();

        assert_eq!(nemeses.len(), 1);
        assert_eq!(nemeses[0].nemesis_node, n2);
        assert!(nemeses[0].similarity_score < -0.9); // Should be roughly -1.0
    }

    #[test]
    fn test_nemesis_invalid_threshold() {
        let db = AletheiaDB::new().unwrap();
        let detector = NemesisDetector::new(&db);

        let n1 = NodeId::new(1).unwrap();

        // Threshold > 0 should error
        let result = detector.find_nemeses(n1, "vec", 0.5);
        assert!(result.is_err());
    }
}
