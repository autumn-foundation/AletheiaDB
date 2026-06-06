#![allow(clippy::collapsible_if)]

//! Doppelgänger: Detecting Semantic Twins 👯
//!
//! "Who is trying to be me?"
//!
//! The Doppelgänger Detector identifies nodes that are highly semantically similar
//! (their vectors are close) but structurally distant (they share few or no neighbors).
//! This can reveal duplicate entities, divergent evolutions, or spoofing attempts.
//!
//! # Concepts
//! - **Semantic Similarity**: Nodes with vectors pointing in the same direction (e.g., Cosine > 0.9).
//! - **Structural Similarity**: The Jaccard index of the nodes' neighbors (e.g., < 0.1 means they share almost no context).
//!
//! # Use Cases
//! - **Fraud Detection**: Finding a new user account whose embedding matches an existing user perfectly, but connects to completely different networks to avoid detection.
//! - **Knowledge Graph Deduplication**: Flagging nodes that mean the same thing but haven't been merged structurally.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::doppelganger::{DoppelgangerDetector, DoppelgangerConfig};
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = NodeId::new(0).unwrap();
//!
//! let detector = DoppelgangerDetector::new(&db);
//! let config = DoppelgangerConfig::default();
//!
//! // let doppelgangers = detector.find_doppelgangers(node_id, &config, 10)?;
//! // for d in doppelgangers {
//! //     println!("Found doppelganger {} with semantic similarity {}", d.node_id, d.semantic_similarity);
//! // }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use std::collections::HashSet;

/// Configuration for the Doppelganger Detector.
#[derive(Debug, Clone)]
pub struct DoppelgangerConfig {
    /// Minimum vector similarity to be considered a doppelganger.
    pub min_semantic_similarity: f32,
    /// Maximum structural similarity (Jaccard) to be considered a doppelganger.
    pub max_structural_similarity: f32,
    /// Vector property to search on.
    pub vector_property: String,
}

impl Default for DoppelgangerConfig {
    fn default() -> Self {
        Self {
            min_semantic_similarity: 0.9,
            max_structural_similarity: 0.1,
            vector_property: "embedding".to_string(),
        }
    }
}

/// A detected doppelganger pair.
#[derive(Debug, Clone, PartialEq)]
pub struct DoppelgangerResult {
    /// The target node.
    pub node_id: NodeId,
    /// The semantic similarity (cosine).
    pub semantic_similarity: f32,
    /// The structural similarity (Jaccard).
    pub structural_similarity: f32,
}

/// Engine to find doppelgangers.
pub struct DoppelgangerDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DoppelgangerDetector<'a> {
    /// Creates a new DoppelgangerDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Finds doppelgangers for a given node.
    pub fn find_doppelgangers(
        &self,
        node_id: NodeId,
        config: &DoppelgangerConfig,
        k: usize,
    ) -> Result<Vec<DoppelgangerResult>> {
        let mut results = Vec::new();

        // 1. Find semantic neighbors
        let semantic_neighbors = self
            .db
            .find_similar_in(&config.vector_property, node_id, k)?;

        // 2. Get the base node's neighbor set
        let base_edges = self.db.get_outgoing_edges(node_id);
        let mut base_neighbors = HashSet::new();
        for edge_id in base_edges {
            if let Ok(target) = self.db.get_edge_target(edge_id) {
                base_neighbors.insert(target);
            }
        }

        let incoming_edges = self.db.get_incoming_edges(node_id);
        for edge_id in incoming_edges {
            if let Ok(source) = self.db.get_edge_source(edge_id) {
                base_neighbors.insert(source);
            }
        }

        for (target, similarity) in semantic_neighbors {
            if target == node_id {
                continue;
            }
            if similarity < config.min_semantic_similarity {
                continue;
            }

            // Get target neighbor set
            let mut target_neighbors = HashSet::new();
            for edge_id in self.db.get_outgoing_edges(target) {
                if let Ok(n) = self.db.get_edge_target(edge_id) {
                    target_neighbors.insert(n);
                }
            }
            for edge_id in self.db.get_incoming_edges(target) {
                if let Ok(n) = self.db.get_edge_source(edge_id) {
                    target_neighbors.insert(n);
                }
            }

            // Calculate Jaccard Similarity
            let intersection = base_neighbors.intersection(&target_neighbors).count();
            let union = base_neighbors.union(&target_neighbors).count();

            let structural_similarity = if union == 0 {
                0.0
            } else {
                intersection as f32 / union as f32
            };

            if structural_similarity <= config.max_structural_similarity {
                results.push(DoppelgangerResult {
                    node_id: target,
                    semantic_similarity: similarity,
                    structural_similarity,
                });
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::hnsw::HnswConfig;

    #[test]
    fn test_doppelganger() -> Result<()> {
        let db = AletheiaDB::new()?;
        let config_hnsw = HnswConfig::new(2, DistanceMetric::Cosine);
        db.vector_index("embedding").hnsw(config_hnsw).enable()?;

        let props1 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let node1 = db.create_node("Test", props1)?;

        let props2 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.99, 0.01])
            .build();
        let node2 = db.create_node("Test", props2)?;

        let props3 = PropertyMapBuilder::new().build();
        let common = db.create_node("Common", props3)?;

        // Connect both to common, giving them high structural similarity
        db.write(|tx| {
            tx.create_edge(node1, common, "KNOWS", Default::default())?;
            tx.create_edge(node2, common, "KNOWS", Default::default())?;
            Ok::<_, crate::core::error::Error>(())
        })?;

        let detector = DoppelgangerDetector::new(&db);
        let config = DoppelgangerConfig::default();

        // node1 and node2 have high semantic similarity, but also high structural similarity,
        // so they shouldn't be doppelgangers
        let results = detector.find_doppelgangers(node1, &config, 10)?;
        assert_eq!(results.len(), 0);

        // Add a node with high semantic similarity, but low structural similarity
        let props4 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.98, 0.02])
            .build();
        let node4 = db.create_node("Test", props4)?;

        let results2 = detector.find_doppelgangers(node1, &config, 10)?;
        assert_eq!(results2.len(), 1);
        assert_eq!(results2[0].node_id, node4);

        Ok(())
    }
}
