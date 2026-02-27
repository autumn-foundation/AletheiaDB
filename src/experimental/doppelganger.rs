//! Doppelganger: Structural Twins with Semantic Opposition.
//!
//! "Same friends, opposite beliefs."
//!
//! The Doppelganger Detector identifies pairs of nodes that are structurally very similar
//! (they share many of the same neighbors) but semantically distant or opposed.
//!
//! # Use Cases
//! - **Polarization Analysis**: Finding nodes in a social network that are in the same community but hold opposite views.
//! - **Fraud Detection**: Identifying accounts that behave the same way (structural twins) but claim completely different identities.
//! - **Anomaly Detection**: Flagging nodes whose semantics do not match their structural role.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::doppelganger::DoppelgangerDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let detector = DoppelgangerDetector::new(&db);
//!
//! # let target_id = aletheiadb::core::id::NodeId::new(1).unwrap();
//! // Find nodes that share at least 50% of neighbors but have < 0.0 cosine similarity
//! let doppelgangers = detector.find_doppelgangers(target_id, "embedding", 0.5, 0.0)?;
//!
//! for d in doppelgangers {
//!     println!("Found doppelganger: {}", d.node_id);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::cosine_similarity;
use std::collections::HashSet;

/// A detected doppelganger node.
#[derive(Debug, Clone, PartialEq)]
pub struct DoppelgangerMatch {
    /// The node ID of the doppelganger.
    pub node_id: NodeId,
    /// Structural similarity score (Jaccard index of neighbors, 0.0 to 1.0).
    pub structural_similarity: f32,
    /// Semantic similarity score (e.g., Cosine similarity, -1.0 to 1.0).
    pub semantic_similarity: f32,
}

/// Detector for finding structural twins with semantic opposition.
pub struct DoppelgangerDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DoppelgangerDetector<'a> {
    /// Create a new DoppelgangerDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find doppelgangers for a target node.
    ///
    /// # Arguments
    /// * `target` - The node to analyze.
    /// * `property` - The name of the vector property.
    /// * `min_structural_sim` - Minimum Jaccard similarity of neighbors (0.0 to 1.0).
    /// * `max_semantic_sim` - Maximum allowed semantic similarity (e.g., 0.0 for orthogonal/opposite).
    pub fn find_doppelgangers(
        &self,
        target: NodeId,
        property: &str,
        min_structural_sim: f32,
        max_semantic_sim: f32,
    ) -> Result<Vec<DoppelgangerMatch>> {
        // 1. Get target vector
        let target_node = self.db.get_node(target)?;
        let target_vec = match target_node
            .properties
            .get(property)
            .and_then(|v| v.as_vector())
        {
            Some(v) => v,
            None => return Ok(Vec::new()), // Cannot compute without vector
        };

        // 2. Get target neighbors
        let target_neighbors = self.get_all_neighbors(target)?;
        if target_neighbors.is_empty() {
            return Ok(Vec::new()); // Isolated nodes have no structural context
        }

        // 3. Find candidates (nodes that share at least one neighbor)
        // Optimization: For a large graph, we shouldn't scan all nodes.
        // We look at the "neighbors of neighbors" (2-hop neighborhood) to find structurally similar nodes.
        let mut candidates = HashSet::new();
        for &neighbor in &target_neighbors {
            let neighbor_neighbors = self.get_all_neighbors(neighbor)?;
            for candidate in neighbor_neighbors {
                if candidate != target {
                    candidates.insert(candidate);
                }
            }
        }

        let mut results = Vec::new();

        for candidate in candidates {
            // Check structural similarity (Jaccard index)
            let candidate_neighbors = self.get_all_neighbors(candidate)?;
            if candidate_neighbors.is_empty() {
                continue;
            }

            let intersection = target_neighbors.intersection(&candidate_neighbors).count() as f32;
            let union = target_neighbors.union(&candidate_neighbors).count() as f32;

            let structural_sim = if union > 0.0 {
                intersection / union
            } else {
                0.0
            };

            if structural_sim < min_structural_sim {
                continue;
            }

            // Check semantic similarity
            #[allow(clippy::collapsible_if)]
            if let Ok(candidate_node) = self.db.get_node(candidate) {
                if let Some(candidate_vec) = candidate_node
                    .properties
                    .get(property)
                    .and_then(|v| v.as_vector())
                {
                    if let Ok(semantic_sim) = cosine_similarity(target_vec, candidate_vec) {
                        if semantic_sim <= max_semantic_sim {
                            results.push(DoppelgangerMatch {
                                node_id: candidate,
                                structural_similarity: structural_sim,
                                semantic_similarity: semantic_sim,
                            });
                        }
                    }
                }
            }
        }

        // Sort results by structural similarity (descending), then semantic similarity (ascending)
        results.sort_by(|a, b| {
            b.structural_similarity
                .partial_cmp(&a.structural_similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.semantic_similarity
                        .partial_cmp(&b.semantic_similarity)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        Ok(results)
    }

    /// Helper to get all unique neighbors (incoming and outgoing) for a node.
    fn get_all_neighbors(&self, node: NodeId) -> Result<HashSet<NodeId>> {
        let mut neighbors = HashSet::new();

        let outgoing = self.db.get_outgoing_edges(node);
        for edge_id in outgoing {
            if let Ok(edge) = self.db.get_edge(edge_id) {
                neighbors.insert(edge.target);
            }
        }

        let incoming = self.db.get_incoming_edges(node);
        for edge_id in incoming {
            if let Ok(edge) = self.db.get_edge(edge_id) {
                neighbors.insert(edge.source);
            }
        }

        Ok(neighbors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_find_doppelgangers_success() {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index to test real-world like usage, though it's not strictly necessary since
        // find_doppelgangers uses the node properties directly.
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();

        // Target: [1.0, 0.0]
        let p_target = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let target = db.create_node("User", p_target).unwrap();

        // Doppelganger: Same structure, Opposite semantics [-1.0, 0.0] -> Cosine -1.0
        let p_doppel = PropertyMapBuilder::new()
            .insert_vector("embedding", &[-1.0, 0.0])
            .build();
        let doppel = db.create_node("User", p_doppel).unwrap();

        // Ally: Same structure, Similar semantics [0.9, 0.1] -> Cosine > 0
        let p_ally = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.9, 0.1])
            .build();
        let ally = db.create_node("User", p_ally).unwrap();

        // Create common neighbors
        for _ in 0..5 {
            let neighbor = db
                .create_node("Post", PropertyMapBuilder::new().build())
                .unwrap();
            db.create_edge(target, neighbor, "LIKES", PropertyMapBuilder::new().build())
                .unwrap();
            db.create_edge(doppel, neighbor, "LIKES", PropertyMapBuilder::new().build())
                .unwrap();
            db.create_edge(ally, neighbor, "LIKES", PropertyMapBuilder::new().build())
                .unwrap();
        }

        let detector = DoppelgangerDetector::new(&db);

        // Find nodes with at least 80% shared neighbors and cosine <= 0.0
        let results = detector
            .find_doppelgangers(target, "embedding", 0.8, 0.0)
            .unwrap();

        assert_eq!(results.len(), 1, "Should only find one doppelganger");
        assert_eq!(results[0].node_id, doppel);
        assert!(results[0].structural_similarity > 0.9);
        assert!(results[0].semantic_similarity < -0.9);

        // Verify ally is NOT included because semantic sim is too high
        let ally_included = results.iter().any(|r| r.node_id == ally);
        assert!(
            !ally_included,
            "Ally should not be identified as a doppelganger"
        );
    }

    #[test]
    fn test_find_doppelgangers_structural_mismatch() {
        let db = AletheiaDB::new().unwrap();

        // Target: [1.0, 0.0]
        let p_target = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let target = db.create_node("User", p_target).unwrap();

        // Mismatched: Opposite semantics, but NO shared neighbors
        let p_mismatch = PropertyMapBuilder::new()
            .insert_vector("vec", &[-1.0, 0.0])
            .build();
        let mismatch = db.create_node("User", p_mismatch).unwrap();

        // Target neighbors
        for _ in 0..3 {
            let n = db
                .create_node("Item", PropertyMapBuilder::new().build())
                .unwrap();
            db.create_edge(target, n, "LIKES", PropertyMapBuilder::new().build())
                .unwrap();
        }

        // Mismatch neighbors
        for _ in 0..3 {
            let n = db
                .create_node("Item", PropertyMapBuilder::new().build())
                .unwrap();
            db.create_edge(mismatch, n, "LIKES", PropertyMapBuilder::new().build())
                .unwrap();
        }

        let detector = DoppelgangerDetector::new(&db);
        let results = detector
            .find_doppelgangers(target, "vec", 0.5, 0.0)
            .unwrap();

        assert!(
            results.is_empty(),
            "Should not find any doppelgangers because structural sim is 0"
        );
    }
}
