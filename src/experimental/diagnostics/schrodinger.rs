//! Schrodinger: Semantic Polysemy & Fracture Detector 🐈
//!
//! "Is this node dead or alive? Or both?"
//!
//! The Schrodinger Engine detects nodes that act as "bridges" between two entirely orthogonal
//! semantic spaces. For instance, the node "Apple" might be connected to "Fruit" nodes and "Tech" nodes.
//! To the database, it's one node. Semantically, it exists in two completely unrelated states.
//!
//! # Concepts
//! - **Fracture**: A node's neighborhood is split into multiple distinct clusters that share
//!   almost no semantic similarity with each other.
//! - **Polysemy Score**: A high score indicates the node likely represents multiple distinct meanings
//!   crammed into a single entity (a data modeling anomaly).
//!
//! # Use Cases
//! - **Entity Resolution Audits**: Did we accidentally merge two different "John Smiths"?
//! - **Jargon Detection**: Is this word being used in two conflicting contexts across the graph?

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use std::collections::HashSet;

/// Result of a Schrodinger analysis
#[derive(Debug, Clone, PartialEq)]
pub struct SchrodingerResult {
    /// How "split" the node's semantic neighborhood is (0.0 to 1.0)
    pub polysemy_score: f32,
    /// Identifies the node under observation
    pub node_id: NodeId,
    /// Number of distinct semantic clusters found
    pub distinct_clusters: usize,
}

/// The Schrodinger Engine for detecting semantic polysemy.
pub struct SchrodingerDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> SchrodingerDetector<'a> {
    /// Create a new SchrodingerDetector instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if a node is fractured semantically.
    /// `property_name`: The vector property to analyze.
    /// `orthogonality_threshold`: Maximum cosine similarity to consider two clusters distinct (e.g., 0.2).
    pub fn measure_polysemy(
        &self,
        node_id: NodeId,
        property_name: &str,
        orthogonality_threshold: f32,
    ) -> Result<SchrodingerResult> {
        let mut neighbors = HashSet::new();
        for edge in self.db.get_outgoing_edges(node_id) {
            if let Ok(target) = self.db.get_edge_target(edge) {
                neighbors.insert(target);
            }
        }
        for edge in self.db.get_incoming_edges(node_id) {
            if let Ok(source) = self.db.get_edge_source(edge) {
                neighbors.insert(source);
            }
        }

        let mut vectors: Vec<Vec<f32>> = Vec::new();
        for neighbor in neighbors {
            if let Ok(node) = self.db.get_node(neighbor) {
                if let Some(prop) = node.get_property(property_name) {
                    if let Some(vec) = prop.as_vector() {
                        vectors.push(vec.to_vec());
                    }
                }
            }
        }

        if vectors.is_empty() {
            return Ok(SchrodingerResult {
                polysemy_score: 0.0,
                node_id,
                distinct_clusters: 0,
            });
        }

        // Extremely naive clustering: Count how many vectors are mutually orthogonal
        let mut clusters: Vec<Vec<f32>> = Vec::new();

        for vec in vectors {
            let mut belongs_to_cluster = false;
            for cluster in &clusters {
                if let Ok(sim) = crate::core::vector::ops::cosine_similarity(&vec, cluster) {
                    if sim > orthogonality_threshold {
                        belongs_to_cluster = true;
                        break;
                    }
                }
            }
            if !belongs_to_cluster {
                clusters.push(vec);
            }
        }

        let distinct_clusters = clusters.len();
        // A single cluster means it's cohesive. Two or more means it's polysemic.
        // We cap the score at 1.0 using a simple asymptotic curve based on clusters.
        let polysemy_score = if distinct_clusters <= 1 {
            0.0
        } else {
            1.0 - (1.0 / (distinct_clusters as f32))
        };

        Ok(SchrodingerResult {
            polysemy_score,
            node_id,
            distinct_clusters,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_schrodinger_polysemy() {
        let db = AletheiaDB::new().unwrap();

        // Let's create an "Apple" node connected to "Fruit" and "Tech" concepts
        let mut apple = NodeId::new(0).unwrap();

        db.write(|tx| {
            apple = tx
                .create_node("Concept", PropertyMapBuilder::new().build())
                .unwrap();

            // "Fruit" side
            let pear = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();
            let banana = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.9, 0.1, 0.0])
                        .build(),
                )
                .unwrap();

            // "Tech" side
            let computer = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0, 0.0])
                        .build(),
                )
                .unwrap();
            let phone = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.9, 0.1])
                        .build(),
                )
                .unwrap();

            // "Financial" side
            let bank = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0, 1.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(apple, pear, "IS_LIKE", Default::default())
                .unwrap();
            tx.create_edge(apple, banana, "IS_LIKE", Default::default())
                .unwrap();
            tx.create_edge(apple, computer, "IS_LIKE", Default::default())
                .unwrap();
            tx.create_edge(apple, phone, "IS_LIKE", Default::default())
                .unwrap();
            tx.create_edge(apple, bank, "IS_LIKE", Default::default())
                .unwrap();

            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let detector = SchrodingerDetector::new(&db);

        // With threshold 0.5, we expect 3 distinct clusters (Fruit, Tech, Financial)
        let result = detector.measure_polysemy(apple, "vec", 0.5).unwrap();

        assert_eq!(result.distinct_clusters, 3);
        assert!(result.polysemy_score > 0.6); // 1 - 1/3 = 0.666...
    }
}
