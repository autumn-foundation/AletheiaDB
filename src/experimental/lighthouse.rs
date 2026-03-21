//! Lighthouse: Hidden Influencer / Beacon Detector.
//!
//! "We have vector similarity and graph structure, but they might disagree.
//! Can we find 'Lighthouses'—nodes that are semantically central to a concept
//! but structurally isolated from it?"
//!
//! Lighthouse scores nodes based on high semantic centrality (many nearby nodes
//! in vector space) combined with low structural degree.

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;

/// The result of a Lighthouse analysis.
#[derive(Debug, Clone)]
pub struct LighthouseScore {
    /// The node that was analyzed.
    pub node_id: NodeId,
    /// The calculated lighthouse score. Higher is better.
    pub score: f32,
    /// Number of semantic neighbors found within the limit.
    pub semantic_neighbors: usize,
    /// Number of structural edges (incoming + outgoing).
    pub structural_degree: usize,
}

impl LighthouseScore {
    /// Heuristic to determine if this is a significant lighthouse.
    pub fn is_lighthouse(&self) -> bool {
        self.score > 2.0 && self.semantic_neighbors > 1
    }
}

/// The Lighthouse Detector.
pub struct LighthouseDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> LighthouseDetector<'a> {
    /// Create a new LighthouseDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze a node to see if it acts as a "Lighthouse".
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property` - The vector property to use for semantic similarity.
    /// * `k` - The maximum number of semantic neighbors to retrieve.
    /// * `distance_threshold` - Optional threshold; only semantic neighbors closer than this are counted.
    ///   If None, all `k` results are counted if they exist.
    pub fn analyze_node(
        &self,
        node_id: NodeId,
        property: &str,
        k: usize,
        distance_threshold: Option<f32>,
    ) -> Result<LighthouseScore> {
        // 1. Get the Node's Vector
        let vector = self.db.read(|tx| {
            let node = tx.get_node(node_id)?;
            let prop = node.properties.get(property).ok_or_else(|| {
                Error::other(format!("Property '{}' not found on node", property))
            })?;

            let vec = prop
                .as_vector()
                .ok_or_else(|| Error::other(format!("Property '{}' is not a vector", property)))?;

            Ok::<Vec<f32>, Error>(vec.to_vec())
        })?;

        // 2. Find Semantic Neighbors
        let search_results = self.db.search_vectors_in(property, &vector, k + 1)?;

        // Filter out the node itself, and apply distance threshold
        let mut semantic_neighbors = 0;
        for (neighbor_id, dist) in search_results {
            if neighbor_id != node_id {
                if let Some(thresh) = distance_threshold {
                    if dist <= thresh {
                        semantic_neighbors += 1;
                    }
                } else {
                    semantic_neighbors += 1;
                }
            }
        }

        // 3. Count Structural Edges
        let outgoing = self.db.get_outgoing_edges(node_id).len();
        let incoming = self.db.get_incoming_edges(node_id).len();
        let structural_degree = outgoing + incoming;

        // 4. Calculate Score
        // Score = Semantic Centrality / Structural Density
        // Add 1 to denominator to avoid division by zero and dampen the effect of 0 edges.
        let score = semantic_neighbors as f32 / (structural_degree as f32 + 1.0);

        Ok(LighthouseScore {
            node_id,
            score,
            semantic_neighbors,
            structural_degree,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_lighthouse_high_score() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        // Create the Lighthouse node (Isolated structurally, but near others)
        let lighthouse = db
            .create_node(
                "Beacon",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Create semantic neighbors that are NOT connected to the lighthouse
        for i in 1..=5 {
            db.create_node(
                "Ship",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.01 * i as f32, 0.01 * i as f32])
                    .build(),
            )
            .unwrap();
        }

        let detector = LighthouseDetector::new(&db);
        let score = detector
            .analyze_node(lighthouse, "vec", 5, Some(0.1))
            .unwrap();

        assert_eq!(score.structural_degree, 0);
        assert_eq!(score.semantic_neighbors, 5);
        assert_eq!(score.score, 5.0); // 5 / (0 + 1)
        assert!(score.is_lighthouse());
    }

    #[test]
    fn test_lighthouse_low_score_connected() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        let center = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Create semantic neighbors that ARE connected (Not a hidden influencer)
        for i in 1..=5 {
            let neighbor = db
                .create_node(
                    "Spoke",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.01 * i as f32, 0.01 * i as f32])
                        .build(),
                )
                .unwrap();

            db.create_edge(center, neighbor, "CONNECTED_TO", Default::default())
                .unwrap();
        }

        let detector = LighthouseDetector::new(&db);
        let score = detector.analyze_node(center, "vec", 5, Some(0.1)).unwrap();

        assert_eq!(score.structural_degree, 5);
        assert_eq!(score.semantic_neighbors, 5);
        // Score = 5 / (5 + 1) = 0.833
        assert!(score.score < 1.0);
        assert!(!score.is_lighthouse());
    }

    #[test]
    fn test_lighthouse_no_semantic_neighbors() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        let hermit = db
            .create_node(
                "Hermit",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[100.0, 100.0])
                    .build(),
            )
            .unwrap();

        // Create nodes far away
        for i in 1..=5 {
            db.create_node(
                "Distant",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, i as f32])
                    .build(),
            )
            .unwrap();
        }

        let detector = LighthouseDetector::new(&db);
        let score = detector.analyze_node(hermit, "vec", 5, Some(10.0)).unwrap();

        assert_eq!(score.structural_degree, 0);
        assert_eq!(
            score.semantic_neighbors, 0,
            "Found {} semantic neighbors",
            score.semantic_neighbors
        );
        assert_eq!(score.score, 0.0);
        assert!(!score.is_lighthouse());
    }
}
