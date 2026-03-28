//! Mirage: Semantic Illusion Detector.
//!
//! "Is this a hallucination, or a genuine discovery?"
//!
//! Mirage detects nodes that *sound* highly similar to an existing cluster
//! (high vector similarity) but are structurally disjoint (low neighborhood overlap).
//! This is particularly useful for LLM-driven applications to detect hallucinated
//! facts that mimic the semantic style of true data but lack actual structural grounding.
//!
//! # Use Cases
//! - **Hallucination Detection**: Flagging LLM outputs that look right but connect to nothing.
//! - **Deception Detection**: Identifying malicious nodes trying to blend into a community.
//! - **Silo Discovery**: Finding valid but structurally isolated discoveries.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::mirage::MirageDetector;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let node_id = NodeId::new(1).unwrap();
//!
//! let detector = MirageDetector::new(&db);
//! // Check if node_id is a mirage compared to the rest of the graph
//! // let results = detector.detect(node_id, "embedding", 0.8)?;
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use std::collections::HashSet;

/// Result of a Mirage detection analysis.
#[derive(Debug, Clone)]
pub struct MirageResult {
    /// The node being compared against.
    pub target_node: NodeId,
    /// Semantic similarity (Cosine) - How much they "sound" alike.
    pub semantic_similarity: f32,
    /// Structural similarity (Jaccard) - How much their neighborhoods overlap.
    pub structural_similarity: f32,
    /// The final mirage score: semantic_similarity * (1.0 - structural_similarity).
    /// High score (e.g., > 0.8) indicates a strong mirage (looks identical, shares no neighbors).
    pub mirage_score: f32,
}

/// The Mirage Engine.
pub struct MirageDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> MirageDetector<'a> {
    /// Create a new Mirage Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if the source node is a "mirage" relative to other nodes.
    ///
    /// It finds nodes that are semantically similar (>= min_semantic_similarity)
    /// but structurally disjoint.
    ///
    /// # Arguments
    /// * `source_node` - The node to investigate (e.g., a newly added LLM fact).
    /// * `property_name` - The vector property used for semantic comparison.
    /// * `min_semantic_similarity` - Only consider targets that sound similar (e.g., 0.7).
    pub fn detect(
        &self,
        source_node: NodeId,
        property_name: &str,
        min_semantic_similarity: f32,
    ) -> Result<Vec<MirageResult>> {
        let mut results = Vec::new();

        // 1. Get source vector and neighbors
        let source_vec = self.get_vec(source_node, property_name)?;
        let source_neighbors = self.get_all_neighbors_db(source_node)?;

        // 2. Scan for semantically similar nodes using the vector index
        let similar_nodes = self.db.search_vectors_in(property_name, &source_vec, 50)?;

        for (target_node, sim) in similar_nodes {
            if target_node == source_node || sim < min_semantic_similarity {
                continue;
            }

            // Calculate structural overlap
            let target_neighbors = self.get_all_neighbors_db(target_node)?;

            let structural_similarity =
                Self::jaccard_similarity(&source_neighbors, &target_neighbors);

            // High semantic, low structural -> High mirage
            let mirage_score = sim * (1.0 - structural_similarity);

            results.push(MirageResult {
                target_node,
                semantic_similarity: sim,
                structural_similarity,
                mirage_score,
            });
        }

        results.sort_by(|a, b| b.mirage_score.partial_cmp(&a.mirage_score).unwrap());

        Ok(results)
    }

    fn get_vec(&self, node_id: NodeId, prop: &str) -> Result<Vec<f32>> {
        let node = self.db.get_node(node_id)?;
        if let Some(v) = node.get_property(prop).and_then(|p| p.as_vector()) {
            Ok(v.to_vec())
        } else {
            Err(Error::other("Vector not found"))
        }
    }

    fn get_all_neighbors_db(&self, node_id: NodeId) -> Result<HashSet<NodeId>> {
        let mut neighbors = HashSet::new();
        for edge_id in self.db.get_outgoing_edges(node_id) {
            if let Ok(edge) = self.db.get_edge(edge_id) {
                neighbors.insert(edge.target);
            }
        }
        for edge_id in self.db.get_incoming_edges(node_id) {
            if let Ok(edge) = self.db.get_edge(edge_id) {
                neighbors.insert(edge.source);
            }
        }
        Ok(neighbors)
    }

    fn jaccard_similarity(set1: &HashSet<NodeId>, set2: &HashSet<NodeId>) -> f32 {
        if set1.is_empty() && set2.is_empty() {
            return 1.0; // Both have no neighbors, structurally identical (isolated)
        }
        let intersection = set1.intersection(set2).count();
        let union = set1.union(set2).count();
        // union == 0 is impossible here because if both are empty, we return 1.0 above.
        intersection as f32 / union as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_mirage_detection() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("embedding", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Let's create a core valid cluster
        let n1 = db
            .create_node(
                "Valid",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
        let n2 = db
            .create_node(
                "Valid",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9, 0.1])
                    .build(),
            )
            .unwrap();
        let n3 = db
            .create_node(
                "Valid",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.8, 0.2])
                    .build(),
            )
            .unwrap();

        // Connect them heavily
        db.create_edge(n1, n2, "KNOWS", Default::default()).unwrap();
        db.create_edge(n2, n3, "KNOWS", Default::default()).unwrap();
        db.create_edge(n1, n3, "KNOWS", Default::default()).unwrap();

        // Create an "Illusion" / Mirage node
        // Semantically identical to n1, but disconnected structurally
        let mirage = db
            .create_node(
                "Mirage",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();

        let detector = MirageDetector::new(&db);
        let results = detector.detect(mirage, "embedding", 0.8).unwrap();

        assert!(!results.is_empty());

        // The mirage should have a very high mirage score against n1
        let mirage_vs_n1 = results.iter().find(|r| r.target_node == n1).unwrap();

        // They sound the same, so semantic similarity is high (~1.0)
        assert!(mirage_vs_n1.semantic_similarity > 0.9);

        // But structural similarity is 0.0 (Mirage has 0 neighbors, n1 has 2)
        assert_eq!(mirage_vs_n1.structural_similarity, 0.0);

        // So mirage score should be high
        assert!(mirage_vs_n1.mirage_score > 0.9);
    }
}
