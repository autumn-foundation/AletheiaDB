#![allow(clippy::collapsible_if)]
//! Paradox: Contradiction Detection Engine.
//!
//! "How can two contradictory facts exist in the same database?"
//!
//! The Paradox engine scans the temporal graph for semantic contradictions over time
//! or space. It identifies nodes that have sharply contrasting vectors at different points
//! in time, or strongly connected nodes that have completely opposed meanings.
//!
//! # Concepts
//! - **Temporal Contradiction**: A single entity's semantic meaning has inverted over time.
//! - **Structural Contradiction**: Two entities are strongly connected but semantically opposed.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::paradox::ParadoxDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let paradox = ParadoxDetector::new(&db);
//!
//! let contradictions = paradox.detect_structural_contradictions("embedding", 0.0)?;
//! println!("Found {} contradictions", contradictions.len());
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;

/// A detected structural contradiction.
#[derive(Debug, Clone, PartialEq)]
pub struct Contradiction {
    /// The source node.
    pub source: NodeId,
    /// The target node connected to the source.
    pub target: NodeId,
    /// The negative similarity score (cosine similarity < 0).
    pub similarity: f32,
}

/// The Paradox Engine.
pub struct ParadoxDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> ParadoxDetector<'a> {
    /// Create a new Paradox Detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect structural contradictions: connected nodes with highly dissimilar vectors.
    ///
    /// # Arguments
    /// * `property_name` - The vector property to compare.
    /// * `threshold` - The maximum similarity score to be considered a contradiction.
    ///   Typically <= 0.0 (orthogonal or opposed).
    pub fn detect_structural_contradictions(
        &self,
        property_name: &str,
        threshold: f32,
    ) -> Result<Vec<Contradiction>> {
        let mut contradictions = Vec::new();

        self.db.read(|tx| {
            // Scan all nodes
            let results = self.db.query().scan(None).execute(self.db)?;

            for row in results {
                let row = row?;
                if let Some(source_node) = row.entity.as_node() {
                    let source_id = source_node.id;

                    if let Some(source_prop) = source_node.get_property(property_name) {
                        if let Some(source_vec) = source_prop.as_vector() {
                            // Check outgoing edges
                            let outgoing = tx.get_outgoing_edges(source_id);
                            for edge_id in outgoing {
                                if let Ok(edge) = tx.get_edge(edge_id) {
                                    let target_id = edge.target;

                                    if let Ok(target_node) = tx.get_node(target_id) {
                                        if let Some(target_prop) =
                                            target_node.get_property(property_name)
                                        {
                                            if let Some(target_vec) = target_prop.as_vector() {
                                                let sim =
                                                    ops::cosine_similarity(source_vec, target_vec)?;

                                                if sim <= threshold {
                                                    contradictions.push(Contradiction {
                                                        source: source_id,
                                                        target: target_id,
                                                        similarity: sim,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok::<(), Error>(())
        })?;

        Ok(contradictions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_paradox_structural_contradiction() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();
        let mut n3 = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Concept A
            n1 = tx
                .create_node(
                    "Idea",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Concept B (Opposite of A)
            n2 = tx
                .create_node(
                    "Idea",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[-1.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Concept C (Orthogonal to A)
            n3 = tx
                .create_node(
                    "Idea",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            // Connect A -> B (Contradiction: sim = -1)
            tx.create_edge(n1, n2, "RELATED_TO", Default::default())
                .unwrap();

            // Connect A -> C (Orthogonal: sim = 0)
            tx.create_edge(n1, n3, "RELATED_TO", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = ParadoxDetector::new(&db);

        // Find strong contradictions (sim < -0.5)
        let strong = detector
            .detect_structural_contradictions("vec", -0.5)
            .unwrap();
        assert_eq!(strong.len(), 1);
        assert_eq!(strong[0].source, n1);
        assert_eq!(strong[0].target, n2);

        // Find mild contradictions/orthogonality (sim <= 0.0)
        let mild = detector
            .detect_structural_contradictions("vec", 0.0)
            .unwrap();
        assert_eq!(mild.len(), 2);
    }
}
