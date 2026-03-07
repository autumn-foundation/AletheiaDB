//! Nexus: Semantic Hub Detector.
//!
//! "Find the bridges between worlds."
//!
//! The Nexus engine identifies nodes that act as central hubs connecting highly distinct
//! semantic domains. A "Nexus" is a node with high structural connectivity (degree)
//! whose neighbors are highly diverse in vector space.
//!
//! # Concepts
//! - **Structural Degree**: The number of edges connected to a node.
//! - **Semantic Diversity**: How varied the vectors of a node's neighbors are.
//!   Calculated as the average pairwise distance between all neighbors.
//! - **Nexus Score**: A combined score of structural degree and semantic diversity.
//!
//! # Use Cases
//! - **Key Translators**: Identifying people or concepts that bridge different communities.
//! - **Core Concepts**: Finding unifying ideas that connect disparate topics.
//! - **Vulnerability Analysis**: Finding bottlenecks in information flow.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::nexus::NexusDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let detector = NexusDetector::new(&db);
//!
//! // Analyze a specific node
//! # let node_id = db.create_node("Node", Default::default())?;
//! let score = detector.analyze_node(node_id, "embedding")?;
//! println!("Nexus Score: {}", score.nexus_score);
//! # Ok(())
//! # }
//! ```

#![allow(clippy::collapsible_if)]
use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use std::collections::HashSet;

/// The result of a Nexus analysis for a single node.
#[derive(Debug, Clone)]
pub struct NexusScore {
    /// The node that was analyzed.
    pub node: NodeId,
    /// The number of neighbors considered.
    pub degree: usize,
    /// The semantic diversity of the neighbors (0.0 = identical, higher = more diverse).
    pub semantic_diversity: f32,
    /// The combined Nexus score (higher is better).
    pub nexus_score: f32,
}

/// The Nexus Engine for finding semantic hubs.
pub struct NexusDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> NexusDetector<'a> {
    /// Create a new NexusDetector instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze a single node to calculate its Nexus Score.
    ///
    /// The score is calculated by finding all neighbors of the node, extracting their
    /// vectors for the specified `property_name`, and computing the average pairwise
    /// distance between them.
    ///
    /// # Arguments
    /// * `node` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic distance.
    pub fn analyze_node(&self, node: NodeId, property_name: &str) -> Result<NexusScore> {
        let mut neighbor_vectors = Vec::new();

        self.db.read(|tx| {
            // Collect unique neighbors from both incoming and outgoing edges
            let mut neighbors = HashSet::new();

            let outgoing = tx.get_outgoing_edges(node);
            for edge_id in outgoing {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    neighbors.insert(edge.target);
                }
            }

            let incoming = tx.get_incoming_edges(node);
            for edge_id in incoming {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    neighbors.insert(edge.source);
                }
            }

            // Exclude self-loops
            neighbors.remove(&node);

            // Extract vectors for neighbors
            for &neighbor_id in &neighbors {
                if let Ok(neighbor_node) = tx.get_node(neighbor_id) {
                    if let Some(prop) = neighbor_node
                        .get_property(property_name)
                        .and_then(|p| p.as_vector())
                    {
                        neighbor_vectors.push(prop.to_vec());
                    }
                }
            }

            Ok::<(), Error>(())
        })?;

        let degree = neighbor_vectors.len();

        if degree < 2 {
            // Need at least 2 neighbors to calculate diversity
            return Ok(NexusScore {
                node,
                degree,
                semantic_diversity: 0.0,
                nexus_score: 0.0,
            });
        }

        // Calculate semantic diversity (average pairwise Euclidean distance)
        let mut total_distance = 0.0;
        let mut pairs = 0;

        for i in 0..degree {
            for j in (i + 1)..degree {
                let dist = self.euclidean_distance(&neighbor_vectors[i], &neighbor_vectors[j])?;
                total_distance += dist;
                pairs += 1;
            }
        }

        let semantic_diversity = if pairs > 0 {
            total_distance / pairs as f32
        } else {
            0.0
        };

        // Combine degree and diversity into a final score.
        // Simple formula: log(degree) * diversity
        // Log is used to dampen the effect of very high degree nodes, emphasizing
        // that a true Nexus needs both connections AND diversity.
        let nexus_score = (degree as f32).ln() * semantic_diversity;

        Ok(NexusScore {
            node,
            degree,
            semantic_diversity,
            nexus_score,
        })
    }

    /// Analyze a list of nodes and return their scores, sorted by descending score.
    pub fn analyze_nodes(&self, nodes: &[NodeId], property_name: &str) -> Result<Vec<NexusScore>> {
        let mut scores = Vec::with_capacity(nodes.len());
        for &node in nodes {
            match self.analyze_node(node, property_name) {
                Ok(score) => scores.push(score),
                Err(e) => {
                    // Log or handle error for individual node?
                    // For now, we fail the whole batch if one fails, or we could skip.
                    // Let's return the error as it might indicate a missing property.
                    return Err(e);
                }
            }
        }

        // Sort descending
        scores.sort_by(|a, b| b.nexus_score.partial_cmp(&a.nexus_score).unwrap());
        Ok(scores)
    }

    fn euclidean_distance(&self, a: &[f32], b: &[f32]) -> Result<f32> {
        if a.len() != b.len() {
            return Err(Error::other("Vector dimensions do not match"));
        }
        let dist_sq: f32 = a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum();
        Ok(dist_sq.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_nexus_score_isolated_node() {
        let db = AletheiaDB::new().unwrap();
        let mut node = NodeId::new(0).unwrap();

        db.write(|tx| {
            node = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = NexusDetector::new(&db);
        let score = detector.analyze_node(node, "vec").unwrap();

        assert_eq!(score.degree, 0);
        assert_eq!(score.semantic_diversity, 0.0);
        assert_eq!(score.nexus_score, 0.0);
    }

    #[test]
    fn test_nexus_score_homogeneous_neighbors() {
        let db = AletheiaDB::new().unwrap();
        let mut center = NodeId::new(0).unwrap();

        db.write(|tx| {
            center = tx
                .create_node("Center", PropertyMapBuilder::new().build())
                .unwrap();

            let n1 = tx
                .create_node(
                    "Neighbor",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            let n2 = tx
                .create_node(
                    "Neighbor",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0]) // Identical vector
                        .build(),
                )
                .unwrap();

            tx.create_edge(center, n1, "LINK", Default::default())
                .unwrap();
            tx.create_edge(center, n2, "LINK", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = NexusDetector::new(&db);
        let score = detector.analyze_node(center, "vec").unwrap();

        assert_eq!(score.degree, 2);
        assert_eq!(score.semantic_diversity, 0.0); // Identical neighbors
        assert_eq!(score.nexus_score, 0.0);
    }

    #[test]
    fn test_nexus_score_diverse_neighbors() {
        let db = AletheiaDB::new().unwrap();
        let mut center = NodeId::new(0).unwrap();

        db.write(|tx| {
            center = tx
                .create_node("Center", PropertyMapBuilder::new().build())
                .unwrap();

            let n1 = tx
                .create_node(
                    "Neighbor",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            let n2 = tx
                .create_node(
                    "Neighbor",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0]) // Orthogonal vector
                        .build(),
                )
                .unwrap();

            let n3 = tx
                .create_node(
                    "Neighbor",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[-1.0, 0.0]) // Opposite vector
                        .build(),
                )
                .unwrap();

            tx.create_edge(center, n1, "LINK", Default::default())
                .unwrap();
            tx.create_edge(center, n2, "LINK", Default::default())
                .unwrap();
            tx.create_edge(n3, center, "LINK", Default::default()) // Incoming edge
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = NexusDetector::new(&db);
        let score = detector.analyze_node(center, "vec").unwrap();

        assert_eq!(score.degree, 3);
        assert!(score.semantic_diversity > 1.0); // Neighbors are far apart
        assert!(score.nexus_score > 1.0);
    }
}
